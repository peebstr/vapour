use std::path::PathBuf;

use inquire::{Confirm, Select, Text};

use crate::config::{self, VapourConfig};
use crate::error::{VapourError, VapourResult};
use crate::library::{self, find_steam_root, get_installed_games};
use crate::non_steam::{
    add_non_steam_game, delete_non_steam_game, find_available_protons,
    get_compat_data_path_for_appid, load_non_steam_games, set_compat_tool_in_config_vdf,
    update_shortcut_in_all_vdf, update_shortcut_launch_options_in_all_vdf,
};

use super::launch::find_item_by_query;
use super::{is_interactive, AddArgs, PlayableItem};

/// Reports a shortcuts.vdf write error to stderr; true only if something was updated.
fn vdf_ok(r: crate::error::VapourResult<bool>) -> bool {
    r.unwrap_or_else(|e| {
        eprintln!("\x1b[31m[ERROR]\x1b[0m {e}");
        false
    })
}

pub fn handle_add_game(
    config: &mut VapourConfig,
    args: AddArgs,
    quiet: bool,
    json: bool,
) -> VapourResult<()> {
    if let (Some(name), Some(exe)) = (&args.name, &args.exe) {
        if !exe.exists() && !quiet && !json {
            eprintln!(
                "\x1b[33m[WARN]\x1b[0m Target executable {:?} does not exist on disk at this moment.",
                exe
            );
        }
        let is_windows = exe
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
        let use_proton = is_windows || args.compat.is_some();
        let extra_args: Vec<String> = args
            .launch_options
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(String::from)
            .collect();

        let appid = add_non_steam_game(
            name,
            exe,
            use_proton,
            args.compat.clone(),
            &extra_args,
            config,
        )?;
        {
            {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "ok",
                            "name": name,
                            "appid": appid,
                            "exe": exe,
                            "runner": if let Some(ref t) = args.compat { format!("Proton ({})", t) } else if use_proton { "Proton".to_string() } else { "Native".to_string() },
                        })
                    );
                } else if !quiet {
                    println!(
                        "\x1b[32m[OK]\x1b[0m Successfully registered '{}' to Vapour!",
                        name
                    );
                    println!("     Assigned AppID: {}", appid);
                    if let Some(tool) = args.compat {
                        println!("     Runner:         Proton ({})", tool);
                        println!(
                            "     Prefix Path:    {}",
                            get_compat_data_path_for_appid(appid).display()
                        );
                        println!("     Protontricks:   $ protontricks {} <command>", appid);
                    } else if use_proton {
                        println!("     Runner:         Proton (Default)");
                    } else {
                        println!("     Runner:         Native");
                    }
                }
            }
        }
    } else if is_interactive() {
        interactive_add_non_steam(
            config,
            args.name,
            args.exe,
            args.compat,
            args.launch_options,
        );
    } else {
        return Err(VapourError::Usage(
            "'name' and 'exe' are required when running non-interactively. Usage: vapour add <NAME> <EXE> [--compat <TOOL>] [--launch-options <OPTS>]".into(),
        ));
    }
    Ok(())
}

