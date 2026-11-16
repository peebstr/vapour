//! Shared game-lifecycle mechanism: process tracking, cgroup reclaim, PSI, cloud-sync wait.
//! The CLI watcher (`watcher.rs`) and the daemon (`daemon.rs`) are thin drivers over this module.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::launcher::{get_steam_pid, get_steamwebhelper_pids, is_steam_running, shutdown_steam};
use crate::telemetry::parse_appid_from_scope_name;
use crate::watcher::find_steam_log;

/// RAII wrapper for Linux `pidfd` file descriptor (Linux 5.3+).
/// Guarantees immunity against PID recycling races.
#[derive(Debug)]
pub struct PidFd {
    raw: libc::c_int,
    pid: u32,
}

impl PidFd {
    /// Sends `sig` to the process this pidfd refers to; falls back to `kill(2)` if the syscall fails.
    pub fn send_signal(&self, sig: libc::c_int) {
        // SAFETY: self.raw is a live pidfd owned by self; a null siginfo and flags 0 are valid.
        let r = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.raw,
                sig,
                std::ptr::null::<libc::c_void>(),
                0,
            )
        };
        if r != 0 {
            // SAFETY: kill only sends a signal; self.pid > 0 so it never targets a process group.
            unsafe {
                libc::kill(self.pid as libc::pid_t, sig);
            }
        }
    }

    /// Attempts to acquire a pidfd for the given PID using `libc::SYS_pidfd_open`.
    pub fn open(pid: u32) -> Option<Self> {
        // SAFETY: SYS_pidfd_open takes a pid and flags; failure is reported through the return value, which is checked.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if fd >= 0 {
            Some(PidFd {
                raw: fd as libc::c_int,
                pid,
            })
        } else {
            None
        }
    }

    /// Checks if the process represented by this pidfd is still alive.
    /// Uses non-blocking `poll(2)` with zero timeout.
    pub fn is_alive(&self) -> bool {
        let mut pfd = libc::pollfd {
            fd: self.raw,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pfd is a valid, exclusively borrowed pollfd (a pidfd) and nfds is 1.
        let res = unsafe { libc::poll(&mut pfd, 1, 0) };
        if res < 0 {
            // EINTR or any other failure is not evidence of death; report "still alive".
            true
        } else if res == 0 {
            // No exit event: process is still alive and running
            true
        } else {
            // POLLIN / POLLHUP / POLLERR: process has exited
            false
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for PidFd {
    fn drop(&mut self) {
        if self.raw >= 0 {
            // SAFETY: raw is the pidfd obtained in PidFd::open and is closed only here, in Drop.
            unsafe { libc::close(self.raw) };
        }
    }
}

/// Process handle abstraction supporting Linux PIDFD with /proc fallback.
#[derive(Debug)]
pub enum ProcessTracker {
    PidFd(PidFd),
    Proc(u32),
}

impl ProcessTracker {
    pub fn for_pid(pid: u32) -> Self {
        if let Some(pfd) = PidFd::open(pid) {
            ProcessTracker::PidFd(pfd)
        } else {
            ProcessTracker::Proc(pid)
        }
    }

    pub fn is_alive(&self) -> bool {
        match self {
            ProcessTracker::PidFd(pfd) => pfd.is_alive(),
            ProcessTracker::Proc(pid) => Path::new(&format!("/proc/{}", pid)).exists(),
        }
    }

    pub fn pid(&self) -> u32 {
        match self {
            ProcessTracker::PidFd(pfd) => pfd.pid(),
            ProcessTracker::Proc(pid) => *pid,
        }
    }
}

/// Checks if a line in cloud_log.txt marks the end of an upload for appid.
/// Steam logs `AutoCloud complete` and `already synced` *before* the exit upload starts,
/// so only the `Upload complete` lines count.
pub fn parse_cloud_sync_complete(line: &str, appid: u32) -> bool {
    let needle = format!("[AppID {appid}]");
    line.contains(&needle)
        && (line.contains("Upload complete, result OK")
            || line.contains("Upload complete in build list"))
}

/// Per-AppID progress of the exit cloud sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudState {
    Waiting,
    ExitSyncStarted,
    Done,
    Conflict,
}

/// Advances the cloud state for one log line. A line only counts if it moves the state forward.
pub fn cloud_step(state: CloudState, line: &str, appid: u32) -> CloudState {
    if matches!(state, CloudState::Done | CloudState::Conflict) {
        return state;
    }
    if parse_cloud_sync_conflict(line, appid) {
        return CloudState::Conflict;
    }
    if parse_cloud_sync_complete(line, appid) {
        return CloudState::Done;
    }
    if state == CloudState::Waiting
        && line.contains(&format!("[AppID {appid}] Starting sync (up,AC Exit"))
    {
        return CloudState::ExitSyncStarted;
    }
    state
}

/// Checks if a line in cloud_log.txt indicates a cloud sync conflict for appid.
pub fn parse_cloud_sync_conflict(line: &str, appid: u32) -> bool {
    let needle = format!("[AppID {appid}]");
    if line.contains(&needle) {
        let lower = line.to_lowercase();
        if lower.contains("conflict")
            || lower.contains("mismatch")
            || lower.contains("files differ")
        {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct MemoryPressure {
    pub some_avg10: f64,
    pub some_avg60: f64,
    pub some_avg300: f64,
    pub full_avg10: f64,
}

/// Reads Linux kernel Pressure Stall Information (PSI) for memory (/proc/pressure/memory) with stack buffer.
/// Returns None if PSI is not supported or disabled in the running kernel.
pub fn read_memory_pressure() -> Option<MemoryPressure> {
    use std::fs::File;
    use std::io::Read;

    let mut file = File::open("/proc/pressure/memory").ok()?;
    let mut buf = [0u8; 512];
    let n = file.read(&mut buf).ok()?;
    let content = std::str::from_utf8(&buf[..n]).ok()?;
    parse_memory_pressure(content)
}

/// Parses the textual format of /proc/pressure/memory.
pub fn parse_memory_pressure(content: &str) -> Option<MemoryPressure> {
    let mut some_avg10 = 0.0;
    let mut some_avg60 = 0.0;
    let mut some_avg300 = 0.0;
    let mut full_avg10 = 0.0;
    let mut found_some = false;

    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        if parts[0] == "some" {
            found_some = true;
            for token in &parts[1..] {
                if let Some(val) = token.strip_prefix("avg10=") {
                    some_avg10 = val.parse::<f64>().unwrap_or(0.0);
                } else if let Some(val) = token.strip_prefix("avg60=") {
                    some_avg60 = val.parse::<f64>().unwrap_or(0.0);
                } else if let Some(val) = token.strip_prefix("avg300=") {
                    some_avg300 = val.parse::<f64>().unwrap_or(0.0);
                }
            }
        } else if parts[0] == "full" {
            for token in &parts[1..] {
                if let Some(val) = token.strip_prefix("avg10=") {
                    full_avg10 = val.parse::<f64>().unwrap_or(0.0);
                }
            }
        }
    }

    if found_some {
        Some(MemoryPressure {
            some_avg10,
            some_avg60,
            some_avg300,
            full_avg10,
        })
    } else {
        None
    }
}

fn read_cgroup_u64(path: &Path) -> Option<u64> {
    use std::fs::File;
    use std::io::Read;

    let mut file = File::open(path).ok()?;
    let mut buf = [0u8; 64];
    let n = file.read(&mut buf).ok()?;
    let s = std::str::from_utf8(&buf[..n]).ok()?;
    s.trim().parse::<u64>().ok()
}

/// Resolves all cgroup directories belonging to the active Steam instance and its background CEF webhelpers.
/// Matches both dedicated `vapour-steam-*.scope` daemon scopes and pressure-vessel `app-steam-unknown-*.scope`
/// units created for CEF renderers.
/// Explicitly excludes game scopes (`app-steam-app*`) to guarantee game memory is never evicted.
pub fn find_steam_cgroup_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let app_slice = PathBuf::from(format!(
        "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice"
    ));

    // 1. Check app.slice for scopes created by vapour or pressure-vessel for Steam webhelpers
    if app_slice.exists() {
        if let Ok(entries) = std::fs::read_dir(&app_slice) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Never target game containers! Valve prefixes games with app-steam-app<appid>
                if name.starts_with("app-steam-app") {
                    continue;
                }
                if (name.starts_with("vapour-steam-") || name.starts_with("app-steam-unknown-"))
                    && name.ends_with(".scope")
                {
                    let cg_dir = entry.path();
                    if cg_dir.join("memory.reclaim").exists() {
                        dirs.push(cg_dir);
                    }
                }
            }
        }
    }

    // 2. Fallback / supplementary: inspect /proc/<pid>/cgroup for active Steam and steamwebhelper processes
    let mut target_pids = Vec::new();
    if let Some(steam_pid) = get_steam_pid() {
        target_pids.push(steam_pid);
    }
    target_pids.extend(get_steamwebhelper_pids());

    for pid in target_pids {
        let cgroup_path = format!("/proc/{}/cgroup", pid);
        if let Ok(cgroup_content) = std::fs::read_to_string(&cgroup_path) {
            for line in cgroup_content.lines() {
                if let Some(rel_path) = line.strip_prefix("0::") {
                    let cg_dir = Path::new("/sys/fs/cgroup").join(rel_path.trim_start_matches('/'));
                    // Never target game containers (check the leaf, not ancestor directories)
                    if cg_dir
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("app-steam-app"))
                    {
                        continue;
                    }
                    if cg_dir.join("memory.reclaim").exists() {
                        dirs.push(cg_dir);
                    }
                }
            }
        }
    }

    dirs.sort();
    dirs.dedup();
    dirs
}

