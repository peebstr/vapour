use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::library::{extract_vdf_value, find_steam_root, home_dir};
pub use crate::session::*;

/// Finds a Steam log file (e.g. `content_log.txt`) under the Steam root or the standard home locations.
pub(crate) fn find_steam_log(file: &str) -> Option<PathBuf> {
    if let Some(root) = find_steam_root() {
        let p = root.join("logs").join(file);
        if p.exists() {
            return Some(p);
        }
    }
    let home = home_dir()?;
    [
        home.join(".local/share/Steam/logs").join(file),
        home.join(".steam/steam/logs").join(file),
    ]
    .into_iter()
    .find(|c| c.exists())
}

/// Resolves an AppID to a human-friendly name (e.g. 1628350 -> "Steam Linux Runtime 3.0 (sniper)").
pub fn resolve_app_name(appid: u32) -> String {
    if let Some(name) = crate::library::valve_tool_name(appid) {
        return name.to_string();
    }

    for lib in crate::library::get_all_library_folders() {
        let manifest = lib.join(format!("steamapps/appmanifest_{appid}.acf"));
        if let Ok(m_content) = std::fs::read_to_string(&manifest) {
            if let Some(name) = extract_vdf_value(&m_content, "name") {
                return name.to_string();
            }
        }
    }

    // Non-Steam games registered in shortcuts.vdf
    if (appid & 0x80000000) != 0 {
        let config = crate::config::VapourConfig::load();
        for nsg in crate::non_steam::load_non_steam_games(&config) {
            if nsg.appid == appid {
                return nsg.name;
            }
        }
    }

    format!("AppID {}", appid)
}

/// Gets the current downloaded bytes for an AppID directly from its manifest or staged downloading directory.
pub fn get_app_downloaded_bytes(appid: u32) -> Option<(u64, u64)> {
    for lib in crate::library::get_all_library_folders() {
        let manifest = lib.join(format!("steamapps/appmanifest_{appid}.acf"));
        let downloading_dir = lib.join(format!("steamapps/downloading/{appid}"));

        let dir_bytes = if downloading_dir.is_dir() {
            crate::prune::calculate_dir_size(&downloading_dir)
        } else {
            0
        };

        if let Ok(content) = std::fs::read_to_string(&manifest) {
            let total = extract_vdf_value(&content, "BytesToDownload")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let downloaded = extract_vdf_value(&content, "BytesDownloaded")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);

            let effective_downloaded = downloaded.max(dir_bytes);
            if total > 0 || effective_downloaded > 0 {
                return Some((effective_downloaded, total));
            }
        } else if dir_bytes > 0 {
            return Some((dir_bytes, 0));
        }
    }
    None
}

/// Parses a line from content_log.txt for AppID state changes.
/// Returns Some(true) if the game transitioned to running.
/// Returns Some(false) if the game transitioned to stopped.
pub fn parse_app_running_state(line: &str, appid: u32) -> Option<bool> {
    let needle = format!("AppID {appid} state changed : ");
    if let Some(idx) = line.find(&needle) {
        let state = &line[idx + needle.len()..];
        if state.contains("App Running") {
            return Some(true);
        } else {
            return Some(false);
        }
    }
    None
}

/// Checks if an update started, extracting AppID and total download size in MB if logged.
pub fn parse_update_started(line: &str) -> Option<(u32, Option<f64>)> {
    let start_idx = line.find("AppID ")?;
    let rest = &line[start_idx + 6..];
    let end_id = rest.find(' ')?;
    let appid = rest[..end_id].parse::<u32>().ok()?;

    if line.contains("update started :") {
        if let Some(dl_idx) = line.find("download ") {
            let dl_rest = &line[dl_idx + 9..];
            if let Some(slash_idx) = dl_rest.find('/') {
                let after_slash = &dl_rest[slash_idx + 1..];
                if let Some(comma) = after_slash.find(',') {
                    if let Ok(bytes) = after_slash[..comma].trim().parse::<u64>() {
                        if bytes > 0 {
                            let mb = (bytes as f64) / (1024.0 * 1024.0);
                            return Some((appid, Some(mb)));
                        } else {
                            return Some((appid, None));
                        }
                    }
                }
            }
        }
        return Some((appid, None));
    }

    if line.contains("state changed :")
        && (line.contains("Update Started") || line.contains("Update Running"))
    {
        return Some((appid, None));
    }

    None
}

