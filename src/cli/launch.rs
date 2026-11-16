use inquire::Select;

use crate::config::VapourConfig;
use crate::error::{VapourError, VapourResult};
use crate::launcher;
use crate::library::get_installed_games;
use crate::mount;
use crate::non_steam::{self, load_non_steam_games};
use crate::watcher::watch_and_cleanup;

use super::tui::extract_acronyms;
use super::{is_interactive, LaunchExecutionOptions, PlayableItem};

pub fn find_item_by_query(items: &[PlayableItem], query: &str) -> Option<PlayableItem> {
    find_item_by_query_with(items, query, is_interactive())
}

fn ambiguous(query: &str, names: &[&str]) -> Option<PlayableItem> {
    eprintln!(
        "Ambiguous query '{}' matches: {}. Use the AppID.",
        query,
        names.join(", ")
    );
    None
}

fn item_name(item: &PlayableItem) -> &str {
    match item {
        PlayableItem::Steam(g) => &g.name,
        PlayableItem::NonSteam(ng) => &ng.name,
    }
}

fn find_item_by_query_with(
    items: &[PlayableItem],
    query: &str,
    interactive: bool,
) -> Option<PlayableItem> {
    let q_lower = query.trim().to_lowercase();
    let q_clean = q_lower.replace(['-', '_', ' '], "");
    if q_lower.is_empty() {
        return None;
    }

    // 1. Exact AppID match
    if let Ok(target_appid) = q_lower.parse::<u32>() {
        for item in items {
            let appid = match item {
                PlayableItem::Steam(g) => g.appid,
                PlayableItem::NonSteam(ng) => ng.appid,
            };
            if appid == target_appid {
                return Some(item.clone());
            }
        }
    }

    // 2. Exact Acronym match
    let acronym_matches: Vec<&PlayableItem> = items
        .iter()
        .filter(|item| {
            let name = match item {
                PlayableItem::Steam(g) => &g.name,
                PlayableItem::NonSteam(ng) => &ng.name,
            };
            extract_acronyms(name).iter().any(|a| a == &q_lower)
        })
        .collect();

    if acronym_matches.len() == 1 {
        return Some(acronym_matches[0].clone());
    } else if acronym_matches.len() > 1 {
        if !interactive {
            let names: Vec<&str> = acronym_matches.iter().map(|i| item_name(i)).collect();
            return ambiguous(query, &names);
        }
        println!("Multiple matches for acronym '{}':", query);
        return Select::new(
            "Select game:",
            acronym_matches.into_iter().cloned().collect(),
        )
        .prompt()
        .ok();
    }

    // 3. Name substring match
    let matches: Vec<&PlayableItem> = items
        .iter()
        .filter(|item| {
            let name = match item {
                PlayableItem::Steam(g) => &g.name,
                PlayableItem::NonSteam(ng) => &ng.name,
            };
            let clean = name.to_lowercase().replace(['-', '_', ' '], "");
            clean.contains(&q_clean)
        })
        .collect();

    // An exact (normalized) name beats substring hits: "portal" must not be ambiguous with "Portal 2".
    let exact: Vec<&&PlayableItem> = matches
        .iter()
        .filter(|item| item_name(item).to_lowercase().replace(['-', '_', ' '], "") == q_clean)
        .collect();
    if exact.len() == 1 {
        return Some((*exact[0]).clone());
    }

    match matches.len() {
        0 => {
            eprintln!("No games matched query '{}'.", query);
            None
        }
        1 => Some(matches[0].clone()),
        _ => {
            if interactive {
                println!("Multiple matches for '{}':", query);
                Select::new("Select game:", matches.into_iter().cloned().collect())
                    .prompt()
                    .ok()
            } else {
                let names: Vec<&str> = matches.iter().map(|i| item_name(i)).collect();
                ambiguous(query, &names)
            }
        }
    }
}