pub fn handle_remove_game(
    config: &mut VapourConfig,
    query: Option<String>,
    yes: bool,
    quiet: bool,
    json: bool,
) -> VapourResult<()> {
    if let Some(q) = query {
        let steam_games = get_installed_games(false).unwrap_or_default();
        let non_steam = load_non_steam_games(config);
        let mut all_items: Vec<PlayableItem> =
            steam_games.into_iter().map(PlayableItem::Steam).collect();
        all_items.extend(non_steam.into_iter().map(PlayableItem::NonSteam));

        let item = find_item_by_query(&all_items, &q)
            .ok_or_else(|| VapourError::GameNotFound(format!("No game found matching '{}'.", q)))?;

        let game = match item {
            PlayableItem::Steam(sg) => {
                return Err(VapourError::Usage(format!(
                    "'{}' (AppID: {}) is an official Steam game. Official Steam games cannot be deleted from the CLI. Manage them directly through Steam.",
                    sg.name, sg.appid
                )));
            }
            PlayableItem::NonSteam(ng) => ng,
        };

        if !yes {
            if !is_interactive() {
                return Err(VapourError::Usage(format!(
                    "Confirmation flag '--yes' is required when removing a game non-interactively. Usage: vapour remove '{}' --yes",
                    q
                )));
            }
            let prompt = format!(
                "Are you sure you want to delete non-Steam game '{}' (AppID: {}) and its Proton prefix?",
                game.name, game.appid
            );
            let confirmed = Confirm::new(&prompt)
                .with_default(false)
                .prompt()
                .unwrap_or(false);
            if !confirmed {
                println!("Deletion cancelled.");
                return Ok(());
            }
        }

        delete_non_steam_game(&game.appid.to_string(), config)?;
        {
            {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "deleted",
                            "name": game.name,
                            "appid": game.appid,
                        })
                    );
                } else if !quiet {
                    println!(
                        "\x1b[32m[OK]\x1b[0m Successfully removed '{}' (AppID: {})",
                        game.name, game.appid
                    );
                    println!(
                        "     Removed from Vapour settings, shortcuts.vdf, and deleted prefix."
                    );
                }
            }
        }
    } else if is_interactive() {
        interactive_delete_non_steam(config);
    } else {
        return Err(VapourError::Usage(
            "Game name or AppID is required when running non-interactively. Usage: vapour remove <GAME_OR_APPID> [--yes]".into(),
        ));
    }
    Ok(())
}

pub fn handle_compat(
    config: &mut VapourConfig,
    game_query: Option<String>,
    tool_name: Option<String>,
    quiet: bool,
    json: bool,
) -> VapourResult<()> {
    let steam_games = get_installed_games(false).unwrap_or_default();
    let non_steam = load_non_steam_games(config);
    let mut all_items: Vec<PlayableItem> =
        steam_games.into_iter().map(PlayableItem::Steam).collect();
    all_items.extend(non_steam.into_iter().map(PlayableItem::NonSteam));

    let chosen_item = if let Some(ref q) = game_query {
        find_item_by_query(&all_items, q)
            .ok_or_else(|| VapourError::GameNotFound(format!("No game found matching '{}'.", q)))?
    } else if is_interactive() {
        match Select::new("Select game to configure compatibility tool:", all_items).prompt() {
            Ok(i) => i,
            _ => return Ok(()),
        }
    } else {
        return Err(VapourError::Usage(
            "Game name or AppID is required when running non-interactively. Usage: vapour compat <GAME> <TOOL>".into(),
        ));
    };

    let (name, appid, is_official) = match chosen_item {
        PlayableItem::Steam(sg) => (sg.name, sg.appid, true),
        PlayableItem::NonSteam(ng) => (ng.name, ng.appid, false),
    };

    let chosen_tool = if let Some(t) = tool_name {
        t
    } else if is_interactive() {
        let available_tools = find_available_protons();
        if available_tools.is_empty() {
            return Err(VapourError::Runtime(
                "No Proton or compatibility tools discovered.".into(),
            ));
        }
        let tool_names: Vec<String> = available_tools.into_iter().map(|t| t.name).collect();
        match Select::new(
            &format!("Select Compatibility Tool for '{}':", name),
            tool_names,
        )
        .prompt()
        {
            Ok(t) => t,
            _ => return Ok(()),
        }
    } else {
        return Err(VapourError::Usage(format!(
            "Compatibility tool name is required when running non-interactively. Usage: vapour compat '{}' <TOOL>",
            name
        )));
    };

    let steam_root = find_steam_root().unwrap_or_else(|| {
        library::home_dir()
            .map(|h| h.join(".local/share/Steam"))
            .unwrap_or_else(|| PathBuf::from("."))
    });

    set_compat_tool_in_config_vdf(&steam_root, appid, &chosen_tool)?;

    if !is_official {
        let appid_key = appid.to_string();
        let _ = config.set_game_compat_tool(&appid_key, Some(chosen_tool.clone()));
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "status": "ok",
                "name": name,
                "appid": appid,
                "compat_tool": chosen_tool,
            })
        );
    } else if !quiet {
        println!(
            "\x1b[32m[OK]\x1b[0m Assigned Compatibility Tool for '{}' (AppID: {}):",
            name, appid
        );
        println!("     Tool: {}", chosen_tool);
        println!("     Steam config/config.vdf updated.");
    }
    Ok(())
}