/// Parses update phase from content_log.txt (e.g. Verifying, Preallocating, Downloading, Installing).
pub fn parse_update_phase(line: &str) -> Option<(u32, &'static str)> {
    let start_idx = line.find("AppID ")?;
    let rest = &line[start_idx + 6..];
    let end_id = rest.find(' ')?;
    let appid = rest[..end_id].parse::<u32>().ok()?;

    let change_needle = "App update changed :";
    let change_idx = line.find(change_needle)?;
    let state = &line[change_idx + change_needle.len()..];

    if state.contains("Verifying") {
        Some((appid, "Verifying"))
    } else if state.contains("Preallocating") {
        Some((appid, "Preallocating"))
    } else if state.contains("Downloading") {
        Some((appid, "Downloading"))
    } else if state.contains("Committing") || state.contains("Staging") {
        Some((appid, "Installing"))
    } else {
        None
    }
}

/// Checks if an update completed for an AppID.
pub fn parse_update_finished(line: &str) -> Option<u32> {
    let start_idx = line.find("AppID ")?;
    let rest = &line[start_idx + 6..];
    let end_id = rest.find(' ')?;
    let appid = rest[..end_id].parse::<u32>().ok()?;

    if line.contains("finished update") {
        return Some(appid);
    }

    if line.contains("state changed :")
        && line.contains("Fully Installed")
        && !line.contains("Update Running")
        && !line.contains("Update Started")
        && !line.contains("App Running")
    {
        return Some(appid);
    }

    None
}

/// Renders a clean ASCII progress bar: [=========>         ]
pub fn render_progress_bar(pct: f64, width: usize) -> String {
    let pct_clamped = pct.clamp(0.0, 100.0);
    let filled = ((pct_clamped / 100.0) * (width as f64)).round() as usize;
    let empty = width.saturating_sub(filled);
    let mut bar = String::with_capacity(width + 2);
    bar.push('[');
    for _ in 0..filled {
        bar.push('=');
    }
    if filled < width {
        bar.push('>');
        for _ in 0..empty.saturating_sub(1) {
            bar.push(' ');
        }
    }
    bar.push(']');
    bar
}

#[derive(Debug, Clone)]
struct ActiveAppUpdate {
    name: String,
    phase: &'static str,
    total_bytes: u64,
}

/// Returns true if an updating AppID is relevant to the target game being launched.
/// Discards background updates for unrelated games that are not runtime dependencies.
pub fn is_relevant_update(up_id: u32, target_appid: u32) -> bool {
    if up_id == target_appid {
        return true;
    }

    // Shared Valve Steam Linux Runtime / Proton runtimes and common redistributables
    crate::library::valve_tool_name(up_id).is_some()
}

