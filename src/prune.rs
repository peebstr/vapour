use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{VapourError, VapourResult};
use crate::launcher::is_steam_running;
use crate::library::{
    format_bytes, get_all_library_folders, get_installed_games, is_blacklisted_utility,
};
use crate::mount::{check_library_mount, MountInfo};
use crate::non_steam::load_non_steam_games;

/// Category of an orphaned Steam storage directory or IPC resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PruneTargetKind {
    ProtonPrefix,
    ShaderCache,
    SharedMemory,
    OrphanProcess,
}

impl std::fmt::Display for PruneTargetKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProtonPrefix => write!(f, "Proton Prefix (compatdata)"),
            Self::ShaderCache => write!(f, "Shader Cache (shadercache)"),
            Self::SharedMemory => write!(f, "POSIX Shared Memory (/dev/shm)"),
            Self::OrphanProcess => write!(f, "Orphaned Process & Swap (/proc)"),
        }
    }
}

/// Information about an orphaned storage directory or resource candidate for pruning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrphanCandidate {
    pub appid: u32,
    pub kind: PruneTargetKind,
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// Recursively calculates the byte size of a directory.
pub fn calculate_dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += calculate_dir_size(&p);
            } else if let Ok(meta) = p.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// Identifies all currently active AppIDs across installed Steam games, non-Steam shortcuts,
/// system runtimes, and Proton tools.
pub fn collect_active_appids() -> VapourResult<HashSet<u32>> {
    let mut active = HashSet::new();

    // AppID 0 is Steam's template prefix and must never be pruned
    active.insert(0);

    // Collect all installed official games
    for game in get_installed_games(false).map_err(VapourError::Storage)? {
        active.insert(game.appid);
    }

    // Collect all registered non-Steam games
    let config = crate::config::VapourConfig::load();
    for ns in load_non_steam_games(&config) {
        active.insert(ns.appid);
    }

    // Known Valve system runtimes & compatibility tools
    active.extend(crate::library::VALVE_TOOLS.iter().map(|(id, _)| *id));

    Ok(active)
}

/// Options to control the pruning scan.
#[derive(Debug, Clone, Copy, Default)]
pub struct PruneOptions {
    pub compat_only: bool,
    pub shader_only: bool,
    pub shm_only: bool,
    pub proc_only: bool,
}

/// Scans `/proc` to collect all `/dev/shm` paths that are currently opened by any running process.
pub fn collect_open_shm_files() -> HashSet<PathBuf> {
    let mut open_files = HashSet::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return open_files;
    };

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }

        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };

        // Filter to process directories (pure numeric PIDs)
        if !name_str.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        let fd_dir = entry.path().join("fd");
        let Ok(fd_entries) = fs::read_dir(fd_dir) else {
            continue;
        };

        for fd_entry in fd_entries.flatten() {
            if let Ok(link_target) = fs::read_link(fd_entry.path()) {
                let target_str = link_target.to_string_lossy();
                if target_str.starts_with("/dev/shm/") {
                    let clean_path = if let Some(stripped) = target_str.strip_suffix(" (deleted)") {
                        PathBuf::from(stripped)
                    } else {
                        link_target.clone()
                    };
                    open_files.insert(clean_path);
                    open_files.insert(link_target);
                }
            }
        }
    }

    open_files
}