pub fn handle_config_get(config: &VapourConfig, key: &str, json: bool) -> VapourResult<()> {
    let k_lower = key.to_lowercase();
    match k_lower.as_str() {
        "overlay" => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "overlay": config.defaults.overlay })
                );
            } else {
                println!("{}", config.defaults.overlay);
            }
        }
        "zero_ram" | "zeroram" | "reclaim" => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "zero_ram": config.defaults.zero_ram })
                );
            } else {
                println!("{}", config.defaults.zero_ram);
            }
        }
        "nojoy" => {
            if json {
                println!("{}", serde_json::json!({ "nojoy": config.defaults.nojoy }));
            } else {
                println!("{}", config.defaults.nojoy);
            }
        }
        _ => {
            if let Some(game_cfg) = config.games.get(&k_lower) {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(game_cfg).unwrap_or_default()
                    );
                } else {
                    println!("Overlay: {:?}", game_cfg.overlay);
                    println!("Extra Args: {:?}", game_cfg.extra_args);
                    println!("Compat Tool: {:?}", game_cfg.compat_tool);
                    println!("Wrapper: {:?}", game_cfg.wrapper);
                }
            } else {
                return Err(VapourError::Usage(format!(
                    "Unknown configuration key '{}'. Valid global keys: overlay, zero_ram, nojoy",
                    key
                )));
            }
        }
    }
    Ok(())
}

pub fn handle_set_command(config: &mut VapourConfig, pair: &str) -> VapourResult<()> {
    let Some((key, val)) = pair.split_once('=') else {
        return Err(VapourError::Runtime(
            "Format must be key=value (e.g. overlay=off)".into(),
        ));
    };

    let val_bool = match val.to_lowercase().as_str() {
        "true" | "1" | "on" | "enable" | "enabled" => true,
        "false" | "0" | "off" | "disable" | "disabled" => false,
        _ => {
            return Err(VapourError::Runtime(
                "Expected boolean value (on/off, true/false)".into(),
            ));
        }
    };

    match key.to_lowercase().as_str() {
        "overlay" => {
            config.defaults.overlay = val_bool;
            println!(
                "\x1b[32m[OK]\x1b[0m Global Overlay set to: {}",
                if val_bool { "ENABLED" } else { "DISABLED" }
            );
        }
        "zero_ram" | "zeroram" | "reclaim" => {
            config.defaults.zero_ram = val_bool;
            println!(
                "\x1b[32m[OK]\x1b[0m Memory Reclaim set to: {}",
                if val_bool {
                    "ENABLED (Auto-Kill Steam)"
                } else {
                    "DISABLED"
                }
            );
        }
        "nojoy" => {
            config.defaults.nojoy = val_bool;
            println!(
                "\x1b[32m[OK]\x1b[0m Disable Joysticks / Gamepads (-nojoy) set to: {}",
                if val_bool {
                    "ENABLED (Gamepads disabled)"
                } else {
                    "DISABLED (Gamepads supported)"
                }
            );
        }
        _ => {
            return Err(VapourError::Runtime(format!(
                "Unknown setting key: '{}'. Valid keys: overlay, zero_ram, nojoy",
                key
            )));
        }
    }

    config.save()
}

