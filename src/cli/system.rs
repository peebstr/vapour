use std::time::Duration;

use inquire::{Confirm, Select};

use crate::config::VapourConfig;
use crate::desktop;
use crate::doctor::run_doctor_checks;
use crate::error::{VapourError, VapourResult};
use crate::launcher::shutdown_steam;
use crate::library::{self, get_installed_games};
use crate::non_steam::load_non_steam_games;
use crate::prune;
use crate::telemetry::{
    collect_telemetry_snapshot, print_telemetry_snapshot_rich, run_telemetry_hud,
};

use super::launch::find_item_by_query;
use super::list::show_selected_item_info;
use super::{is_interactive, DaemonArgs, DesktopEntriesArgs, PlayableItem, PruneArgs};
use crate::daemon;

pub fn handle_doctor(json: bool, plain: bool, deep: bool) -> VapourResult<()> {
    let report = run_doctor_checks(deep);
    if json {
        println!("{}", report.to_json());
    } else {
        report.print_human(plain);
    }
    if !report.all_healthy {
        return Err(VapourError::Runtime(
            "Environment checks reported problems".into(),
        ));
    }
    Ok(())
}

pub fn handle_shutdown(quiet: bool, json: bool) -> VapourResult<()> {
    if !quiet && !json {
        println!("\x1b[36m[INFO]\x1b[0m Requesting graceful Steam shutdown...");
    }
    shutdown_steam()?;

    // Wait briefly for Steam to exit so we can autoclean /dev/shm
    let wait_start = std::time::Instant::now();
    while crate::launcher::is_steam_running()
        && wait_start.elapsed() < std::time::Duration::from_secs(3)
    {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    let report = crate::prune::autoclean_session_leftovers();

    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "shutdown_signal_sent",
                "shm_bytes_freed": report.shm_bytes_freed,
                "orphan_procs_killed": report.orphan_procs_killed,
            })
        );
    } else if !quiet {
        println!("\x1b[32m[OK]\x1b[0m Shutdown signal sent.");
        if report.shm_files_removed > 0 {
            println!(
                "\x1b[32m[AUTOCLEAN]\x1b[0m Purged {} leftover Steam IPC shared memory buffer(s) ({}) from /dev/shm.",
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
    Ok(())
}

pub fn handle_prune(args: PruneArgs, json: bool) -> VapourResult<()> {
    let opts = prune::PruneOptions {
        compat_only: args.compat_only,
        shader_only: args.shader_only,
        shm_only: args.shm_only,
        proc_only: args.proc_only,
    };

    let candidates = prune::scan_orphan_storage(opts);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&candidates).unwrap_or_default()
        );
        return Ok(());
    }

    if candidates.is_empty() {
        println!("\x1b[32m[OK]\x1b[0m No orphaned Proton prefixes, shader caches, or shared memory buffers detected. Storage is clean!");
        return Ok(());
    }

    prune::print_orphan_summary(&candidates, args.dry_run);

    if args.dry_run {
        println!(
            "\x1b[36m[NOTICE]\x1b[0m Dry-run completed. No files were modified. Re-run without --dry-run to purge."
        );
        return Ok(());
    }

    if !args.yes {
        if !is_interactive() {
            return Err(VapourError::Usage(
                "Refusing to delete orphaned targets non-interactively without --yes / -y.".into(),
            ));
        }
        let confirm = Confirm::new("Permanently delete these orphaned targets?")
            .with_default(false)
            .prompt()
            .unwrap_or(false);
        if !confirm {
            println!("Pruning aborted by user.");
            return Ok(());
        }
    }

    let reclaimed = prune::execute_prune(&candidates)?;
    println!(
        "\x1b[32m[SUCCESS]\x1b[0m Successfully pruned {} orphaned targets! Reclaimed {}.",
        candidates.len(),
        library::format_bytes(reclaimed)
    );
    Ok(())
}

pub fn handle_desktop_entries(args: DesktopEntriesArgs, json: bool) -> VapourResult<()> {
    let summary = desktop::generate_desktop_entries(args.dry_run, args.clean)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );
    } else {
        desktop::print_desktop_summary(&summary, args.dry_run);
    }
    Ok(())
}

pub fn check_status(watch: bool, interval: f64, json: bool) {
    if watch {
        let dur = Duration::from_secs_f64(interval.max(0.1));
        run_telemetry_hud(dur, None);
        return;
    }

    let snapshot = collect_telemetry_snapshot();
    if json {
        if let Ok(serialized) = serde_json::to_string_pretty(&snapshot) {
            println!("{}", serialized);
        } else {
            println!(
                "{}",
                serde_json::json!({ "steam_running": snapshot.steam_running })
            );
        }
    } else {
        print_telemetry_snapshot_rich(&snapshot);
    }
}

pub fn handle_info(config: &VapourConfig, query: Option<&str>, json: bool) -> VapourResult<()> {
    let steam_games = get_installed_games(false).unwrap_or_default();
    let non_steam = load_non_steam_games(config);

    let mut all_items: Vec<PlayableItem> =
        steam_games.into_iter().map(PlayableItem::Steam).collect();
    all_items.extend(non_steam.into_iter().map(PlayableItem::NonSteam));

    let item = if let Some(q) = query {
        find_item_by_query(&all_items, q)
            .ok_or_else(|| VapourError::GameNotFound(format!("No game found matching '{}'", q)))?
    } else if is_interactive() {
        match Select::new("Select game to inspect:", all_items).prompt() {
            Ok(i) => i,
            _ => return Ok(()),
        }
    } else {
        return Err(VapourError::Usage(
            "Game query is required when running non-interactively. Usage: vapour info <GAME_OR_APPID>".into(),
        ));
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&item.to_unified()).unwrap_or_default()
        );
    } else {
        show_selected_item_info(&item, config);
    }
    Ok(())
}

pub fn handle_daemon(args: DaemonArgs, config: &VapourConfig) -> VapourResult<()> {
    if args.install {
        return daemon::install_systemd_service();
    }

    if args.uninstall {
        return daemon::uninstall_systemd_service();
    }

    if args.status {
        daemon::print_service_status();
        return Ok(());
    }

    // Default: Run supervisor event loop
    daemon::run_supervisor(config.daemon.clone(), args.foreground)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_info_should_return_not_found_when_query_matches_nothing() {
        let cfg = VapourConfig::default();
        let err = handle_info(&cfg, Some("zzzz-no-such-game-qqqq"), true).unwrap_err();
        assert!(matches!(err, VapourError::GameNotFound(_)));
    }
}