/// Discovers unreferenced Steam POSIX shared memory files within a target directory (typically `/dev/shm`).
pub fn scan_orphan_shm_in_dir(
    shm_dir: &Path,
    open_files: &HashSet<PathBuf>,
    steam_running: bool,
    uid: u32,
    candidates: &mut Vec<OrphanCandidate>,
) {
    let Ok(entries) = fs::read_dir(shm_dir) else {
        return;
    };

    let shm_prefix = format!("u{uid}-Shm_");
    let valve_ipc_prefix = format!("u{uid}-ValveIPCSharedObj-");
    let valve_prefix = format!("u{uid}-Valve");

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }

        let Some(name_str) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        let is_shm = name_str.starts_with(&shm_prefix);
        let is_valve =
            name_str.starts_with(&valve_ipc_prefix) || name_str.starts_with(&valve_prefix);

        if !is_shm && !is_valve {
            continue;
        }

        // If Steam is running, rendezvous IPC shared objects must be preserved
        if steam_running && is_valve {
            continue;
        }

        // If any process holds an open file handle, do not touch
        if open_files.contains(&path) {
            continue;
        }

        let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        candidates.push(OrphanCandidate {
            appid: 0,
            kind: PruneTargetKind::SharedMemory,
            path,
            size_bytes,
        });
    }
}

/// Scans the system's `/dev/shm` for unreferenced Steam POSIX shared memory buffers.
pub fn scan_orphan_shm(candidates: &mut Vec<OrphanCandidate>) {
    let open_files = collect_open_shm_files();
    let steam_running = is_steam_running();
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    scan_orphan_shm_in_dir(
        Path::new("/dev/shm"),
        &open_files,
        steam_running,
        uid,
        candidates,
    );
}

/// Steam-only helpers. Never add wine*: other launchers (Lutris, Heroic, umu) run them too.
const ORPHAN_PROCESS_COMMS: &[&str] = &["steamwebhelper", "srt-bwrap", "pv-adverb"];

fn is_orphan_candidate_comm(comm: &str) -> bool {
    ORPHAN_PROCESS_COMMS.contains(&comm)
}

/// Scans `/proc` for orphaned Steam helpers, WebHelpers and runtime wrapper processes
/// that remain alive in the background holding swap and RAM after the main Steam daemon has exited.
pub fn scan_orphan_processes(candidates: &mut Vec<OrphanCandidate>) {
    use std::os::unix::fs::MetadataExt;

    // Only prune orphan processes if the primary Steam daemon is not running
    if is_steam_running() {
        return;
    }

    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let Ok(entries) = fs::read_dir("/proc") else {
        return;
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };

        let Ok(pid) = name_str.parse::<u32>() else {
            continue;
        };

        // Don't target our own process
        if pid == std::process::id() {
            continue;
        }

        // Verify process ownership matches current user
        let proc_path = entry.path();
        let Ok(meta) = proc_path.metadata() else {
            continue;
        };
        if meta.uid() != uid {
            continue;
        }

        let comm_path = proc_path.join("comm");
        let Ok(comm) = fs::read_to_string(&comm_path) else {
            continue;
        };
        let comm_trim = comm.trim();

        if is_orphan_candidate_comm(comm_trim) {
            // Read swap and RSS from /proc/<pid>/status
            let mut mem_bytes = 0u64;
            let status_path = proc_path.join("status");
            if let Ok(status) = fs::read_to_string(&status_path) {
                let mut vmrss_kb = 0u64;
                let mut vmswap_kb = 0u64;
                for line in status.lines() {
                    if let Some(rest) = line.strip_prefix("VmRSS:") {
                        let val_str = rest.split_whitespace().next().unwrap_or("0");
                        vmrss_kb = val_str.parse::<u64>().unwrap_or(0);
                    } else if let Some(rest) = line.strip_prefix("VmSwap:") {
                        let val_str = rest.split_whitespace().next().unwrap_or("0");
                        vmswap_kb = val_str.parse::<u64>().unwrap_or(0);
                    }
                }
                mem_bytes = (vmrss_kb + vmswap_kb) * 1024;
            }

            candidates.push(OrphanCandidate {
                appid: pid,
                kind: PruneTargetKind::OrphanProcess,
                path: proc_path,
                size_bytes: mem_bytes,
            });
        }
    }
}