pub fn interactive_settings(config: &mut VapourConfig) {
    let original_config = config.clone();
    let mut current_idx = 0;

    loop {
        let menu_items = vec![
            format!(
                "Overlay (Global):          [{}]",
                if config.defaults.overlay {
                    "ENABLED"
                } else {
                    "DISABLED"
                }
            ),
            format!(
                "Memory Reclaim (Global):   [{}]",
                if config.defaults.zero_ram {
                    "ENABLED (Steam auto-quits)"
                } else {
                    "DISABLED (Steam stays running)"
                }
            ),
            format!(
                "Suppress Gamepads (-nojoy):[{}]",
                if config.defaults.nojoy {
                    "ENABLED (Joysticks ignored)"
                } else {
                    "DISABLED (Gamepads supported)"
                }
            ),
            "Configure Specific Game...".to_string(),
            "Save and Exit".to_string(),
            "Discard Changes and Exit".to_string(),
        ];

        let selection = match Select::new("Vapour Configuration Settings:", menu_items)
            .with_starting_cursor(current_idx)
            .prompt()
        {
            Ok(s) => s,
            Err(_) => break,
        };

        if selection.starts_with("Overlay (Global):") {
            current_idx = 0;
            config.defaults.overlay = !config.defaults.overlay;
        } else if selection.starts_with("Memory Reclaim") {
            current_idx = 1;
            config.defaults.zero_ram = !config.defaults.zero_ram;
        } else if selection.starts_with("Suppress Gamepads") {
            current_idx = 2;
            config.defaults.nojoy = !config.defaults.nojoy;
        } else if selection.starts_with("Configure Specific Game") {
            current_idx = 3;
            interactive_game_settings(config);
        } else if selection.starts_with("Save and Exit") {
            if let Err(e) = config.save() {
                eprintln!("\x1b[31m[ERROR]\x1b[0m Failed to save settings: {}", e);
            } else {
                println!("\x1b[32m[OK]\x1b[0m Settings saved successfully to ~/.config/vapour/settings.toml");
            }
            break;
        } else {
            *config = original_config;
            println!("Changes discarded.");
            break;
        }
    }
}