/// Result of a kernel cgroup v2 memory reclamation pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReclaimResult {
    /// Physical RAM freed (both dropped file cache and anonymous pages evicted).
    pub mem_freed: u64,
    /// Bytes of anonymous memory compressed into swap / zRAM.
    pub swap_delta: u64,
}

/// 0 = unknown, 1 = `swappiness=` accepted, 2 = rejected (kernel < 6.11).
static SWAPPINESS_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Asks the kernel to reclaim up to 1500M. Uses the `swappiness=200` bias to push idle CEF heap
/// into swap/zRAM; after the first EINVAL it remembers the kernel lacks it and writes the plain request.
fn write_reclaim_request(reclaim_file: &Path) -> std::io::Result<()> {
    use std::sync::atomic::Ordering::Relaxed;
    if SWAPPINESS_STATE.load(Relaxed) != 2 {
        match std::fs::write(reclaim_file, "1500M swappiness=200\n") {
            Ok(()) => {
                SWAPPINESS_STATE.store(1, Relaxed);
                return Ok(());
            }
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => SWAPPINESS_STATE.store(2, Relaxed),
            Err(e) => {
                // EAGAIN means the request was parsed and partially served, so the key is supported.
                if e.raw_os_error() == Some(libc::EAGAIN) {
                    SWAPPINESS_STATE.store(1, Relaxed);
                }
                return Err(e);
            }
        }
    }
    std::fs::write(reclaim_file, "1500M\n")
}