/// Scans all library folders and `/dev/shm` for orphaned compatdata, shadercache directories,
/// and unreferenced Steam POSIX shared memory allocations.
pub fn scan_orphan_storage(options: PruneOptions) -> Vec<OrphanCandidate> {
    let mut candidates = Vec::new();

    // If caller specifically requested --proc-only, scan processes and return immediately
    if options.proc_only {
        scan_orphan_processes(&mut candidates);
        candidates.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
        return candidates;
    }

    // 1. Scan /dev/shm for orphaned Steam shared memory buffers (unless restricted to compat or shader only)
    if !options.compat_only && !options.shader_only {
        scan_orphan_shm(&mut candidates);
    }

    // 2. Scan for orphan processes holding memory/swap when Steam is dead
    if !options.compat_only && !options.shader_only && !options.shm_only {
        scan_orphan_processes(&mut candidates);
    }

    // If caller specifically requested --shm-only, return immediately without touching disk libraries
    if options.shm_only {
        candidates.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
        return candidates;
    }

    let libraries = get_all_library_folders();

    // Check if any library is unmounted. If unmounted, skip directory pruning to avoid
    // deleting prefixes belonging to disconnected drives.
    let any_unmounted = libraries
        .iter()
        .any(|lib| matches!(check_library_mount(lib), MountInfo::Unmounted { .. }));

    if any_unmounted {
        eprintln!(
            "\x1b[33m[WARN]\x1b[0m One or more Steam libraries are currently unmounted. Directory pruning is deferred to prevent data loss."
        );
        candidates.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
        return candidates;
    }

    let Ok(active_appids) = collect_active_appids() else {
        eprintln!(
            "\x1b[33m[WARN]\x1b[0m could not read installed games; directory pruning skipped"
        );
        candidates.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
        return candidates;
    };

    for lib in &libraries {
        let steamapps = lib.join("steamapps");

        // 2. Scan compatdata/
        if !options.shader_only {
            let compat_dir = steamapps.join("compatdata");
            if compat_dir.is_dir() {
                scan_dir_for_orphans(
                    &compat_dir,
                    PruneTargetKind::ProtonPrefix,
                    &active_appids,
                    &libraries,
                    &mut candidates,
                );
            }
        }

        // 3. Scan shadercache/
        if !options.compat_only {
            let shader_dir = steamapps.join("shadercache");
            if shader_dir.is_dir() {
                scan_dir_for_orphans(
                    &shader_dir,
                    PruneTargetKind::ShaderCache,
                    &active_appids,
                    &libraries,
                    &mut candidates,
                );
            }
        }
    }

    // Sort descending by size (largest reclaimable targets first)
    candidates.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
    candidates
}

fn scan_dir_for_orphans(
    parent_dir: &Path,
    kind: PruneTargetKind,
    active_appids: &HashSet<u32>,
    libraries: &[PathBuf],
    candidates: &mut Vec<OrphanCandidate>,
) {
    let Ok(entries) = fs::read_dir(parent_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(name_str) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        let Ok(appid) = name_str.parse::<u32>() else {
            continue;
        };

        // Safety Filter: never prune active games, AppID 0, or Valve runtimes
        if active_appids.contains(&appid) || is_blacklisted_utility(appid, "") {
            continue;
        }

        // An appmanifest in any library means the game is installed, even if the ACF parse missed it.
        let manifest = format!("steamapps/appmanifest_{appid}.acf");
        if libraries.iter().any(|lib| lib.join(&manifest).exists()) {
            continue;
        }

        let size_bytes = calculate_dir_size(&path);
        // Only register if folder actually contains files (> 4 KB)
        if size_bytes > 4096 {
            candidates.push(OrphanCandidate {
                appid,
                kind,
                path,
                size_bytes,
            });
        }
    }
}