pub fn interactive_game_settings(config: &mut VapourConfig) {
    let steam_games = get_installed_games(false).unwrap_or_default();
    let non_steam = load_non_steam_games(config);

    let mut items: Vec<PlayableItem> = steam_games.into_iter().map(PlayableItem::Steam).collect();
    items.extend(non_steam.into_iter().map(PlayableItem::NonSteam));

    if items.is_empty() {
        println!("No games found to configure.");
        return;
    }

    let selection = match Select::new("Select game to configure overrides for:", items).prompt() {
        Ok(s) => s,
        _ => return,
    };

    match selection {
        PlayableItem::Steam(game) => {
            let appid_key = game.appid.to_string();
            let mut game_cfg = config.get_game_config(&appid_key);

            loop {
                let ov_status = match game_cfg.overlay {
                    Some(true) => "Always ON",
                    Some(false) => "Always OFF (Suppressed)",
                    None => "Use Global Default",
                };
                let steam_opts =
                    library::get_steam_localconfig_launch_options(game.appid).unwrap_or_default();
                let menu = vec![
                    format!("Overlay:           [{}]", ov_status),
                    format!(
                        "Steam Launch Options: [{}]",
                        if steam_opts.is_empty() {
                            "None"
                        } else {
                            &steam_opts
                        }
                    ),
                    "Done".to_string(),
                ];

                let action =
                    match Select::new(&format!("Config for Steam Game '{}':", game.name), menu)
                        .prompt()
                    {
                        Ok(a) => a,
                        _ => break,
                    };

                if action.starts_with("Overlay:") {
                    let ov_choices = vec![
                        "Use Global Default".to_string(),
                        "Force ON".to_string(),
                        "Force OFF (Suppressed)".to_string(),
                    ];
                    if let Ok(choice) = Select::new("Overlay Behavior:", ov_choices).prompt() {
                        match choice.as_str() {
                            "Force ON" => game_cfg.overlay = Some(true),
                            "Force OFF (Suppressed)" => game_cfg.overlay = Some(false),
                            _ => game_cfg.overlay = None,
                        }
                    }
                } else if action.starts_with("Steam Launch Options:") {
                    if let Ok(opts) =
                        Text::new("Steam Launch Options (replaces the current ones; empty clears):")
                            .with_initial_value(&steam_opts)
                            .prompt()
                    {
                        let formatted = config::format_launch_options(&opts);
                        match library::write_steam_localconfig_launch_options(
                            game.appid, &game.name, &formatted,
                        ) {
                            Ok(()) => game_cfg.launch_options = None,
                            Err(e) => eprintln!("\x1b[31m[ERROR]\x1b[0m {e}"),
                        }
                    }
                } else {
                    break;
                }
            }

            config.games.insert(appid_key, game_cfg);
        }
        PlayableItem::NonSteam(ng) => {
            let appid_key = ng.appid.to_string();
            let mut game_cfg = config.get_game_config(&appid_key);

            loop {
                let current_exe = game_cfg.exe_path.as_ref().unwrap_or(&ng.exe);
                let exe_status = if current_exe.exists() {
                    current_exe.display().to_string()
                } else {
                    format!("{} [NOT FOUND / MOVED]", current_exe.display())
                };

                let current_tool = game_cfg
                    .compat_tool
                    .as_deref()
                    .or(ng.compat_tool.as_deref())
                    .unwrap_or("Native / None");
                let launch_opts_str = game_cfg.launch_options.as_deref().unwrap_or("None");
                let menu = vec![
                    format!("Executable Path:      [{}]", exe_status),
                    format!("Compatibility Tool:   [{}]", current_tool),
                    format!("Steam Launch Options: [{}]", launch_opts_str),
                    "Done".to_string(),
                ];

                let action =
                    match Select::new(&format!("Config for Non-Steam Game '{}':", ng.name), menu)
                        .prompt()
                    {
                        Ok(a) => a,
                        _ => break,
                    };

                if action.starts_with("Executable Path:") {
                    if let Ok(new_p) = Text::new("New executable path:")
                        .with_default(&current_exe.display().to_string())
                        .prompt()
                    {
                        let trimmed = new_p.trim();
                        if !trimmed.is_empty() {
                            let p = PathBuf::from(trimmed);
                            game_cfg.start_dir = p.parent().map(|d| d.to_path_buf());
                            game_cfg.exe_path = Some(p.clone());
                            vdf_ok(update_shortcut_in_all_vdf(ng.appid, &p, p.parent()));
                        }
                    }
                } else if action.starts_with("Compatibility Tool:") {
                    let mut tools = vec!["Native (Linux Executable)".to_string()];
                    for tool in find_available_protons() {
                        tools.push(format!("Proton: {}", tool.name));
                    }
                    if let Ok(choice) = Select::new("Select Compatibility Tool:", tools).prompt() {
                        if choice.starts_with("Proton: ") {
                            let tool_name = choice.trim_start_matches("Proton: ").to_string();
                            game_cfg.compat_tool = Some(tool_name.clone());
                            if let Some(root) = find_steam_root() {
                                let _ = set_compat_tool_in_config_vdf(&root, ng.appid, &tool_name);
                            }
                        } else {
                            game_cfg.compat_tool = None;
                        }
                    }
                } else if action.starts_with("Steam Launch Options:") {
                    if let Err(e) = library::ensure_game_not_running(ng.appid, &ng.name) {
                        eprintln!("\x1b[31m[ERROR]\x1b[0m {e}");
                        continue;
                    }
                    let current = game_cfg
                        .launch_options
                        .clone()
                        .filter(|o| !o.is_empty())
                        .unwrap_or_else(|| ng.launch_options.clone());
                    if let Ok(opts) =
                        Text::new("Launch Options (replaces the current ones; empty clears):")
                            .with_initial_value(&current)
                            .prompt()
                    {
                        let formatted = config::format_launch_options(&opts);
                        game_cfg.extra_args.clear();
                        game_cfg.launch_options = if formatted.is_empty() {
                            None
                        } else {
                            Some(formatted.clone())
                        };
                        vdf_ok(update_shortcut_launch_options_in_all_vdf(
                            ng.appid, &formatted,
                        ));
                    }
                } else {
                    break;
                }
            }

            config.games.insert(appid_key, game_cfg);
        }
    }
}

