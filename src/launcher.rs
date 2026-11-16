use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::error::{VapourError, VapourResult};
use crate::library::home_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchOutcome {
    pub appid: u32,
    pub started_steam: bool,
}

/// Checks if Steam's IPC named pipe is actively open for reading by the daemon.
/// Uses POSIX O_NONBLOCK: open() fails with ENXIO if no reader is listening,
/// and succeeds immediately (< 1 µs) once the Steam daemon opens it for reading.
pub fn is_steam_pipe_open() -> bool {
    let Some(home) = home_dir() else { return false };
    let pipe_path = home.join(".steam/steam.pipe");
    let Ok(c_path) = std::ffi::CString::new(pipe_path.to_string_lossy().as_bytes()) else {
        return false;
    };

    // SAFETY: c_path is a live NUL-terminated CString; O_NONBLOCK makes open on the FIFO return immediately (invariant 5).
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
    if fd >= 0 {
        // SAFETY: fd was returned by open above (>= 0) and is closed exactly once.
        unsafe { libc::close(fd) };
        true
    } else {
        false
    }
}

/// Checks if Valve's IPC shared memory object (/dev/shm/u<UID>-ValveIPCSharedObj-Steam) exists,
/// indicating the Steam client core is fully initialized for client-game IPC.
pub fn is_valve_ipc_ready() -> bool {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    Path::new(&format!("/dev/shm/u{uid}-ValveIPCSharedObj-Steam")).exists()
}

/// Computes the 64-bit Long AppID for a Steam non-Steam shortcut: ((appid as u64) << 32) | 0x02000000.
pub fn calculate_long_appid(appid_32: u32) -> u64 {
    ((appid_32 as u64) << 32) | 0x02000000
}

fn comm_is_steam(comm: &str) -> bool {
    comm.trim() == "steam"
}

/// Guards against a stale steam.pid whose PID was recycled by another process.
fn pid_is_steam(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| comm_is_steam(&c))
}

/// Resolves the PID of the active Steam process, if running.
pub fn get_steam_pid() -> Option<u32> {
    // 1. Check steam.pid in standard directories
    if let Some(home) = home_dir() {
        for pid_path in [
            home.join(".steam/steam.pid"),
            home.join(".local/share/Steam/steam.pid"),
        ] {
            if let Ok(contents) = fs::read_to_string(&pid_path) {
                if let Ok(pid) = contents.trim().parse::<u32>() {
                    if pid_is_steam(pid) {
                        return Some(pid);
                    }
                }
            }
        }
    }

    // 2. Fallback: scan /proc for process with comm == "steam"
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            if let Some(name_str) = file_name.to_str() {
                if let Ok(pid) = name_str.parse::<u32>() {
                    let comm_path = entry.path().join("comm");
                    if let Ok(comm) = fs::read_to_string(&comm_path) {
                        if comm_is_steam(&comm) {
                            return Some(pid);
                        }
                    }
                }
            }
        }
    }

    None
}

/// Resolves the PIDs of any active steamwebhelper processes.
pub fn get_steamwebhelper_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            if let Some(name_str) = file_name.to_str() {
                if let Ok(pid) = name_str.parse::<u32>() {
                    let comm_path = entry.path().join("comm");
                    if let Ok(comm) = fs::read_to_string(&comm_path) {
                        if comm.trim() == "steamwebhelper" {
                            pids.push(pid);
                        }
                    }
                }
            }
        }
    }
    pids
}

/// Checks if Steam is currently running on the system.
pub fn is_steam_running() -> bool {
    get_steam_pid().is_some()
}

/// Resolves the steam executable binary.
pub fn resolve_steam_bin() -> PathBuf {
    PathBuf::from("steam")
}

/// Every Steam flag vapour can pass to the headless daemon, in command-line order.
pub const STEAM_DAEMON_FLAGS: &[&str] = &[
    "-silent",
    "-nocrashdialog",
    "-noverifyfiles",
    "-vrdisable",
    "-nofriendsui",
    "-cef-force-occlusion",
    "-cef-disable-site-isolation",
    "-nojoy",
    "-noshaders",
    "-cef-disable-hang-timeouts",
    "-cef-disable-breakpad",
    "-disable-overlay-gpu-pri",
    "-cef-disable-gpu",
];

/// Selects the flags for this launch: `-nojoy` on request, the GPU-priority flag only with the
/// overlay, and `-cef-disable-gpu` only without it.
fn daemon_flags(nojoy: bool, overlay: bool) -> Vec<String> {
    STEAM_DAEMON_FLAGS
        .iter()
        .filter(|f| match **f {
            "-nojoy" => nojoy,
            "-disable-overlay-gpu-pri" => overlay,
            "-cef-disable-gpu" => !overlay,
            _ => true,
        })
        .map(|f| f.to_string())
        .collect()
}

