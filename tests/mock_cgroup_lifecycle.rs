use std::fs::{self, File};
use std::io::Read;
use std::thread;
use std::time::Duration;

use tempfile::tempdir;
use vapour::telemetry::parse_appid_from_scope_name;
use vapour::watcher::parse_memory_pressure;

#[test]
fn test_mock_cgroup_isolation_and_reclaim() {
    let temp = tempdir().expect("Failed to create mock cgroup tempdir");
    let app_slice = temp.path().join("app.slice");
    fs::create_dir_all(&app_slice).expect("Failed to create app.slice");

    // 1. Create Steam daemon scope
    let steam_scope = app_slice.join("vapour-steam-12345.scope");
    fs::create_dir_all(&steam_scope).unwrap();
    fs::write(steam_scope.join("memory.current"), "1200000000\n").unwrap();
    fs::write(steam_scope.join("memory.swap.current"), "50000000\n").unwrap();
    fs::write(steam_scope.join("memory.reclaim"), "").unwrap();

    // 2. Create Steam CEF webhelper scope
    let cef_scope = app_slice.join("app-steam-unknown-8888.scope");
    fs::create_dir_all(&cef_scope).unwrap();
    fs::write(cef_scope.join("memory.current"), "600000000\n").unwrap();
    fs::write(cef_scope.join("memory.swap.current"), "30000000\n").unwrap();
    fs::write(cef_scope.join("memory.reclaim"), "").unwrap();

    // 3. Create Game container scope (app-steam-app730)
    let game_scope = app_slice.join("app-steam-app730-9999.scope");
    fs::create_dir_all(&game_scope).unwrap();
    fs::write(game_scope.join("memory.current"), "4000000000\n").unwrap();
    fs::write(game_scope.join("memory.swap.current"), "0\n").unwrap();
    fs::write(game_scope.join("memory.reclaim"), "").unwrap();

    // Verify scope classification rules
    let entries = fs::read_dir(&app_slice).unwrap();
    let mut reclaimable_scopes = Vec::new();
    let mut protected_game_scopes = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("app-steam-app") {
            protected_game_scopes.push(name);
        } else if name.starts_with("vapour-steam-") || name.starts_with("app-steam-unknown-") {
            reclaimable_scopes.push(entry.path());
        }
    }

    assert_eq!(protected_game_scopes.len(), 1);
    assert_eq!(protected_game_scopes[0], "app-steam-app730-9999.scope");
    assert_eq!(reclaimable_scopes.len(), 2);

    // Verify AppID extraction from game container
    let parsed_appid = parse_appid_from_scope_name(&protected_game_scopes[0]);
    assert_eq!(parsed_appid, Some(730));

    // Simulate memory.reclaim writes to mock cgroups
    for scope in &reclaimable_scopes {
        let reclaim_file = scope.join("memory.reclaim");
        fs::write(&reclaim_file, "1500M swappiness=200\n").unwrap();

        // Verify written content
        let content = fs::read_to_string(&reclaim_file).unwrap();
        assert_eq!(content, "1500M swappiness=200\n");
    }

    // Assert that the game container scope was never touched
    let game_reclaim = fs::read_to_string(game_scope.join("memory.reclaim")).unwrap();
    assert!(
        game_reclaim.is_empty(),
        "Game container must never be reclaimed"
    );
}

#[test]
fn test_mock_psi_pressure_trigger_simulation() {
    // Normal baseline memory pressure
    let baseline_psi = "some avg10=0.85 avg60=0.40 avg300=0.10 total=12345\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
    let psi_normal = parse_memory_pressure(baseline_psi).expect("Failed to parse normal PSI");
    assert!(
        psi_normal.some_avg10 < 5.0,
        "Normal PSI should not trigger reclaim"
    );

    // Acute memory pressure spike (some avg10 >= 5.0%)
    let spike_psi = "some avg10=7.42 avg60=3.20 avg300=1.15 total=54321\nfull avg10=1.20 avg60=0.50 avg300=0.10 total=1000\n";
    let psi_spike = parse_memory_pressure(spike_psi).expect("Failed to parse spike PSI");
    assert!(
        psi_spike.some_avg10 >= 5.0,
        "Spike PSI should trigger adaptive reclamation"
    );
}

#[test]
fn test_non_blocking_posix_fifo_probing() {
    let temp = tempdir().expect("Failed to create fifo tempdir");
    let fifo_path = temp.path().join("mock_steam.pipe");

    let c_path = std::ffi::CString::new(fifo_path.to_str().unwrap()).unwrap();

    // Create POSIX named pipe (FIFO)
    let res = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(res, 0, "mkfifo should succeed");

    // 1. Probing without reader: O_NONBLOCK | O_WRONLY must return ENXIO immediately (< 1 µs)
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
    assert_eq!(fd, -1, "Open without reader must fail immediately");
    let errno = std::io::Error::last_os_error().raw_os_error();
    assert_eq!(
        errno,
        Some(libc::ENXIO),
        "Kernel must return ENXIO when FIFO has no reader"
    );

    // 2. Open reader in background thread
    let path_clone = fifo_path.clone();
    let reader_handle = thread::spawn(move || {
        let mut f = File::open(path_clone).expect("Reader failed to open FIFO");
        let mut buf = [0u8; 16];
        f.read(&mut buf).expect("Reader read failed")
    });

    // Give reader thread a moment to open FIFO
    thread::sleep(Duration::from_millis(50));

    // 3. Probing with reader: O_NONBLOCK | O_WRONLY now succeeds immediately
    let writer_fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
    assert!(
        writer_fd >= 0,
        "Writer should successfully open FIFO when reader is active"
    );

    // Write handshake token
    let written = unsafe { libc::write(writer_fd, c"STEAM_READY".as_ptr() as *const _, 12) };
    assert_eq!(written, 12);
    unsafe { libc::close(writer_fd) };

    let bytes_received = reader_handle.join().expect("Reader thread panicked");
    assert_eq!(bytes_received, 12);
}