pub fn handle_set_path(
    config: &mut VapourConfig,
    query: &str,
    new_path_str: &str,
) -> VapourResult<()> {
    let non_steam = load_non_steam_games(config);
    let items: Vec<PlayableItem> = non_steam.into_iter().map(PlayableItem::NonSteam).collect();
    let item = match find_item_by_query(&items, query) {
        Some(PlayableItem::NonSteam(ng)) => ng,
        _ => {
            return Err(VapourError::Runtime(format!(
                "No Non-Steam game found matching '{}'",
                query
            )));
        }
    };

    let p = PathBuf::from(new_path_str);
    if !p.exists() {
        eprintln!(
            "\x1b[33m[WARN]\x1b[0m Warning: Target path {:?} does not exist right now.",
            p
        );
    }

    let appid_key = item.appid.to_string();
    let start_dir = p.parent().map(|d| d.to_path_buf());
    config.update_game_path(&appid_key, p.clone(), start_dir)?;

    let vdf_updated =
        item.from_steam_shortcut && vdf_ok(update_shortcut_in_all_vdf(item.appid, &p, p.parent()));

    println!(
        "\x1b[32m[OK]\x1b[0m Updated executable path for '{}' (AppID: {}):",
        item.name, item.appid
    );
    println!("     New Path:  {}", p.display());
    if vdf_updated {
        println!("     Steam shortcuts.vdf: Updated");
    }
    Ok(())
}

pub fn handle_set_launch_options(
    config: &mut VapourConfig,
    query: &str,
    raw_options: &str,
) -> VapourResult<()> {
    let mut items: Vec<PlayableItem> = get_installed_games(false)
        .unwrap_or_default()
        .into_iter()
        .map(PlayableItem::Steam)
        .collect();
    items.extend(
        load_non_steam_games(config)
            .into_iter()
            .map(PlayableItem::NonSteam),
    );
    let item = find_item_by_query(&items, query)
        .ok_or_else(|| VapourError::Runtime(format!("No game found matching '{}'", query)))?;

    match item {
        PlayableItem::Steam(game) => {
            let formatted = config::format_launch_options(raw_options);
            library::write_steam_localconfig_launch_options(game.appid, &game.name, &formatted)?;
            // Steam applies these itself now; a copy in settings.toml would be applied twice.
            let key = game.appid.to_string();
            let mut cfg = config.get_game_config(&key);
            if cfg.launch_options.take().is_some() {
                config.games.insert(key, cfg);
                config.save()?;
            }
            println!(
                "\x1b[32m[OK]\x1b[0m Updated Launch Options for '{}' (AppID: {}):",
                game.name, game.appid
            );
            print_launch_options(&formatted);
            println!("     Steam localconfig.vdf: Updated");
        }
        PlayableItem::NonSteam(item) => {
            library::ensure_game_not_running(item.appid, &item.name)?;
            let key = item.appid.to_string();
            let replaced = config.get_game_config(&key).extra_args;
            let formatted = config.update_game_launch_options(&key, raw_options)?;
            if !replaced.is_empty() {
                println!(
                    "\x1b[33m[NOTICE]\x1b[0m Replaced previous arguments: {}",
                    replaced.join(" ")
                );
            }
            let vdf_updated = item.from_steam_shortcut
                && vdf_ok(update_shortcut_launch_options_in_all_vdf(
                    item.appid, &formatted,
                ));
            println!(
                "\x1b[32m[OK]\x1b[0m Updated Launch Options for '{}' (AppID: {}):",
                item.name, item.appid
            );
            print_launch_options(&formatted);
            if vdf_updated {
                println!("     Steam shortcuts.vdf: Updated");
            }
        }
    }
    Ok(())
}

fn print_launch_options(formatted: &str) {
    if formatted.is_empty() {
        println!("     Launch Options: (cleared)");
    } else {
        println!("     Launch Options: {}", formatted);
    }
}

pub fn interactive_delete_non_steam(config: &mut VapourConfig) {
    let non_steam = load_non_steam_games(config);
    if non_steam.is_empty() {
        println!("\x1b[33m[WARN]\x1b[0m No non-Steam games found to delete.");
        return;
    }

    let items: Vec<PlayableItem> = non_steam.into_iter().map(PlayableItem::NonSteam).collect();
    let selection = match Select::new("Select non-Steam game to delete:", items).prompt() {
        Ok(PlayableItem::NonSteam(ng)) => ng,
        _ => return,
    };

    let prompt = format!(
        "Are you sure you want to delete '{}' (AppID: {}) and its Proton prefix?",
        selection.name, selection.appid
    );
    let confirmed = Confirm::new(&prompt)
        .with_default(false)
        .prompt()
        .unwrap_or(false);
    if !confirmed {
        println!("Deletion cancelled.");
        return;
    }

    println!(
        "\x1b[36m[DELETE]\x1b[0m Deleting non-Steam game '{}' (AppID: {})...",
        selection.name, selection.appid
    );
    match delete_non_steam_game(&selection.appid.to_string(), config) {
        Ok(_) => {
            println!(
                "\x1b[32m[OK]\x1b[0m Successfully removed '{}'",
                selection.name
            );
            println!("     Removed from Vapour settings, shortcuts.vdf, and deleted prefix.");
        }
        Err(e) => {
            eprintln!("\x1b[31m[ERROR]\x1b[0m Error: {}", e);
        }
    }
}