/// Kernel capabilities, each found by trying the feature (never from `uname`).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Caps {
    /// `memory.reclaim` exists and is writable in the user's `app.slice`.
    pub reclaim: bool,
    /// `pidfd_open(getpid())` succeeds.
    pub pidfd: bool,
    /// `/proc/pressure/memory` is readable.
    pub psi: bool,
    /// The user's `app.slice` exposes `cgroup.events`.
    pub cgroup_events: bool,
}

impl Caps {
    pub fn get() -> &'static Caps {
        static CAPS: std::sync::OnceLock<Caps> = std::sync::OnceLock::new();
        CAPS.get_or_init(|| {
            // SAFETY: getuid has no preconditions and cannot fail.
            let uid = unsafe { libc::getuid() };
            let app = PathBuf::from(format!(
                "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice"
            ));
            let writable = |p: &Path| {
                std::ffi::CString::new(p.as_os_str().as_encoded_bytes())
                    // SAFETY: c is a live NUL-terminated path; access only reads it.
                    .is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0)
            };
            Caps {
                reclaim: writable(&app.join("memory.reclaim")),
                pidfd: PidFd::open(std::process::id()).is_some(),
                psi: std::fs::File::open("/proc/pressure/memory").is_ok(),
                cgroup_events: app.join("cgroup.events").exists(),
            }
        })
    }

    /// `Some(true/false)` once a PSI trigger write has shown whether triggers are accepted.
    pub fn psi_trigger() -> Option<bool> {
        match PSI_TRIGGER_STATE.load(std::sync::atomic::Ordering::Relaxed) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    /// `Some(true/false)` once a reclaim write has shown whether `swappiness=` is accepted.
    pub fn reclaim_swappiness() -> Option<bool> {
        match SWAPPINESS_STATE.load(std::sync::atomic::Ordering::Relaxed) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }
}

/// Triggers kernel cgroup v2 memory reclamation on the Steam process and CEF webhelper cgroups.
/// Safely no-ops on systems without cgroups v2, kernels < 5.19, or lacking write permissions.
pub fn reclaim_steam_memory() -> Result<ReclaimResult, crate::error::VapourError> {
    if !crate::launcher::is_steam_running() {
        return Err(crate::error::VapourError::SteamDaemon(
            "Steam is not running. Launch Steam or start a game before reclaiming memory.".into(),
        ));
    }

    let cg_dirs = find_steam_cgroup_dirs();
    if cg_dirs.is_empty() {
        return Err(crate::error::VapourError::CgroupUnavailable(PathBuf::from(
            "/sys/fs/cgroup",
        )));
    }

    let mut total_swap_delta: u64 = 0;
    let mut total_mem_freed: u64 = 0;
    let mut any_reclaimed = false;

    for cg_dir in &cg_dirs {
        let reclaim_file = cg_dir.join("memory.reclaim");
        if !reclaim_file.exists() {
            continue;
        }

        let swap_file = cg_dir.join("memory.swap.current");
        let mem_file = cg_dir.join("memory.current");

        let swap_before = read_cgroup_u64(&swap_file).unwrap_or(0);
        let mem_before = read_cgroup_u64(&mem_file).unwrap_or(0);

        let write_res = write_reclaim_request(&reclaim_file);

        let is_reclaimed = match write_res {
            Ok(_) => true,
            // The kernel returns EAGAIN / WouldBlock when it reclaims all available inactive
            // pages but cannot meet the full 1500M request. This is normal and indicates success.
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.raw_os_error() == Some(libc::EAGAIN) =>
            {
                true
            }
            Err(_) => false,
        };

        if is_reclaimed {
            any_reclaimed = true;
            let swap_after = read_cgroup_u64(&swap_file).unwrap_or(swap_before);
            let mem_after = read_cgroup_u64(&mem_file).unwrap_or(mem_before);

            let swap_delta = swap_after.saturating_sub(swap_before);
            let mem_freed = mem_before.saturating_sub(mem_after);

            total_swap_delta += swap_delta;
            total_mem_freed += mem_freed;
        }
    }

    if any_reclaimed {
        let mb_swapped = total_swap_delta as f64 / (1024.0 * 1024.0);
        let mb_freed = total_mem_freed as f64 / (1024.0 * 1024.0);

        if mb_swapped >= 0.1 && mb_freed >= 0.1 {
            println!(
                "\x1b[32m[MEMORY]\x1b[0m Inactive Steam background pages reclaimed: {:.1} MB compressed into swap/ZRAM ({:.1} MB RAM freed, VAC intact).",
                mb_swapped, mb_freed
            );
        } else if mb_swapped >= 0.1 {
            println!(
                "\x1b[32m[MEMORY]\x1b[0m Inactive Steam background pages reclaimed: {:.1} MB compressed into swap/ZRAM (VAC intact).",
                mb_swapped
            );
        } else if mb_freed >= 0.1 {
            println!(
                "\x1b[32m[MEMORY]\x1b[0m Inactive Steam background pages reclaimed: {:.1} MB RAM freed (VAC intact).",
                mb_freed
            );
        } else {
            println!(
                "\x1b[32m[MEMORY]\x1b[0m Inactive Steam background pages verified minimal (VAC intact)."
            );
        }
    }
    Ok(ReclaimResult {
        mem_freed: total_mem_freed,
        swap_delta: total_swap_delta,
    })
}