/// Ensures a background Steam instance is running with minimal RAM and suppressed bloat.
/// Returns Ok(true) if Steam was freshly spawned, or Ok(false) if Steam was already active.
pub fn ensure_steam_daemon(
    overlay_enabled: bool,
    nojoy: bool,
    env_vars: &[(String, String)],
    dry_run: bool,
) -> VapourResult<bool> {
    let was_running = is_steam_running();
    let steam_bin = resolve_steam_bin();

    if was_running {
        println!("\x1b[36m[INFO]\x1b[0m Steam is already running. Delegating game launch...");
        return Ok(false);
    }

    if !dry_run {
        let _ = crate::prune::autoclean_session_leftovers();
    }

    let flags = daemon_flags(nojoy, overlay_enabled);

    if dry_run {
        println!(
            "\x1b[33m[DRY-RUN]\x1b[0m Steam daemon spawn: {} {}",
            steam_bin.display(),
            flags.join(" ")
        );
        return Ok(true);
    }

    println!("\x1b[36m[INFO]\x1b[0m Steam is not running. Starting minimal background instance...");
    if overlay_enabled {
        println!("       (Store, Chat & bloat suppressed; In-game overlay active)");
    } else {
        println!("       (Store, Chat, CEF GPU & Overlay fully suppressed)");
    }

    let has_swap = fs::read_to_string("/proc/swaps")
        .map(|s| s.lines().count() > 1)
        .unwrap_or(false);
    if !has_swap {
        println!("\x1b[33m[WARN]\x1b[0m No swap or zRAM detected in /proc/swaps; cgroup memory offloading will be limited.");
    }

    let has_systemd_run = Command::new("systemd-run")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let mut daemon_cmd = if has_systemd_run {
        let unit = format!("vapour-steam-{}", std::process::id());
        let mut c = Command::new("systemd-run");
        c.arg("--user")
            .arg("--scope")
            // Delegate: vapour writes memory.reclaim in this cgroup, which systemd only sanctions
            // for delegated subtrees.
            .arg("-p")
            .arg("Delegate=yes")
            .arg(format!("--unit={}", unit))
            .arg(&steam_bin);
        c
    } else {
        Command::new(&steam_bin)
    };

    daemon_cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Instruct Steam's launch wrapper and pressure-vessel to place games into their own transient systemd scopes at birth.
    // This isolates game textures and file page cache from Steam's daemon cgroup completely.
    daemon_cmd
        .env("STEAM_LAUNCH_WRAPPER_SCOPE", "1")
        .env("PRESSURE_VESSEL_SYSTEMD_SCOPE", "1");

    if !overlay_enabled {
        daemon_cmd
            .env("PRESSURE_VESSEL_REMOVE_GAME_OVERLAY", "1")
            .env("SteamNoOverlayUIDrawing", "1")
            .env("DISABLE_VK_LAYER_VALVE_steam_overlay_1", "1");
    }

    for (k, v) in env_vars {
        daemon_cmd.env(k, v);
    }

    for flag in &flags {
        daemon_cmd.arg(flag);
    }

    daemon_cmd.spawn().map_err(|e| {
        VapourError::SteamDaemon(format!(
            "Failed to spawn Steam daemon ({:?}): {}",
            steam_bin, e
        ))
    })?;

    // Locate bootstrap_log.txt to monitor potential client self-updates
    let bootstrap_log = home_dir().and_then(|h| {
        [
            h.join(".local/share/Steam/logs/bootstrap_log.txt"),
            h.join(".steam/steam/logs/bootstrap_log.txt"),
        ]
        .into_iter()
        .find(|p| p.exists())
    });

    let mut bootstrap_offset = bootstrap_log
        .as_ref()
        .and_then(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);

    println!("       Waiting for Steam IPC socket to become ready...");
    let wait_start = Instant::now();
    let mut max_wait_secs = 10;
    let mut update_detected = false;

    while wait_start.elapsed() < Duration::from_secs(max_wait_secs) {
        // Check bootstrap_log.txt for client self-update activity
        if let Some(ref bpath) = bootstrap_log {
            if let Ok(mut f) = fs::File::open(bpath) {
                if let Ok(meta) = f.metadata() {
                    let len = meta.len();
                    if len > bootstrap_offset {
                        use std::io::{Read, Seek, SeekFrom};
                        if f.seek(SeekFrom::Start(bootstrap_offset)).is_ok() {
                            let mut buf = String::new();
                            if f.read_to_string(&mut buf).is_ok() {
                                bootstrap_offset = len;
                                for line in buf.lines() {
                                    let t = line.trim();
                                    if t.contains("Downloading update")
                                        || t.contains("Extracting package")
                                        || t.contains("Installing update")
                                        || t.contains("Verifying installation")
                                    {
                                        if !update_detected {
                                            println!("\x1b[33m[UPDATE]\x1b[0m Steam client self-update in progress...");
                                            update_detected = true;
                                            max_wait_secs = 120; // Extend timeout for update download & installation
                                        }
                                        println!("       {}", t);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if is_steam_pipe_open() && is_valve_ipc_ready() {
            // Brief 200ms grace period so Steam's main event loop is actively accepting IPC commands
            sleep(Duration::from_millis(200));
            break;
        }
        // Responsive 100ms polling interval
        sleep(Duration::from_millis(100));
    }

    Ok(true)
}

/// Launches a game by AppID, applying minimal CEF flags, overlay settings, and custom args.
pub fn launch_game(
    appid: u32,
    name: &str,
    config: &crate::config::ResolvedLaunchConfig,
    dry_run: bool,
) -> VapourResult<LaunchOutcome> {
    use crate::runner::{GameRunner, LaunchTarget, SteamDaemonRunner};
    let target = LaunchTarget::SteamApp {
        appid,
        name: name.to_string(),
    };
    let runner = SteamDaemonRunner;
    let res = runner.launch(&target, config, dry_run)?;
    Ok(LaunchOutcome {
        appid: res.appid,
        started_steam: res.started_steam,
    })
}

/// Launches a non-Steam game registered as a shortcut in Steam using its 64-bit Long AppID.
/// Long AppID formula: ((appid_32 as u64) << 32) | 0x02000000.
pub fn launch_steam_shortcut(
    appid_32: u32,
    name: &str,
    config: &crate::config::ResolvedLaunchConfig,
    dry_run: bool,
) -> VapourResult<LaunchOutcome> {
    use crate::runner::{GameRunner, LaunchTarget, SteamShortcutRunner};
    let target = LaunchTarget::SteamShortcut {
        appid: appid_32,
        name: name.to_string(),
        long_appid: calculate_long_appid(appid_32),
    };
    let runner = SteamShortcutRunner;
    let res = runner.launch(&target, config, dry_run)?;
    Ok(LaunchOutcome {
        appid: res.appid,
        started_steam: res.started_steam,
    })
}

/// Gracefully requests Steam to shut down, allowing it to sync cloud saves.
pub fn shutdown_steam() -> VapourResult<()> {
    let steam_bin = resolve_steam_bin();
    Command::new(&steam_bin)
        .arg("-shutdown")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            VapourError::SteamDaemon(format!("Failed to send shutdown to Steam: {}", e))
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn comm_is_steam_should_match_only_steam() {
        assert!(super::comm_is_steam("steam\n"));
        assert!(!super::comm_is_steam("firefox\n"));
    }
    use super::*;
    use crate::config::ResolvedLaunchConfig;

    #[test]
    fn test_calculate_long_appid() {
        // AppID 2165744007 -> (2165744007 << 32) | 0x02000000
        let long_id = calculate_long_appid(2165744007);
        assert_eq!(long_id, 9301799681606549504);
    }

    #[test]
    fn test_launch_game_dry_run_overlay_enabled() {
        let cfg = ResolvedLaunchConfig {
            overlay: true,
            keep_steam: false,
            nojoy: false,
            gamemode: false,
            mangohud: false,
            gamescope: false,
            gamescope_args: None,
            wrapper: None,
            env: Vec::new(),
            extra_args: vec!["-novid".to_string()],
            cli_args: Vec::new(),
            compat_tool: None,
            localconfig_options: None,
        };
        let outcome = launch_game(730, "Counter-Strike 2", &cfg, true);
        assert!(outcome.is_ok());
        let outcome = outcome.unwrap();
        assert_eq!(outcome.appid, 730);
    }

    #[test]
    fn test_launch_game_dry_run_overlay_disabled() {
        let cfg = ResolvedLaunchConfig {
            overlay: false,
            keep_steam: false,
            nojoy: false,
            gamemode: false,
            mangohud: false,
            gamescope: false,
            gamescope_args: None,
            wrapper: None,
            env: Vec::new(),
            extra_args: Vec::new(),
            cli_args: Vec::new(),
            compat_tool: None,
            localconfig_options: None,
        };
        let outcome = launch_game(730, "Counter-Strike 2", &cfg, true);
        assert!(outcome.is_ok());
        let outcome = outcome.unwrap();
        assert_eq!(outcome.appid, 730);
    }

    #[test]
    fn test_launch_shortcut_dry_run() {
        let cfg = ResolvedLaunchConfig {
            overlay: true,
            keep_steam: false,
            nojoy: false,
            gamemode: false,
            mangohud: false,
            gamescope: false,
            gamescope_args: None,
            wrapper: None,
            env: Vec::new(),
            extra_args: Vec::new(),
            cli_args: Vec::new(),
            compat_tool: None,
            localconfig_options: None,
        };
        let outcome = launch_steam_shortcut(2165744007, "Custom Game", &cfg, true);
        assert!(outcome.is_ok());
        let outcome = outcome.unwrap();
        assert_eq!(outcome.appid, 2165744007);
    }

    #[test]
    fn test_get_steamwebhelper_pids_does_not_panic() {
        let _ = get_steamwebhelper_pids();
    }
}