/// Executes deletion of identified orphan candidates.
pub fn execute_prune(candidates: &[OrphanCandidate]) -> VapourResult<u64> {
    let mut total_reclaimed = 0u64;

    for candidate in candidates {
        if !candidate.path.exists() {
            continue;
        }

        match candidate.kind {
            PruneTargetKind::ProtonPrefix | PruneTargetKind::ShaderCache => {
                // Safety invariant: never delete AppID 0 (template prefix)
                if candidate.appid == 0 {
                    continue;
                }

                // Ensure path resides strictly inside steamapps/compatdata or steamapps/shadercache
                let path_str = candidate.path.to_string_lossy();
                if !path_str.contains("steamapps/compatdata")
                    && !path_str.contains("steamapps/shadercache")
                {
                    continue;
                }

                if let Ok(()) = fs::remove_dir_all(&candidate.path) {
                    total_reclaimed += candidate.size_bytes;
                }
            }
            PruneTargetKind::SharedMemory => {
                // Safety invariants for shared memory:
                // 1. Path must reside strictly inside /dev/shm/
                let path_str = candidate.path.to_string_lossy();
                if !path_str.starts_with("/dev/shm/") {
                    continue;
                }

                // 2. File name must strictly match current user's Steam IPC prefixes
                let Some(file_name) = candidate.path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                // SAFETY: getuid has no preconditions and cannot fail.
                let uid = unsafe { libc::getuid() };
                let shm_prefix = format!("u{uid}-Shm_");
                let valve_prefix = format!("u{uid}-Valve");
                if !file_name.starts_with(&shm_prefix) && !file_name.starts_with(&valve_prefix) {
                    continue;
                }

                // 3. Remove single file
                if let Ok(()) = fs::remove_file(&candidate.path) {
                    total_reclaimed += candidate.size_bytes;
                }
            }
            PruneTargetKind::OrphanProcess => {
                let pid = candidate.appid as i32;
                if pid > 0 {
                    // One pidfd for both signals: SIGKILL can never hit a recycled PID.
                    let pfd = crate::session::PidFd::open(pid as u32);
                    let alive = || match &pfd {
                        Some(p) => p.is_alive(),
                        None => Path::new(&format!("/proc/{pid}")).exists(),
                    };
                    let send = |sig| match &pfd {
                        Some(p) => p.send_signal(sig),
                        None => crate::session::send_signal(pid as u32, sig),
                    };
                    send(libc::SIGTERM);
                    // Give the process up to 1 s to exit on SIGTERM before escalating.
                    for _ in 0..20 {
                        if !alive() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    if alive() {
                        send(libc::SIGKILL);
                    }
                    total_reclaimed += candidate.size_bytes;
                }
            }
        }
    }

    Ok(total_reclaimed)
}

/// Result of an automated post-session cleanup pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutocleanReport {
    pub shm_bytes_freed: u64,
    pub shm_files_removed: usize,
    pub orphan_procs_killed: usize,
    pub swap_bytes_freed: u64,
}

impl AutocleanReport {
    pub fn is_empty(&self) -> bool {
        self.shm_files_removed == 0 && self.orphan_procs_killed == 0
    }
}

/// Automatically purges orphaned Steam IPC shared memory buffers (/dev/shm)
/// and any lingering helper processes after Steam daemon termination.
///
/// Safety Invariant:
/// Returns early if the primary Steam client is still running or if any other
/// active process holds open file descriptors to the shared memory objects.
pub fn autoclean_session_leftovers() -> AutocleanReport {
    if is_steam_running() {
        return AutocleanReport::default();
    }

    let mut report = AutocleanReport::default();

    // 1. Scan and purge orphaned POSIX shared memory buffers
    let mut shm_candidates = Vec::new();
    scan_orphan_shm(&mut shm_candidates);
    if !shm_candidates.is_empty() {
        report.shm_files_removed = shm_candidates.len();
        if let Ok(bytes) = execute_prune(&shm_candidates) {
            report.shm_bytes_freed = bytes;
        }
    }

    // 2. Scan and terminate any orphaned helper processes
    let mut proc_candidates = Vec::new();
    scan_orphan_processes(&mut proc_candidates);
    if !proc_candidates.is_empty() {
        report.orphan_procs_killed = proc_candidates.len();
        if let Ok(bytes) = execute_prune(&proc_candidates) {
            report.swap_bytes_freed = bytes;
        }
    }

    report
}