/// True if a NUL-separated environ/cmdline blob has an entry exactly equal to one of `wanted`.
pub fn blob_has_exact_entry<S: AsRef<[u8]>>(blob: &[u8], wanted: &[S]) -> bool {
    blob.split(|&b| b == 0)
        .any(|entry| wanted.iter().any(|w| w.as_ref() == entry))
}

/// Attempts to locate active process IDs associated with the target game.
/// Every game process spawned by Steam has `SteamAppId=<appid>` in its `/proc/<pid>/environ`.
/// Highly optimized: filters UID first, inspects 16-byte comm, skips steam client, and parses environ only on target candidates.
pub fn find_game_pids(appid: u32) -> Vec<u32> {
    let mut pids = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return pids;
    };

    // SAFETY: getuid has no preconditions and cannot fail.
    let current_uid = unsafe { libc::getuid() };
    let target_env = format!("SteamAppId={}", appid);
    let target_game_env = format!("SteamGameId={}", appid);
    let long_id_env = format!(
        "SteamGameId={}",
        crate::launcher::calculate_long_appid(appid)
    );

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let Ok(pid) = name_str.parse::<u32>() else {
            continue;
        };

        // 1. Skip all processes owned by root or other users without touching environ
        let Ok(meta) = entry.metadata() else { continue };
        if meta.uid() != current_uid {
            continue;
        }

        // 2. Direct comm check (16 bytes, avoid environ read overhead)
        let comm_path = entry.path().join("comm");
        let comm = std::fs::read_to_string(&comm_path).unwrap_or_default();
        let comm_t = comm.trim();

        // Skip Steam client daemon and helper processes directly
        if comm_t == "steam" || comm_t == "steamwebhelper" {
            continue;
        }

        // 3. Fast cmdline check: matches Steam reaper, pressure-vessel, and proton wrappers immediately
        let cmd_path = entry.path().join("cmdline");
        if let Ok(cmd_data) = std::fs::read(&cmd_path) {
            let appid_needle = format!("AppId={}", appid);
            if cmd_data
                .split(|&b| b == 0)
                .any(|tok| tok.ends_with(appid_needle.as_bytes()))
            {
                pids.push(pid);
                continue;
            }
        }

        // 4. Generic check: Steam sets SteamAppId or SteamGameId in /proc/<pid>/environ
        let env_path = entry.path().join("environ");
        if let Ok(data) = std::fs::read(&env_path) {
            if blob_has_exact_entry(&data, &[&target_env, &target_game_env, &long_id_env]) {
                pids.push(pid);
            }
        }
    }

    pids
}

/// How the exit cloud sync ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudOutcome {
    /// Non-Steam shortcut: there is no cloud sync to wait for.
    Skipped,
    Synced,
    /// Wait window closed without a completion line.
    TimedOut,
    /// Steam reported a conflict; keep Steam alive so the user can resolve it.
    Conflict,
}