pub fn interactive_add_non_steam(
    config: &mut VapourConfig,
    prefill_name: Option<String>,
    prefill_exe: Option<PathBuf>,
    prefill_compat: Option<String>,
    prefill_opts: Option<String>,
) {
    println!("==> Add a Non-Steam Game to Vapour\n");

    let name = if let Some(n) = prefill_name {
        n
    } else {
        match Text::new("Game Title:").prompt() {
            Ok(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => return,
        }
    };

    let exe = if let Some(e) = prefill_exe {
        e
    } else {
        let exe_str = match Text::new("Executable Path (.exe or Linux binary):").prompt() {
            Ok(e) if !e.trim().is_empty() => e.trim().to_string(),
            _ => return,
        };
        PathBuf::from(&exe_str)
    };

    if !exe.exists() {
        eprintln!(
            "\x1b[33m[WARN]\x1b[0m File {:?} does not exist at this moment.",
            exe
        );
    }

    let is_windows = exe
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));

    let (use_proton, compat_tool) = if let Some(tool) = prefill_compat {
        (true, Some(tool))
    } else {
        let available_tools = find_available_protons();
        let mut tool_options = Vec::new();
        if !is_windows {
            tool_options.push("Native (Linux Executable)".to_string());
        }
        for tool in &available_tools {
            tool_options.push(format!("Proton: {}", tool.name));
        }
        if is_windows {
            tool_options.push("Native (No Proton)".to_string());
        }

        if tool_options.is_empty() {
            (is_windows, None)
        } else {
            match Select::new("Select Runner / Compatibility Tool:", tool_options).prompt() {
                Ok(ref choice) if choice.starts_with("Proton: ") => {
                    let tool_name = choice.trim_start_matches("Proton: ").to_string();
                    (true, Some(tool_name))
                }
                Ok(ref choice) if choice.starts_with("Native") => (false, None),
                _ => (is_windows, None),
            }
        }
    };

    let args_str = if let Some(opts) = prefill_opts {
        opts
    } else {
        Text::new("Launch arguments (optional, press Enter for none):")
            .prompt()
            .unwrap_or_default()
    };
    let args: Vec<String> = args_str.split_whitespace().map(String::from).collect();

    match add_non_steam_game(&name, &exe, use_proton, compat_tool.clone(), &args, config) {
        Ok(appid) => {
            println!(
                "\n\x1b[32m[OK]\x1b[0m Successfully registered '{}' to Vapour!",
                name
            );
            println!("     Assigned AppID: {}", appid);
            if let Some(tool) = compat_tool {
                println!("     Runner:         Proton ({})", tool);
                println!(
                    "     Prefix Path:    {}",
                    get_compat_data_path_for_appid(appid).display()
                );
                println!("     Protontricks:   $ protontricks {} <command>", appid);
            } else {
                println!("     Runner:         Native");
            }
        }
        Err(e) => {
            eprintln!("\x1b[31m[ERROR]\x1b[0m Error adding game: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_config_get_should_return_usage_err_when_key_unknown() {
        let cfg = VapourConfig::default();
        let err = handle_config_get(&cfg, "no-such-key", false).unwrap_err();
        assert!(matches!(err, VapourError::Usage(_)));
    }

    #[test]
    fn handle_set_command_should_return_err_when_format_invalid() {
        let mut cfg = VapourConfig::default();
        assert!(handle_set_command(&mut cfg, "overlay").is_err());
        assert!(handle_set_command(&mut cfg, "overlay=maybe").is_err());
    }
}