/// Renders a human-readable summary of identified orphan storage.
pub fn print_orphan_summary(candidates: &[OrphanCandidate], dry_run: bool) {
    if candidates.is_empty() {
        println!("\x1b[32m[OK]\x1b[0m No orphaned Proton prefixes, shader caches, or shared memory detected. Storage is clean!");
        return;
    }

    let total_bytes: u64 = candidates.iter().map(|c| c.size_bytes).sum();
    let action_str = if dry_run {
        "Prunable (Dry Run)"
    } else {
        "Reclaimable"
    };

    println!(
        "\x1b[1;36m[STORAGE]\x1b[0m Found {} orphaned targets ({} {}):",
        candidates.len(),
        format_bytes(total_bytes),
        action_str
    );
    println!();

    for c in candidates {
        let appid_label = if c.kind == PruneTargetKind::SharedMemory {
            "-".to_string()
        } else {
            c.appid.to_string()
        };
        println!(
            "  • {:<12} [{:>10}] {:<32} {}",
            appid_label,
            format_bytes(c.size_bytes),
            c.kind.to_string(),
            c.path.display()
        );
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_prefix(lib: &Path, appid: u32, manifest: bool) {
        let dir = lib.join(format!("steamapps/compatdata/{appid}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("f"), vec![0u8; 5000]).unwrap();
        if manifest {
            fs::write(lib.join(format!("steamapps/appmanifest_{appid}.acf")), "x").unwrap();
        }
    }

    fn scan(lib: &Path) -> Vec<OrphanCandidate> {
        let mut c = Vec::new();
        scan_dir_for_orphans(
            &lib.join("steamapps/compatdata"),
            PruneTargetKind::ProtonPrefix,
            &HashSet::new(),
            &[lib.to_path_buf()],
            &mut c,
        );
        c
    }

    #[test]
    fn scan_dir_for_orphans_should_skip_appid_with_manifest_file() {
        let tmp = tempfile::tempdir().unwrap();
        make_prefix(tmp.path(), 123, true);
        assert!(scan(tmp.path()).is_empty());
    }

    #[test]
    fn scan_dir_for_orphans_should_flag_appid_without_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        make_prefix(tmp.path(), 123, false);
        assert_eq!(scan(tmp.path()).len(), 1);
    }

    #[test]
    fn is_orphan_candidate_comm_should_reject_wineserver() {
        assert!(!is_orphan_candidate_comm("wineserver"));
    }

    #[test]
    fn is_orphan_candidate_comm_should_accept_steamwebhelper() {
        assert!(is_orphan_candidate_comm("steamwebhelper"));
    }

    #[test]
    fn test_collect_active_appids_contains_zero_and_cs2() {
        let Ok(active) = collect_active_appids() else {
            return; // no readable Steam install in this environment
        };
        assert!(active.contains(&0), "AppID 0 must always be protected");
        assert!(
            active.contains(&228980),
            "Steamworks Redistributables must be protected"
        );
        assert!(
            active.contains(&1628350),
            "SLR 3.0 sniper must be protected"
        );
        assert!(active.contains(&2805730), "Proton 9.0 must be protected");
    }

    #[test]
    fn test_prune_target_kind_display() {
        assert_eq!(
            format!("{}", PruneTargetKind::ProtonPrefix),
            "Proton Prefix (compatdata)"
        );
        assert_eq!(
            format!("{}", PruneTargetKind::ShaderCache),
            "Shader Cache (shadercache)"
        );
        assert_eq!(
            format!("{}", PruneTargetKind::SharedMemory),
            "POSIX Shared Memory (/dev/shm)"
        );
        assert_eq!(
            format!("{}", PruneTargetKind::OrphanProcess),
            "Orphaned Process & Swap (/proc)"
        );
    }

    #[test]
    fn test_safety_check_prevents_arbitrary_deletion() {
        let bad_candidate = OrphanCandidate {
            appid: 730,
            kind: PruneTargetKind::ProtonPrefix,
            path: PathBuf::from("/etc/passwd"),
            size_bytes: 1000,
        };
        // Path does not contain steamapps/compatdata, so execute_prune must ignore it
        let reclaimed = execute_prune(&[bad_candidate]).unwrap();
        assert_eq!(reclaimed, 0);

        let bad_shm = OrphanCandidate {
            appid: 0,
            kind: PruneTargetKind::SharedMemory,
            path: PathBuf::from("/tmp/u1000-Shm_evil"),
            size_bytes: 1000,
        };
        // Path does not start with /dev/shm/, so execute_prune must ignore it
        let reclaimed_shm = execute_prune(&[bad_shm]).unwrap();
        assert_eq!(reclaimed_shm, 0);
    }

    #[test]
    fn test_scan_orphan_shm_in_dir_filters() {
        let temp_dir = tempfile::tempdir().unwrap();
        let shm_dir = temp_dir.path();

        let uid = 1000;
        let orphaned_file = shm_dir.join(format!("u{uid}-Shm_deadbeef"));
        let inuse_file = shm_dir.join(format!("u{uid}-Shm_active123"));
        let valve_rendezvous = shm_dir.join(format!("u{uid}-ValveIPCSharedObj-Steam"));
        let other_user_file = shm_dir.join("u1001-Shm_someoneelse");
        let foreign_file = shm_dir.join("random_file.tmp");

        fs::write(&orphaned_file, vec![0u8; 1024]).unwrap();
        fs::write(&inuse_file, vec![0u8; 2048]).unwrap();
        fs::write(&valve_rendezvous, vec![0u8; 4096]).unwrap();
        fs::write(&other_user_file, vec![0u8; 512]).unwrap();
        fs::write(&foreign_file, vec![0u8; 256]).unwrap();

        let mut open_files = HashSet::new();
        open_files.insert(inuse_file.clone());

        // Scenario 1: Steam is running -> valve_rendezvous is protected
        let mut candidates = Vec::new();
        scan_orphan_shm_in_dir(shm_dir, &open_files, true, uid, &mut candidates);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, orphaned_file);
        assert_eq!(candidates[0].size_bytes, 1024);
        assert_eq!(candidates[0].kind, PruneTargetKind::SharedMemory);

        // Scenario 2: Steam is NOT running -> valve_rendezvous is also marked for prune
        let mut candidates2 = Vec::new();
        scan_orphan_shm_in_dir(shm_dir, &open_files, false, uid, &mut candidates2);

        assert_eq!(candidates2.len(), 2);
        let paths: HashSet<PathBuf> = candidates2.into_iter().map(|c| c.path).collect();
        assert!(paths.contains(&orphaned_file));
        assert!(paths.contains(&valve_rendezvous));
        assert!(!paths.contains(&inuse_file));
        assert!(!paths.contains(&other_user_file));
        assert!(!paths.contains(&foreign_file));
    }

    #[test]
    fn test_autoclean_report_properties() {
        let empty = AutocleanReport::default();
        assert!(empty.is_empty());

        let with_shm = AutocleanReport {
            shm_bytes_freed: 2048,
            shm_files_removed: 1,
            orphan_procs_killed: 0,
            swap_bytes_freed: 0,
        };
        assert!(!with_shm.is_empty());

        let with_proc = AutocleanReport {
            shm_bytes_freed: 0,
            shm_files_removed: 0,
            orphan_procs_killed: 1,
            swap_bytes_freed: 1024,
        };
        assert!(!with_proc.is_empty());
    }

    #[test]
    fn test_autoclean_session_leftovers_does_not_panic() {
        // Runs against live system, must never panic or crash
        let _ = autoclean_session_leftovers();
    }
}