/// Monitors a launched game until it exits, then triggers a graceful Steam shutdown.
pub fn watch_and_cleanup(appid: u32, game_name: &str, keep_steam: bool) {
    println!(
        "\x1b[36m[WATCH]\x1b[0m Monitoring '{}' (AppID: {})...",
        game_name, appid
    );
    println!("        (Press Ctrl+C to detach; Steam will stay running)");

    // Register clean Ctrl+C detach handler
    let _ = ctrlc::set_handler(move || {
        print!("\r\x1b[?25h\n\x1b[33m[DETACH]\x1b[0m Detached from game watcher (Ctrl+C). Steam will remain active in background.\n");
        let _ = std::io::stdout().flush();
        std::process::exit(0);
    });

    let log_path = find_steam_log("content_log.txt");
    let mut offset: u64 = 0;

    if let Some(ref path) = log_path {
        if let Ok(metadata) = std::fs::metadata(path) {
            offset = metadata.len();
        }
    }

    let mut game_active = false;
    let mut active_updates: HashMap<u32, ActiveAppUpdate> = HashMap::new();
    let mut launch_start = Instant::now();

    loop {
        sleep(Duration::from_millis(800));

        // 1. Detect launch via the game scope or /proc
        if !game_active && !find_game_pids(appid).is_empty() {
            game_active = true;
            active_updates.clear();
            println!(
                "\x1b[32m[RUNNING]\x1b[0m '{}' is now active. Steam idle in background.",
                game_name
            );
        }

        // 2. Read new log lines from content_log.txt
        if let Some(ref path) = log_path {
            if let Ok(mut file) = File::open(path) {
                if let Ok(len) = file.metadata().map(|m| m.len()) {
                    if len > offset {
                        if file.seek(SeekFrom::Start(offset)).is_ok() {
                            let mut buf = String::new();
                            if file.read_to_string(&mut buf).is_ok() {
                                offset = len;
                                for line in buf.lines() {
                                    if !game_active {
                                        // Check for update starts (filtered to relevant AppIDs only)
                                        if let Some((up_id, size_mb)) = parse_update_started(line) {
                                            if !is_relevant_update(up_id, appid) {
                                                continue;
                                            }

                                            let name = resolve_app_name(up_id);
                                            let total_bytes = size_mb
                                                .map(|mb| (mb * 1024.0 * 1024.0) as u64)
                                                .unwrap_or(0);

                                            active_updates.entry(up_id).or_insert(
                                                ActiveAppUpdate {
                                                    name: name.clone(),
                                                    phase: "Downloading",
                                                    total_bytes,
                                                },
                                            );

                                            if let Some(mb) = size_mb {
                                                println!(
                                                    "\x1b[33m[UPDATE]\x1b[0m Starting update for '{}' ({:.1} MB)...",
                                                    name, mb
                                                );
                                            } else {
                                                println!(
                                                    "\x1b[33m[UPDATE]\x1b[0m Starting update for '{}' (AppID: {})...",
                                                    name, up_id
                                                );
                                            }
                                            launch_start = Instant::now();
                                        }

                                        // Check for update phase transitions (Verifying, Preallocating, Committing)
                                        if let Some((phase_id, phase)) = parse_update_phase(line) {
                                            if !is_relevant_update(phase_id, appid) {
                                                continue;
                                            }

                                            if let Some(up) = active_updates.get_mut(&phase_id) {
                                                up.phase = phase;
                                                if phase != "Downloading" {
                                                    println!(
                                                        "\x1b[33m[UPDATE]\x1b[0m '{}': {} files...",
                                                        up.name, phase
                                                    );
                                                }
                                                launch_start = Instant::now();
                                            }
                                        }

                                        // Check for update completion
                                        if let Some(finished_id) = parse_update_finished(line) {
                                            if !is_relevant_update(finished_id, appid) {
                                                continue;
                                            }

                                            if let Some(up) = active_updates.remove(&finished_id) {
                                                println!(
                                                    "\r\x1b[32m[OK]\x1b[0m Finished updating '{}' (AppID: {}).                 ",
                                                    up.name, finished_id
                                                );
                                                let _ = std::io::stdout().flush();
                                                launch_start = Instant::now();
                                            }
                                        }
                                    }

                                    // Check for app running state
                                    if let Some(running) = parse_app_running_state(line, appid) {
                                        if running && !game_active {
                                            game_active = true;
                                            active_updates.clear();
                                            println!("\x1b[32m[RUNNING]\x1b[0m '{}' is now active. Steam idle in background.", game_name);
                                        } else if !running && game_active {
                                            println!("\x1b[36m[EXIT]\x1b[0m Game exit detected from Steam log.");
                                            wait_cloud_and_shutdown(appid, game_name, keep_steam);
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    } else if len < offset {
                        // Log file was rotated or truncated by Steam
                        offset = 0;
                    }
                }
            }
        }

        // 3. Authoritative AppManifest State Check:
        if let Some(manifest) = crate::library::get_app_manifest_state(appid) {
            if manifest.is_updating() {
                // Steam is actively downloading, verifying, or staging an update
                launch_start = Instant::now(); // Prevent false launch timeout
                let dl_bytes = manifest
                    .bytes_downloaded
                    .max(get_app_downloaded_bytes(appid).map(|(d, _)| d).unwrap_or(0));
                if manifest.bytes_to_download > 0 && dl_bytes < manifest.bytes_to_download {
                    let pct = (dl_bytes as f64 / manifest.bytes_to_download as f64 * 100.0)
                        .clamp(0.0, 100.0);
                    let bar = render_progress_bar(pct, 18);
                    let cur_mb = dl_bytes as f64 / (1024.0 * 1024.0);
                    let tot_mb = manifest.bytes_to_download as f64 / (1024.0 * 1024.0);
                    print!(
                        "\r\x1b[33m[UPDATE]\x1b[0m Manifest update: {} {:>5.1}% ({:.1}/{:.1} MB)",
                        bar, pct, cur_mb, tot_mb
                    );
                    let _ = std::io::stdout().flush();
                } else if manifest.bytes_to_stage > 0
                    && manifest.bytes_staged < manifest.bytes_to_stage
                {
                    let pct = (manifest.bytes_staged as f64 / manifest.bytes_to_stage as f64
                        * 100.0)
                        .clamp(0.0, 100.0);
                    let bar = render_progress_bar(pct, 18);
                    print!(
                        "\r\x1b[33m[UPDATE]\x1b[0m Staging files: {} {:>5.1}% ({:.1}/{:.1} MB)",
                        bar,
                        pct,
                        manifest.bytes_staged as f64 / (1024.0 * 1024.0),
                        manifest.bytes_to_stage as f64 / (1024.0 * 1024.0)
                    );
                    let _ = std::io::stdout().flush();
                } else {
                    print!(
                        "\r\x1b[33m[UPDATE]\x1b[0m Verifying / updating files...                      "
                    );
                    let _ = std::io::stdout().flush();
                }
            } else if manifest.is_running() && !game_active {
                game_active = true;
                active_updates.clear();
                println!(
                    "\r\x1b[32m[RUNNING]\x1b[0m '{}' is now active (verified from appmanifest).",
                    game_name
                );
            }
        }

        // Game is up: hand over to the shared lifecycle engine (reclaim, exit, cloud, shutdown).
        if game_active {
            break;
        }

        // 3. Render live progress for active downloading updates
        if !game_active && !active_updates.is_empty() {
            for (up_id, update) in &active_updates {
                if update.phase == "Downloading" {
                    let (dl_bytes, tot_bytes) = get_app_downloaded_bytes(*up_id).unwrap_or((0, 0));
                    let total = if update.total_bytes > 0 {
                        update.total_bytes
                    } else {
                        tot_bytes
                    };
                    if total > 0 {
                        let pct = (dl_bytes as f64 / total as f64 * 100.0).clamp(0.0, 100.0);
                        let bar = render_progress_bar(pct, 18);
                        let cur_mb = dl_bytes as f64 / (1024.0 * 1024.0);
                        let tot_mb = total as f64 / (1024.0 * 1024.0);
                        print!(
                            "\r\x1b[33m[UPDATE]\x1b[0m '{}': {} {:>5.1}% ({:.1}/{:.1} MB)",
                            update.name, bar, pct, cur_mb, tot_mb
                        );
                        let _ = std::io::stdout().flush();
                    } else {
                        print!(
                            "\r\x1b[33m[UPDATE]\x1b[0m '{}': Verifying files...                        ",
                            update.name
                        );
                        let _ = std::io::stdout().flush();
                    }
                }
            }
        }

        // 4. Fallback timeout only if not actively updating and game hasn't started after 45s
        if !game_active
            && active_updates.is_empty()
            && launch_start.elapsed() > Duration::from_secs(45)
        {
            println!(
                "\x1b[33m[WARN]\x1b[0m Launch timed out (did not report 'App Running' after 45s)."
            );
            println!("       Leaving Steam running in background.");
            return;
        }
    }

    let origin = steam_origin();
    let on_exit = decide_on_exit(
        origin,
        false,
        keep_steam,
        ExitCfg {
            shutdown_if_headless: true,
            restore_gui_on_exit: false,
        },
    );
    let policy = SessionPolicy {
        reclaim_at: CLI_RECLAIM_AT,
        psi_cap: Some(4),
        psi_cooldown: Duration::from_secs(15),
        tick: Duration::from_secs(2),
        on_exit,
        verbose: true,
    };

    // Steam's own "app stopped running" line stays an exit signal for games whose processes
    // can't be found (no scope, no SteamAppId in the environment).
    let hint = move || {
        let Some(path) = log_path.as_ref() else {
            return false;
        };
        let Ok(mut file) = File::open(path) else {
            return false;
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return false;
        };
        if len < offset {
            offset = 0;
        }
        if len == offset || file.seek(SeekFrom::Start(offset)).is_err() {
            return false;
        }
        let mut buf = String::new();
        if file.read_to_string(&mut buf).is_err() {
            return false;
        }
        offset = len;
        buf.lines()
            .any(|l| parse_app_running_state(l, appid) == Some(false))
    };

    GameSession::attach(appid, game_name, None)
        .with_exit_hint(Box::new(hint))
        .run(&policy, &|| true);
}

/// CLI reclaim schedule: early bootstrap heap at T+12 s, cold CEF renderer heap at T+40 s.
const CLI_RECLAIM_AT: &[Duration] = &[Duration::from_secs(12), Duration::from_secs(40)];

#[cfg(test)]
mod tests {
    #[test]
    fn blob_has_exact_entry_should_reject_prefix_appid() {
        assert!(!super::blob_has_exact_entry(
            b"SteamAppId=7300\0",
            &["SteamAppId=730".to_string()]
        ));
    }

    #[test]
    fn blob_has_exact_entry_should_accept_exact_appid() {
        assert!(super::blob_has_exact_entry(
            b"A=1\0SteamAppId=730\0",
            &["SteamAppId=730".to_string()]
        ));
    }
    use super::*;

    #[test]
    fn test_parse_app_running_state() {
        let line_started =
            "[2026-10-24 10:18:05] AppID 730 state changed : Fully Installed,App Running,";
        let line_stopped = "[2026-10-24 10:19:54] AppID 730 state changed : Fully Installed,";
        let line_unrelated =
            "[2026-10-24 10:18:05] AppID 431960 state changed : Fully Installed,App Running,";

        assert_eq!(parse_app_running_state(line_started, 730), Some(true));
        assert_eq!(parse_app_running_state(line_stopped, 730), Some(false));
        assert_eq!(parse_app_running_state(line_unrelated, 730), None);
    }

    #[test]
    fn test_parse_update_started_with_size() {
        let line = "[2026-10-24 10:17:25] AppID 1628350 update started : download 0/244182992, store 0/0, reuse 0/0, delta 0/0, stage 0/810335462";
        let (id, mb) = parse_update_started(line).unwrap();
        assert_eq!(id, 1628350);
        assert!(mb.is_some());
        let size = mb.unwrap();
        assert!(size > 232.0 && size < 233.0);
    }

    #[test]
    fn test_parse_update_phase() {
        let line_verify = "[2026-10-24 10:17:52] AppID 730 App update changed : Running Update,Verifying Installed,";
        let line_prealloc =
            "[2026-10-24 10:17:52] AppID 730 App update changed : Running Update,Preallocating,";
        let line_download = "[2026-10-24 10:17:52] AppID 730 App update changed : Running Update,Downloading,Staging,";
        let line_commit =
            "[2026-10-24 10:18:02] AppID 730 App update changed : Running Update,Committing,";

        assert_eq!(parse_update_phase(line_verify), Some((730, "Verifying")));
        assert_eq!(
            parse_update_phase(line_prealloc),
            Some((730, "Preallocating"))
        );
        assert_eq!(
            parse_update_phase(line_download),
            Some((730, "Downloading"))
        );
        assert_eq!(parse_update_phase(line_commit), Some((730, "Installing")));
    }

    #[test]
    fn test_render_progress_bar() {
        let bar_0 = render_progress_bar(0.0, 10);
        assert_eq!(bar_0, "[>         ]");

        let bar_50 = render_progress_bar(50.0, 10);
        assert_eq!(bar_50, "[=====>    ]");

        let bar_100 = render_progress_bar(100.0, 10);
        assert_eq!(bar_100, "[==========]");
    }

    #[test]
    fn test_parse_update_finished() {
        let line = "[2026-10-24 10:17:51] AppID 1628350 finished update, 1 mounted depots (BuildID 26116003) : 1628351 (6238007303758425057),";
        assert_eq!(parse_update_finished(line), Some(1628350));
    }

    #[test]
    fn test_resolve_app_name() {
        assert_eq!(
            resolve_app_name(1628350),
            "Steam Linux Runtime 3.0 (sniper)"
        );
        assert_eq!(
            resolve_app_name(1391110),
            "Steam Linux Runtime 2.0 (soldier)"
        );
        assert_eq!(resolve_app_name(99999999), "AppID 99999999");
    }

    /// Feeds the real-log fixture line by line and returns the (line, state) transitions to Done.
    fn done_lines(appid: u32) -> (Vec<String>, CloudState) {
        let log = include_str!("../tests/fixtures/cloud_log_exit.txt");
        let mut st = CloudState::Waiting;
        let mut done = Vec::new();
        for line in log.lines() {
            let next = cloud_step(st, line, appid);
            if next == CloudState::Done && st != CloudState::Done {
                done.push(line.to_string());
            }
            st = next;
        }
        (done, st)
    }

    #[test]
    fn cloud_step_should_complete_only_on_upload_complete_when_upload_needed() {
        let (done, st) = done_lines(730);
        assert_eq!(st, CloudState::Done);
        assert_eq!(done.len(), 1);
        assert!(done[0].ends_with("Upload complete, result OK"));
    }

    #[test]
    fn cloud_step_should_complete_on_build_list_when_nothing_changed() {
        let (done, st) = done_lines(241100);
        assert_eq!(st, CloudState::Done);
        assert!(done[0].ends_with("Upload complete in build list"));
    }

    #[test]
    fn cloud_step_should_ignore_start_of_session_already_synced() {
        let (done, st) = done_lines(7);
        assert!(done.is_empty());
        assert_eq!(st, CloudState::Waiting);
    }

    #[test]
    fn cloud_step_should_not_complete_before_upload_after_exit_sync_started() {
        let mut st = CloudState::Waiting;
        for l in [
            "[AppID 730] Starting sync (up,AC Exit,)",
            "[AppID 730] AutoCloud complete",
            "[AppID 730] Currently already synced to global change number '1', should be nothing to download",
            "[AppID 730] Need to upload file a.txt",
        ] {
            st = cloud_step(st, l, 730);
        }
        assert_eq!(st, CloudState::ExitSyncStarted);
        assert_eq!(
            cloud_step(st, "[AppID 730] Upload complete, result OK", 730),
            CloudState::Done
        );
    }

    #[test]
    fn test_parse_cloud_sync_complete() {
        let line_ok = "[2026-10-24 10:19:58] [AppID 730] Upload complete, result OK";
        let line_autocloud = "[2026-10-24 10:19:58] [AppID 730] AutoCloud complete";
        let line_diff_app = "[2026-10-24 10:19:58] [AppID 241100] Upload complete, result OK";

        assert!(parse_cloud_sync_complete(line_ok, 730));
        assert!(!parse_cloud_sync_complete(line_autocloud, 730));
        assert!(!parse_cloud_sync_complete(line_diff_app, 730));
    }

    #[test]
    fn test_is_relevant_update() {
        let target = 2165744007;

        // Unrelated app updates must be discarded
        assert!(!is_relevant_update(730, target));
        assert!(!is_relevant_update(431960, target));

        // Shared Steam Linux Runtime and Proton dependencies must be allowed
        assert!(is_relevant_update(1628350, target));
        assert!(is_relevant_update(228980, target));
        assert!(is_relevant_update(1493710, target));

        // The target game itself must be allowed
        assert!(is_relevant_update(target, target));
    }

    #[test]
    fn test_cgroup_dir_scope_matching_rules() {
        let is_steam_scope = |name: &str| -> bool {
            if name.starts_with("app-steam-app") {
                return false;
            }
            if name.starts_with("vapour-steam-") && name.ends_with(".scope") {
                return true;
            }
            if name.starts_with("app-steam-unknown-") && name.ends_with(".scope") {
                return true;
            }
            false
        };

        // Game containers must never match
        assert!(!is_steam_scope("app-steam-app730-61209.scope"));
        assert!(!is_steam_scope("app-steam-app1086940-12345.scope"));

        // Steam daemon and CEF helpers must match
        assert!(is_steam_scope("vapour-steam-60020.scope"));
        assert!(is_steam_scope("app-steam-unknown-60170.scope"));
        assert!(!is_steam_scope("app-steamwebhelper-12345.scope"));

        // Unrelated scopes
        assert!(!is_steam_scope("app-discord-41835.scope"));
        assert!(!is_steam_scope("gamemoded.service"));
    }

    #[test]
    fn test_live_find_steam_cgroup_dirs() {
        let dirs = find_steam_cgroup_dirs();
        println!("Live discovered Steam cgroups: {:?}", dirs);
        for d in &dirs {
            assert!(!d.to_string_lossy().contains("app-steam-app"));
        }
    }

    #[test]
    fn test_live_reclaim_steam_memory_does_not_panic() {
        let res = reclaim_steam_memory();
        if !crate::launcher::is_steam_running() {
            assert!(matches!(
                res,
                Err(crate::error::VapourError::SteamDaemon(_))
            ));
        } else if find_steam_cgroup_dirs().is_empty() {
            assert!(matches!(
                res,
                Err(crate::error::VapourError::CgroupUnavailable(_))
            ));
        } else {
            assert!(res.is_ok());
        }
    }

    #[test]
    fn test_parse_cloud_sync_conflict() {
        let line_conflict =
            "[2026-10-24 10:19:58] [AppID 730] Conflict detected between local and remote files";
        let line_mismatch = "[2026-10-24 10:19:58] [AppID 730] Cloud sync mismatch detected";
        let line_diff = "[2026-10-24 10:19:58] [AppID 730] Remote and local files differ";
        let line_ok = "[2026-10-24 10:19:58] [AppID 730] Upload complete, result OK";
        let line_other = "[2026-10-24 10:19:58] [AppID 550] Conflict detected";

        assert!(parse_cloud_sync_conflict(line_conflict, 730));
        assert!(parse_cloud_sync_conflict(line_mismatch, 730));
        assert!(parse_cloud_sync_conflict(line_diff, 730));
        assert!(!parse_cloud_sync_conflict(line_ok, 730));
        assert!(!parse_cloud_sync_conflict(line_other, 730));
    }

    #[test]
    fn test_parse_memory_pressure() {
        let sample = "some avg10=4.25 avg60=2.10 avg300=1.05 total=123456\nfull avg10=1.12 avg60=0.50 avg300=0.10 total=45678\n";
        let psi = parse_memory_pressure(sample).unwrap();
        assert!((psi.some_avg10 - 4.25).abs() < 1e-4);
        assert!((psi.some_avg60 - 2.10).abs() < 1e-4);
        assert!((psi.some_avg300 - 1.05).abs() < 1e-4);
        assert!((psi.full_avg10 - 1.12).abs() < 1e-4);

        assert_eq!(parse_memory_pressure("invalid data"), None);
    }

    #[test]
    fn test_process_tracker_self_pid() {
        let self_pid = std::process::id();
        let tracker = ProcessTracker::for_pid(self_pid);
        assert_eq!(tracker.pid(), self_pid);
        assert!(tracker.is_alive());

        // Invalid PID that shouldn't exist
        let dead_tracker = ProcessTracker::for_pid(999_999_999);
        assert!(!dead_tracker.is_alive());
    }
}