pub fn execute_game_launch(
    config: &VapourConfig,
    query: &str,
    opts: &LaunchExecutionOptions,
) -> VapourResult<()> {
    let cli_keep_steam = opts.keep_steam;
    let cli_overlay = if opts.no_overlay {
        Some(false)
    } else if opts.overlay {
        Some(true)
    } else {
        None
    };

    let steam_games = get_installed_games(false).unwrap_or_default();
    let non_steam_games = load_non_steam_games(config);

    let mut items: Vec<PlayableItem> = steam_games.into_iter().map(PlayableItem::Steam).collect();
    items.extend(non_steam_games.into_iter().map(PlayableItem::NonSteam));

    let selected = find_item_by_query(&items, query).ok_or_else(|| {
        VapourError::GameNotFound(format!("No game found matching query '{}'.", query))
    })?;

    // Prevent Steam from downloading background updates for unrelated games on startup
    crate::library::configure_on_demand_updates();

    let (id_or_name, appid) = match selected {
        PlayableItem::Steam(ref game) => {
            if game.is_unmounted {
                let label = game.unmounted_label.as_deref().unwrap_or("External");
                if !opts.yes {
                    if !is_interactive() {
                        eprintln!("Run with '--yes' to auto-mount unmounted media.");
                        return Err(VapourError::DriveUnmounted {
                            game: game.name.clone(),
                            mount_point: game.library_path.clone(),
                        });
                    }
                    let confirm = inquire::Confirm::new(&format!(
                        "Library drive '{}' is unmounted. Mount now via udisksctl to launch '{}'?",
                        label, game.name
                    ))
                    .with_default(true)
                    .prompt()
                    .unwrap_or(false);

                    if !confirm {
                        println!("Launch cancelled.");
                        return Ok(());
                    }
                }

                if let Some((_, dev)) = mount::find_unmounted_device_for_path(&game.library_path) {
                    mount::mount_device_with_udisksctl(&dev, is_interactive())?;
                }
            }

            (game.appid.to_string(), Some(game.appid))
        }
        // Settings are saved under the app ID, not the name.
        PlayableItem::NonSteam(ref ng) => (ng.appid.to_string(), Some(ng.appid)),
    };

    let resolved_cfg = config.resolve_launch_config(
        &id_or_name,
        appid,
        cli_overlay,
        cli_keep_steam,
        opts.extra_cli_args,
    );

    match selected {
        PlayableItem::Steam(ref game) => {
            println!(
                "\n\x1b[1;36m[LAUNCH]\x1b[0m Starting '{}' (AppID: {})...",
                game.name, game.appid
            );

            let _guard = if !opts.dry_run {
                Some(crate::daemon::CliSessionGuard::acquire(game.appid))
            } else {
                None
            };

            let outcome =
                launcher::launch_game(game.appid, &game.name, &resolved_cfg, opts.dry_run)?;
            if !opts.dry_run {
                println!(
                    "\x1b[32m[OK]\x1b[0m Game dispatched to Steam. Entering lifecycle watcher..."
                );
                watch_and_cleanup(outcome.appid, &game.name, resolved_cfg.keep_steam);
            }
        }
        PlayableItem::NonSteam(ref ng) => {
            if opts.direct {
                println!(
                    "\n\x1b[1;36m[LAUNCH]\x1b[0m Starting non-Steam game '{}' directly...",
                    ng.name
                );
                let maybe_pid = non_steam::launch_non_steam(ng, &resolved_cfg, opts.dry_run)?;
                if !opts.dry_run {
                    if let Some(pid) = maybe_pid {
                        println!(
                            "\x1b[32m[OK]\x1b[0m Direct process launched (PID: {}).",
                            pid
                        );
                    }
                }
                return Ok(());
            }

            println!(
                "\n\x1b[1;36m[LAUNCH]\x1b[0m Starting non-Steam shortcut '{}' (AppID: {})...",
                ng.name, ng.appid
            );

            let _guard = if !opts.dry_run {
                Some(crate::daemon::CliSessionGuard::acquire(ng.appid))
            } else {
                None
            };

            let outcome =
                launcher::launch_steam_shortcut(ng.appid, &ng.name, &resolved_cfg, opts.dry_run)?;
            if !opts.dry_run {
                println!("\x1b[32m[OK]\x1b[0m Shortcut dispatched to Steam. Entering lifecycle watcher...");
                watch_and_cleanup(outcome.appid, &ng.name, resolved_cfg.keep_steam);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Game;

    fn item(appid: u32, name: &str) -> PlayableItem {
        PlayableItem::Steam(Game {
            appid,
            name: name.to_string(),
            installdir: String::new(),
            library_path: Default::default(),
            size_on_disk: 0,
            last_played: 0,
            buildid: 0,
            is_unmounted: false,
            unmounted_label: None,
        })
    }

    #[test]
    fn execute_game_launch_should_return_not_found_when_no_match() {
        let opts = LaunchExecutionOptions {
            extra_cli_args: &[],
            dry_run: true,
            no_overlay: false,
            overlay: false,
            direct: false,
            keep_steam: false,
            yes: false,
        };
        let err = execute_game_launch(&VapourConfig::default(), "zzzz-no-such-game-qqqq", &opts)
            .unwrap_err();
        assert!(matches!(err, VapourError::GameNotFound(_)));
    }

    #[test]
    fn find_item_should_match_multi_word_query() {
        let items = [item(220, "Half-Life 2"), item(730, "Counter-Strike 2")];
        let found = find_item_by_query_with(&items, "half life", false).unwrap();
        assert!(matches!(found, PlayableItem::Steam(g) if g.appid == 220));
    }

    #[test]
    fn find_item_should_return_none_when_ambiguous_and_not_interactive() {
        let items = [item(1, "Portal"), item(2, "Portal 2")];
        assert!(find_item_by_query_with(&items, "port", false).is_none());
    }

    #[test]
    fn find_item_should_prefer_exact_name_when_also_substring_of_another() {
        let items = [item(1, "Portal"), item(2, "Portal 2")];
        let found = find_item_by_query_with(&items, "portal", false).unwrap();
        assert!(matches!(found, PlayableItem::Steam(g) if g.appid == 1));
    }

    #[test]
    fn find_item_should_match_exact_appid_first() {
        let items = [item(1, "Portal"), item(2, "Portal 2")];
        let found = find_item_by_query_with(&items, "2", false).unwrap();
        assert!(matches!(found, PlayableItem::Steam(g) if g.appid == 2));
    }
}