/// Waits for the exit cloud sync of `appid` (invariants 6 and 7 apply).
pub fn wait_cloud(appid: u32, game_name: &str) -> CloudOutcome {
    let is_non_steam = (appid & 0x80000000) != 0;
    if is_non_steam {
        return CloudOutcome::Skipped;
    }
    {
        println!("\x1b[36m[SYNC]\x1b[0m Waiting for Steam Cloud to synchronize save data...");

        let cloud_path = find_steam_log("cloud_log.txt");
        let mut cloud_offset: u64 = 0;
        if let Some(ref path) = cloud_path {
            if let Ok(m) = std::fs::metadata(path) {
                cloud_offset = m.len(); // Exact EOF at moment of exit to eliminate false positives
            }
        }

        let sync_start = Instant::now();
        let mut cloud_state = CloudState::Waiting;
        let mut last_activity = Instant::now();

        while sync_start.elapsed() < Duration::from_secs(60) {
            if let Some(ref path) = cloud_path {
                if let Ok(mut file) = File::open(path) {
                    if let Ok(len) = file.metadata().map(|m| m.len()) {
                        if len > cloud_offset {
                            last_activity = Instant::now();
                            if file.seek(SeekFrom::Start(cloud_offset)).is_ok() {
                                let mut buf = String::new();
                                if file.read_to_string(&mut buf).is_ok() {
                                    cloud_offset = len;
                                    for line in buf.lines() {
                                        cloud_state = cloud_step(cloud_state, line, appid);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if matches!(cloud_state, CloudState::Done | CloudState::Conflict) {
                break;
            }
            // No exit sync seen and 3 s of silence: assume up-to-date. Once an exit sync has
            // started, only completion, a conflict or the 60 s cap ends the wait.
            if cloud_state == CloudState::Waiting
                && sync_start.elapsed() >= Duration::from_secs(3)
                && last_activity.elapsed() >= Duration::from_secs(3)
            {
                break;
            }
            sleep(Duration::from_millis(300));
        }
        let sync_conflict = cloud_state == CloudState::Conflict;
        let sync_confirmed = cloud_state == CloudState::Done;
        if cloud_state == CloudState::ExitSyncStarted {
            eprintln!(
                "[WARN] Steam started an exit cloud sync for AppID {appid} but never logged completion; Steam's cloud_log format may have changed. Please report it to the vapour maintainers."
            );
        }

        if sync_conflict {
            eprintln!(
                "\x1b[1;31m[CLOUD CONFLICT]\x1b[0m Steam Cloud reported a sync conflict for '{}' (AppID {})!",
                game_name, appid
            );
            eprintln!(
                "                 Steam daemon kept alive so you can resolve the conflict in Steam UI. Save data preserved."
            );
            return CloudOutcome::Conflict;
        } else if sync_confirmed {
            println!("\x1b[32m[OK]\x1b[0m Cloud save sync verified complete.");
            return CloudOutcome::Synced;
        }
        println!("\x1b[36m[INFO]\x1b[0m Cloud sync window closed.");
        CloudOutcome::TimedOut
    }
}

/// Waits for cloud save upload to finish, then issues graceful shutdown.
pub fn wait_cloud_and_shutdown(appid: u32, game_name: &str, keep_steam: bool) {
    if wait_cloud(appid, game_name) == CloudOutcome::Conflict {
        return;
    }
    if keep_steam {
        println!("\x1b[32m[OK]\x1b[0m Leaving Steam running in background (--keep-steam).");
        return;
    }
    shutdown_steam_gracefully();
}

/// Requests a graceful Steam shutdown, escalating to SIGTERM only if cloud sync is not writing.
pub fn shutdown_steam_gracefully() {
    println!("\x1b[36m[SHUTDOWN]\x1b[0m Closing Steam to return RAM and CPU to 0...");
    if let Err(e) = shutdown_steam() {
        eprintln!(
            "\x1b[31m[ERROR]\x1b[0m Failed to request Steam shutdown: {}",
            e
        );
        return;
    }

    // Wait for Steam process to cleanly exit
    let wait_start = Instant::now();
    while is_steam_running() && wait_start.elapsed() < Duration::from_secs(10) {
        sleep(Duration::from_millis(500));
    }

    // Fallback: If still lingering after 10s, verify cloud sync is not actively writing before SIGTERM
    // Open the pidfd right after resolving the PID so later signals cannot hit a recycled PID.
    if let Some(pid) = get_steam_pid() {
        let steam_pfd = PidFd::open(pid);
        let is_actively_writing = if let Some(ref path) = find_steam_log("cloud_log.txt") {
            if let Ok(m) = std::fs::metadata(path) {
                if let Ok(modified) = m.modified() {
                    modified.elapsed().unwrap_or(Duration::from_secs(10)) < Duration::from_secs(2)
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        if !is_actively_writing {
            match &steam_pfd {
                Some(pfd) => pfd.send_signal(libc::SIGTERM),
                None => send_signal(pid, libc::SIGTERM),
            }
            sleep(Duration::from_millis(1500));
        }
    }

    if !is_steam_running() {
        println!("\x1b[32m[OK]\x1b[0m Steam closed cleanly. 0 MB idle RAM.");
        let report = crate::prune::autoclean_session_leftovers();
        if !report.is_empty() {
            if report.shm_files_removed > 0 {
                println!(
                    "\x1b[32m[AUTOCLEAN]\x1b[0m Purged {} orphaned Steam IPC shared memory buffer(s) ({}) from /dev/shm.",
                    report.shm_files_removed,
                    crate::library::format_bytes(report.shm_bytes_freed)
                );
            }
            if report.orphan_procs_killed > 0 {
                println!(
                    "\x1b[32m[AUTOCLEAN]\x1b[0m Terminated {} lingering Steam helper processes.",
                    report.orphan_procs_killed
                );
            }
        }
    } else {
        println!("\x1b[33m[WARN]\x1b[0m Steam shutdown still in progress in background.");
    }
}

// ---- Scope & game detection (moved from daemon.rs) ----

/// Identifies active game AppIDs by scanning systemd transient scopes under `app.slice`.
pub fn detect_active_game_scope(app_slice: &Path) -> Option<(u32, PathBuf)> {
    let Ok(entries) = fs::read_dir(app_slice) else {
        return None;
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("app-steam-app") {
            if let Some(appid) = parse_appid_from_scope_name(&name) {
                return Some((appid, entry.path()));
            }
        }
    }
    None
}

/// Decides whether the tracked game has really exited: ~3 empty 2 s ticks (≈6 s grace for launcher handoffs).
pub fn should_declare_exit(
    tracked_alive: bool,
    replacement_found: bool,
    missing_ticks: u32,
) -> (bool, u32) {
    if tracked_alive || replacement_found {
        return (false, 0);
    }
    let ticks = missing_ticks + 1;
    (ticks >= 3, ticks)
}

/// True if the cgroup file content places the process in a `vapour-steam-*.scope` unit.
pub fn cgroup_is_vapour_scope(content: &str) -> bool {
    content.contains("/vapour-steam-")
}

/// True only for a Steam that vapour itself started (it lives in a `vapour-steam-*.scope`).
/// A user's own `steam -silent` (e.g. tray autostart) is never treated as headless.
pub fn is_steam_headless() -> bool {
    crate::launcher::get_steam_pid()
        .and_then(|pid| fs::read_to_string(format!("/proc/{pid}/cgroup")).ok())
        .is_some_and(|c| cgroup_is_vapour_scope(&c))
}

// ---- Event sources: cgroup.events, PSI triggers, pidfd signals ----

/// 0 = unknown, 1 = PSI triggers accepted, 2 = rejected (old kernel or EPERM).
static PSI_TRIGGER_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Parses `populated 0|1` out of a cgroup `cgroup.events` file.
pub fn events_populated(content: &str) -> Option<bool> {
    content.lines().find_map(|l| match l.trim() {
        "populated 1" => Some(true),
        "populated 0" => Some(false),
        _ => None,
    })
}

/// `/sys/fs/cgroup/.../app.slice` of the current user.
pub fn app_slice_path() -> PathBuf {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!(
        "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice"
    ))
}

/// Finds the systemd scope Steam created for `appid` (`app-steam-app<appid>-*.scope`).
pub fn find_game_scope(app_slice: &Path, appid: u32) -> Option<PathBuf> {
    let prefix = format!("app-steam-app{appid}-");
    fs::read_dir(app_slice)
        .ok()?
        .flatten()
        .find(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with(&prefix) && n.ends_with(".scope")
        })
        .map(|e| e.path())
}

/// PIDs currently inside a scope (`cgroup.procs`).
pub fn scope_pids(scope: &Path) -> Vec<u32> {
    fs::read_to_string(scope.join("cgroup.procs"))
        .map(|c| c.lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

/// True once the scope is gone or its `cgroup.events` reports `populated 0`.
fn scope_exited(scope: &Path) -> bool {
    match fs::read_to_string(scope.join("cgroup.events")) {
        Ok(c) => events_populated(&c) == Some(false),
        Err(_) => !scope.exists(),
    }
}

/// inotify watch on one file (IN_MODIFY); the fd is pollable with POLLIN.
struct FileWatch {
    fd: libc::c_int,
}

impl FileWatch {
    fn new(path: &Path) -> Option<Self> {
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        // SAFETY: inotify_init1 takes only flag constants; the result is checked below.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        // SAFETY: fd is a valid inotify descriptor and c is a live NUL-terminated path.
        let wd = unsafe { libc::inotify_add_watch(fd, c.as_ptr(), libc::IN_MODIFY) };
        if wd < 0 {
            // SAFETY: fd was returned by inotify_init1 above and is closed exactly once.
            unsafe { libc::close(fd) };
            return None;
        }
        Some(Self { fd })
    }

    fn drain(&self) {
        let mut buf = [0u8; 512];
        // SAFETY: buf is a live, writable buffer and the length passed is its length.
        while unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
    }
}

impl Drop for FileWatch {
    fn drop(&mut self) {
        // SAFETY: fd is owned by this struct and closed only here.
        unsafe { libc::close(self.fd) };
    }
}

/// A kernel PSI trigger on `/proc/pressure/memory`: wakes with POLLPRI when "some" tasks
/// stalled for 300 ms within a 2 s window (a multiple of 2 s, as unprivileged triggers need).
struct PsiTrigger {
    fd: libc::c_int,
}

impl PsiTrigger {
    fn open() -> Option<Self> {
        use std::sync::atomic::Ordering::Relaxed;
        let path = std::ffi::CString::new("/proc/pressure/memory").ok()?;
        // SAFETY: path is a live NUL-terminated CString.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            PSI_TRIGGER_STATE.store(2, Relaxed);
            return None;
        }
        let trigger = b"some 300000 2000000\0";
        // SAFETY: trigger is a live buffer and the length passed is its length.
        let n = unsafe { libc::write(fd, trigger.as_ptr().cast(), trigger.len()) };
        if n < 0 {
            // SAFETY: fd was returned by open above and is closed exactly once.
            unsafe { libc::close(fd) };
            PSI_TRIGGER_STATE.store(2, Relaxed);
            return None;
        }
        PSI_TRIGGER_STATE.store(1, Relaxed);
        Some(Self { fd })
    }

    fn fired(&self) -> bool {
        let mut p = libc::pollfd {
            fd: self.fd,
            events: libc::POLLPRI,
            revents: 0,
        };
        // SAFETY: p is a valid, exclusively borrowed pollfd and nfds is 1.
        unsafe { libc::poll(&mut p, 1, 0) > 0 && p.revents & libc::POLLPRI != 0 }
    }
}

impl Drop for PsiTrigger {
    fn drop(&mut self) {
        // SAFETY: fd is owned by this struct and closed only here.
        unsafe { libc::close(self.fd) };
    }
}

/// Sends `sig` to `pid` through a pidfd (immune to PID reuse after the fd is open),
/// falling back to `kill(2)` when `pidfd_open` is unavailable.
pub fn send_signal(pid: u32, sig: libc::c_int) {
    if let Some(pfd) = PidFd::open(pid) {
        pfd.send_signal(sig);
        return;
    }
    // SAFETY: kill only sends a signal; callers pass pid > 0 so it never targets a process group.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

// ---- Lifecycle engine ----

/// Who started the running Steam client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteamOrigin {
    /// Lives in a `vapour-steam-*.scope`: vapour started it.
    Owned,
    /// Started by the user (tray autostart, desktop launcher): never shut down by vapour.
    Adopted,
}

pub fn steam_origin() -> SteamOrigin {
    if is_steam_headless() {
        SteamOrigin::Owned
    } else {
        SteamOrigin::Adopted
    }
}

/// What to do with Steam once the game has exited and the cloud sync is settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExit {
    ShutdownSteam,
    RestoreGui,
    KeepRunning,
}

/// Policy inputs for [`decide_on_exit`].
#[derive(Debug, Clone, Copy)]
pub struct ExitCfg {
    pub shutdown_if_headless: bool,
    pub restore_gui_on_exit: bool,
}

/// Pure exit policy: only a vapour-owned Steam is ever shut down, and `--keep-steam` wins.
/// A user's Steam whose GUI was tucked to the tray gets it back.
pub fn decide_on_exit(
    origin: SteamOrigin,
    had_gui: bool,
    keep_steam: bool,
    cfg: ExitCfg,
) -> OnExit {
    match origin {
        SteamOrigin::Owned if cfg.shutdown_if_headless && !keep_steam => OnExit::ShutdownSteam,
        SteamOrigin::Adopted if had_gui && cfg.restore_gui_on_exit => OnExit::RestoreGui,
        _ => OnExit::KeepRunning,
    }
}

/// How the supervised session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// The game exited; cloud wait and `on_exit` have run.
    Exited,
    /// `keep_going` returned false (CLI took over, or the daemon is stopping): no teardown ran.
    Yielded,
    /// Reserved for drivers that distinguish a user interrupt from a yield.
    Interrupted,
}

/// Per-driver policy: the CLI and the daemon share the mechanism but not these knobs.
pub struct SessionPolicy {
    /// Reclaim passes, as offsets from session start (CLI: 12 s, 40 s; daemon: 0 s, 25 s).
    pub reclaim_at: &'static [Duration],
    /// Maximum PSI-triggered passes (CLI: Some(4); daemon: None).
    pub psi_cap: Option<u32>,
    pub psi_cooldown: Duration,
    /// Fallback exit-detection tick; three empty ticks declare exit (~6 s with 2 s).
    pub tick: Duration,
    pub on_exit: OnExit,
    pub verbose: bool,
}

type ReclaimHook = Box<dyn Fn(&ReclaimResult)>;

pub struct GameSession {
    pub appid: u32,
    pub name: String,
    scope: Option<PathBuf>,
    trackers: Vec<ProcessTracker>,
    on_reclaim: Option<ReclaimHook>,
    exit_hint: Option<Box<dyn FnMut() -> bool>>,
}

impl GameSession {
    /// Attaches to a running game: scope first (`app-steam-app<id>-*.scope`), `/proc` as fallback.
    pub fn attach(appid: u32, name: &str, hint_pid: Option<u32>) -> Self {
        let scope = find_game_scope(&app_slice_path(), appid);
        let pids = match (&scope, hint_pid) {
            (Some(s), _) => scope_pids(s),
            (None, Some(p)) => vec![p],
            (None, None) => find_game_pids(appid),
        };
        Self {
            appid,
            name: name.to_string(),
            scope,
            trackers: pids.into_iter().map(ProcessTracker::for_pid).collect(),
            on_reclaim: None,
            exit_hint: None,
        }
    }

    /// Called after every successful reclaim pass (the daemon uses it for notifications).
    pub fn with_reclaim_hook(mut self, f: ReclaimHook) -> Self {
        self.on_reclaim = Some(f);
        self
    }

    /// An extra exit signal (the CLI feeds Steam's "app stopped running" log line here), used when
    /// neither a scope nor any process could be found for the game.
    pub fn with_exit_hint(mut self, f: Box<dyn FnMut() -> bool>) -> Self {
        self.exit_hint = Some(f);
        self
    }

    /// Supervises until the game exits (scope `populated 0`, falling back to pidfd/`/proc` ticks),
    /// running the reclaim schedule and PSI passes, then the cloud wait and `p.on_exit`.
    /// `keep_going` is checked every wake-up; if it returns false the session stops without teardown.
    pub fn run(&mut self, p: &SessionPolicy, keep_going: &dyn Fn() -> bool) -> SessionEnd {
        use std::os::fd::RawFd;
        let start = Instant::now();
        let mut reclaim_idx = 0usize;
        let mut psi_passes = 0u32;
        let mut last_psi = Instant::now();
        let mut psi_pending = false;
        let mut total_mem = 0u64;
        let mut total_swap = 0u64;
        let mut missing_ticks = 0u32;
        let mut last_fallback = Instant::now();
        let mut ever_tracked = self.scope.is_some() || !self.trackers.is_empty();

        let caps = Caps::get();
        let mut watch = self
            .scope
            .as_ref()
            .filter(|_| caps.cgroup_events)
            .and_then(|s| FileWatch::new(&s.join("cgroup.events")));
        let psi = if caps.psi { PsiTrigger::open() } else { None };

        loop {
            if !keep_going() {
                return SessionEnd::Yielded;
            }

            // 1. Exit detection: scope first, pidfd/`/proc` ticks as the fallback.
            if self.scope.is_none() {
                if let Some(s) = find_game_scope(&app_slice_path(), self.appid) {
                    watch = FileWatch::new(&s.join("cgroup.events"));
                    self.scope = Some(s);
                    ever_tracked = true;
                }
            }
            if let Some(scope) = &self.scope {
                if scope_exited(scope) {
                    break;
                }
            } else if last_fallback.elapsed() >= p.tick {
                last_fallback = Instant::now();
                let alive = self.trackers.iter().any(|t| t.is_alive());
                if alive {
                    missing_ticks = 0;
                } else {
                    let fresh = find_game_pids(self.appid);
                    if !fresh.is_empty() {
                        self.trackers = fresh.into_iter().map(ProcessTracker::for_pid).collect();
                        ever_tracked = true;
                        missing_ticks = 0;
                    } else if ever_tracked {
                        let (exit, ticks) = should_declare_exit(false, false, missing_ticks);
                        missing_ticks = ticks;
                        if exit {
                            break;
                        }
                    }
                }
                if !ever_tracked {
                    if let Some(hint) = self.exit_hint.as_mut() {
                        if hint() {
                            break;
                        }
                    }
                }
            }

            // 2. Reclaim schedule.
            if reclaim_idx < p.reclaim_at.len() && start.elapsed() >= p.reclaim_at[reclaim_idx] {
                reclaim_idx += 1;
                self.reclaim(p, &mut total_mem, &mut total_swap, "scheduled pass");
            }

            // 3. PSI-driven reclaim, once the schedule is done.
            let psi_ok = reclaim_idx >= p.reclaim_at.len()
                && p.psi_cap.is_none_or(|cap| psi_passes < cap)
                && last_psi.elapsed() >= p.psi_cooldown;
            if psi_ok {
                let pressure = match &psi {
                    Some(t) => std::mem::take(&mut psi_pending) || t.fired(),
                    None => read_memory_pressure().is_some_and(|m| m.some_avg10 >= 5.0),
                };
                if pressure {
                    psi_passes += 1;
                    last_psi = Instant::now();
                    if p.verbose {
                        println!("\x1b[33m[PSI]\x1b[0m System memory pressure elevated. Reclaiming idle Steam/CEF pages...");
                    }
                    self.reclaim(p, &mut total_mem, &mut total_swap, "PSI pass");
                }
            } else {
                // Events during the schedule or cooldown are stale by the time a pass is allowed.
                psi_pending = false;
            }

            // 4. Sleep until a cgroup.events write, a PSI event, or the next tick.
            let mut fds: Vec<libc::pollfd> = Vec::new();
            let mut add = |fd: RawFd, events: i16| {
                fds.push(libc::pollfd {
                    fd,
                    events,
                    revents: 0,
                })
            };
            if let Some(w) = &watch {
                add(w.fd, libc::POLLIN);
            }
            if let Some(t) = &psi {
                add(t.fd, libc::POLLPRI);
            }
            let timeout = p.tick.min(Duration::from_secs(1)).as_millis() as libc::c_int;
            if fds.is_empty() {
                sleep(Duration::from_millis(timeout as u64));
            } else {
                // SAFETY: fds is a live slice of valid pollfds and the length passed is its length.
                unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
                // The kernel clears a PSI trigger event when it is polled (psi.c psi_trigger_poll), so latch it here.
                if psi.is_some() {
                    psi_pending |= fds.last().is_some_and(|f| f.revents & libc::POLLPRI != 0);
                }
                if let Some(w) = &watch {
                    w.drain();
                }
            }
        }

        // Game exited: settle the cloud sync, then apply the exit policy.
        println!(
            "\x1b[36m[EXIT]\x1b[0m '{}' has exited. Validating Steam Cloud sync...",
            self.name
        );
        let cloud = wait_cloud(self.appid, &self.name);
        match p.on_exit {
            OnExit::ShutdownSteam if cloud != CloudOutcome::Conflict && is_steam_running() => {
                shutdown_steam_gracefully();
            }
            OnExit::RestoreGui => crate::daemon::restore_steam_gui(),
            _ => println!("\x1b[32m[OK]\x1b[0m Leaving Steam running in background."),
        }
        SessionEnd::Exited
    }

    fn reclaim(&self, p: &SessionPolicy, mem: &mut u64, swap: &mut u64, what: &str) {
        if let Ok(res) = reclaim_steam_memory() {
            *mem += res.mem_freed;
            *swap += res.swap_delta;
            if p.verbose {
                println!(
                    "\x1b[32m[RECLAIM]\x1b[0m {what}: evicted {} ({} in zRAM) [Total: {} RAM, {} in zRAM].",
                    crate::library::format_bytes(res.mem_freed),
                    crate::library::format_bytes(res.swap_delta),
                    crate::library::format_bytes(*mem),
                    crate::library::format_bytes(*swap),
                );
            }
            if let Some(hook) = &self.on_reclaim {
                hook(&res);
            }
        }
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;

    #[test]
    fn events_populated_should_parse_populated_flag() {
        assert_eq!(events_populated("populated 1\nfrozen 0\n"), Some(true));
        assert_eq!(events_populated("populated 0\nfrozen 0\n"), Some(false));
        assert_eq!(events_populated("frozen 0\n"), None);
    }

    #[test]
    fn find_game_scope_should_match_only_the_requested_appid() {
        let tmp = tempfile::tempdir().unwrap();
        for n in ["app-steam-app730-111.scope", "app-steam-app7300-222.scope"] {
            fs::create_dir(tmp.path().join(n)).unwrap();
        }
        let found = find_game_scope(tmp.path(), 730).unwrap();
        assert!(found.ends_with("app-steam-app730-111.scope"));
        assert!(find_game_scope(tmp.path(), 73).is_none());
    }

    const CFG: ExitCfg = ExitCfg {
        shutdown_if_headless: true,
        restore_gui_on_exit: true,
    };

    #[test]
    fn decide_on_exit_should_shut_down_only_owned_steam_without_keep() {
        use OnExit::*;
        use SteamOrigin::*;
        assert_eq!(decide_on_exit(Owned, false, false, CFG), ShutdownSteam);
        assert_eq!(decide_on_exit(Owned, true, false, CFG), ShutdownSteam);
        assert_eq!(decide_on_exit(Owned, false, true, CFG), KeepRunning);
        let no_shutdown = ExitCfg {
            shutdown_if_headless: false,
            ..CFG
        };
        assert_eq!(
            decide_on_exit(Owned, false, false, no_shutdown),
            KeepRunning
        );
    }

    #[test]
    fn decide_on_exit_should_never_shut_down_adopted_steam() {
        use OnExit::*;
        use SteamOrigin::*;
        assert_eq!(decide_on_exit(Adopted, true, false, CFG), RestoreGui);
        assert_eq!(decide_on_exit(Adopted, false, false, CFG), KeepRunning);
        let no_restore = ExitCfg {
            restore_gui_on_exit: false,
            ..CFG
        };
        assert_eq!(
            decide_on_exit(Adopted, true, false, no_restore),
            KeepRunning
        );
        assert_eq!(decide_on_exit(Adopted, true, true, CFG), RestoreGui);
    }
}
