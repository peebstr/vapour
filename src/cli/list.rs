use std::path::Path;

use crate::config::VapourConfig;
use crate::library::{
    extract_vdf_value, find_steam_root, get_all_library_folders, get_installed_games, SortOrder,
};
use crate::mount::{check_library_mount, MountInfo};
use crate::non_steam::{
    get_compat_data_path_for_appid, load_non_steam_games, parse_compat_tool_mapping,
};

use super::{sort_playable_items, PlayableItem, UnifiedGame};

pub fn list_all_games(
    config: &VapourConfig,
    json: bool,
    dmenu: bool,
    plain: bool,
    sort_order: SortOrder,
) {
    let steam_games = get_installed_games(false).unwrap_or_default();
    let non_steam = load_non_steam_games(config);

    let mut all_items: Vec<PlayableItem> =
        steam_games.into_iter().map(PlayableItem::Steam).collect();
    all_items.extend(non_steam.into_iter().map(PlayableItem::NonSteam));

    sort_playable_items(&mut all_items, sort_order);

    if json {
        let unified: Vec<UnifiedGame> = all_items.iter().map(|i| i.to_unified()).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&unified).unwrap_or_default()
        );
        return;
    }

    if dmenu {
        for item in &all_items {
            match item {
                PlayableItem::Steam(g) => {
                    let mount_status = if g.is_unmounted {
                        format!(
                            "Unmounted ({})",
                            g.unmounted_label.as_deref().unwrap_or("external")
                        )
                    } else {
                        "Mounted".to_string()
                    };
                    println!("{} | {} | Official | {}", g.name, g.appid, mount_status);
                }
                PlayableItem::NonSteam(ng) => {
                    let mount_status = if ng.is_path_missing {
                        "Path Missing"
                    } else {
                        "Mounted"
                    };
                    let runner = if let Some(ref t) = ng.compat_tool {
                        format!("Proton: {}", t)
                    } else if ng.use_proton {
                        "Proton".to_string()
                    } else {
                        "Native".to_string()
                    };
                    println!(
                        "{} | {} | Non-Steam ({}) | {}",
                        ng.name, ng.appid, runner, mount_status
                    );
                }
            }
        }
        return;
    }

    let (c_bold, c_cyan, c_yellow, c_reset) = if plain {
        ("", "", "", "")
    } else {
        ("\x1b[1m", "\x1b[1;36m", "\x1b[33m", "\x1b[0m")
    };

    let official_count = all_items
        .iter()
        .filter(|i| matches!(i, PlayableItem::Steam(_)))
        .count();
    let non_steam_count = all_items.len() - official_count;

    println!("{c_cyan}Steam Games ({} found):{c_reset}\n", official_count);
    for item in &all_items {
        if let PlayableItem::Steam(g) = item {
            let mount_note = if g.is_unmounted {
                format!(
                    " {c_yellow}[UNMOUNTED: {}]{c_reset}",
                    g.unmounted_label.as_deref().unwrap_or("external")
                )
            } else {
                String::new()
            };
            let mut meta = Vec::new();
            if g.size_on_disk > 0 {
                meta.push(crate::library::format_bytes(g.size_on_disk));
            }
            if g.last_played > 0 {
                meta.push(crate::library::format_relative_time(
                    g.last_played,
                    crate::library::current_epoch_secs(),
                ));
            }
            let meta_str = if meta.is_empty() {
                String::new()
            } else {
                format!(" [{}]", meta.join(" | "))
            };
            println!(
                "  - {c_bold}{:<36}{c_reset} [AppID: {:>7}]{meta_str} ({}){}",
                g.name,
                g.appid,
                g.library_path.display(),
                mount_note
            );
        }
    }

    if non_steam_count > 0 {
        println!(
            "\n{c_cyan}Non-Steam Games ({} found):{c_reset}\n",
            non_steam_count
        );
        for item in &all_items {
            if let PlayableItem::NonSteam(ng) = item {
                let runner = if let Some(ref tool) = ng.compat_tool {
                    format!("Proton ({})", tool)
                } else if ng.use_proton {
                    "Proton".to_string()
                } else {
                    "Native".to_string()
                };
                let status_note = if ng.is_path_missing {
                    format!(" {c_yellow}[PATH NOT FOUND / MOVED]{c_reset}")
                } else {
                    String::new()
                };
                println!(
                    "  - {c_bold}{:<36}{c_reset} [{}] [AppID: {}]{}\n    Path: {}",
                    ng.name,
                    runner,
                    ng.appid,
                    status_note,
                    ng.exe.display()
                );
            }
        }
    }
}

pub fn list_libraries(json: bool, plain: bool) {
    let all_libs = get_all_library_folders();

    if json {
        let mut list = Vec::new();
        for lib in all_libs {
            match check_library_mount(&lib) {
                MountInfo::Mounted {
                    device,
                    mountpoint,
                    fstype,
                } => {
                    list.push(serde_json::json!({
                        "path": lib,
                        "status": "mounted",
                        "device": device,
                        "mountpoint": mountpoint,
                        "fstype": fstype,
                    }));
                }
                MountInfo::Unmounted { label, device, .. } => {
                    list.push(serde_json::json!({
                        "path": lib,
                        "status": "unmounted",
                        "label": label,
                        "device": device,
                    }));
                }
                MountInfo::DirectoryMissing { path } => {
                    list.push(serde_json::json!({
                        "path": path,
                        "status": "missing",
                    }));
                }
            }
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&list).unwrap_or_default()
        );
        return;
    }

    let (c_green, c_yellow, c_red, c_reset) = if plain {
        ("", "", "", "")
    } else {
        ("\x1b[32m", "\x1b[33m", "\x1b[31m", "\x1b[0m")
    };

    println!("Registered Steam Libraries ({}):\n", all_libs.len());
    for lib in all_libs {
        match check_library_mount(&lib) {
            MountInfo::Mounted {
                device,
                mountpoint,
                fstype,
            } => {
                println!(
                    "  {c_green}[OK]{c_reset} {:<40} [Mounted: {} ({}) on {}]",
                    lib.display(),
                    device,
                    fstype,
                    mountpoint.display()
                );
            }
            MountInfo::Unmounted { label, device, .. } => {
                println!(
                    "  {c_red}[UNMOUNTED]{c_reset} {:<33} [label '{}', device {}]",
                    lib.display(),
                    label,
                    device.display()
                );
            }
            MountInfo::DirectoryMissing { path } => {
                println!(
                    "  {c_yellow}[MISSING]{c_reset} {:<35} [Directory does not exist]",
                    path.display()
                );
            }
        }
    }
}

pub fn show_selected_item_info(item: &PlayableItem, config: &VapourConfig) {
    match item {
        PlayableItem::Steam(g) => {
            println!("\n\x1b[1;36m=== Game Information: {} ===\x1b[0m", g.name);
            println!("  AppID:               {}", g.appid);
            if g.is_unmounted {
                let label = g.unmounted_label.as_deref().unwrap_or("External");
                println!(
                    "  Mount Status:        \x1b[33mUNMOUNTED (Drive: {})\x1b[0m",
                    label
                );
            } else {
                println!("  Mount Status:        \x1b[32mMOUNTED\x1b[0m");
            }
            println!("  Library Path:        {}", g.library_path.display());
            let full_install_path = g.library_path.join("steamapps/common").join(&g.installdir);
            println!("  Install Directory:   {}", full_install_path.display());

            if g.size_on_disk > 0 {
                let gb = g.size_on_disk as f64 / (1024.0 * 1024.0 * 1024.0);
                if gb >= 1.0 {
                    println!("  Size on Disk:        {:.2} GB", gb);
                } else {
                    let mb = g.size_on_disk as f64 / (1024.0 * 1024.0);
                    println!("  Size on Disk:        {:.1} MB", mb);
                }
            }

            if g.last_played > 0 {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let diff = now.saturating_sub(g.last_played);
                let rel = if diff < 60 {
                    "just now".to_string()
                } else if diff < 3600 {
                    format!("{} minutes ago", diff / 60)
                } else if diff < 86400 {
                    format!("{} hours ago", diff / 3600)
                } else if diff < 86400 * 30 {
                    format!("{} days ago", diff / 86400)
                } else {
                    format!("{} months ago", diff / (86400 * 30))
                };
                println!("  Last Played:         {}", rel);
            } else {
                println!("  Last Played:         Never");
            }

            if g.buildid > 0 {
                println!("  Build ID:            {}", g.buildid);
            }

            let appid_key = g.appid.to_string();
            let resolved =
                config.resolve_launch_config(&appid_key, Some(g.appid), None, false, &[]);

            println!("\n\x1b[1mConfiguration:\x1b[0m");
            println!(
                "  Overlay Active:      {}",
                if resolved.overlay {
                    "\x1b[32mENABLED\x1b[0m"
                } else {
                    "\x1b[33mSUPPRESSED\x1b[0m"
                }
            );
            println!(
                "  Memory Reclaim:      {}",
                if !resolved.keep_steam {
                    "\x1b[32mACTIVE (Daemon pages into swap/zRAM)\x1b[0m"
                } else {
                    "\x1b[33mDISABLED (Daemon stays resident)\x1b[0m"
                }
            );
            if let Some(ref w) = resolved.wrapper {
                println!("  Custom Wrapper:      {}", w);
            }
            if !resolved.extra_args.is_empty() {
                println!("  Resolved Args:       {}", resolved.extra_args.join(" "));
            }
            if let Some(ref lo) = resolved.localconfig_options {
                println!("  Steam localconfig:   \"{}\"", lo);
            }

            // Check Proton Prefix & Protontricks
            let prefix_dir = g
                .library_path
                .join("steamapps/compatdata")
                .join(g.appid.to_string());
            if prefix_dir.join("pfx").exists() {
                println!("\n\x1b[1mProton Environment:\x1b[0m");
                println!("  Prefix (compatdata): {}", prefix_dir.display());
                println!(
                    "  Protontricks Target: $ protontricks {} <command>",
                    g.appid
                );
            }

            // Check Runtime dependencies
            println!("\n\x1b[1mRuntime Dependencies:\x1b[0m");
            check_and_print_runtime_deps(g.appid, &g.library_path);
        }
        PlayableItem::NonSteam(ng) => {
            println!("\n\x1b[1;36m=== Non-Steam Game: {} ===\x1b[0m", ng.name);
            println!("  AppID:               {}", ng.appid);
            println!("  Executable:          {}", ng.exe.display());
            println!(
                "  Path Exists:         {}",
                if ng.is_path_missing {
                    "\x1b[31mNO (MOVED OR UNMOUNTED)\x1b[0m"
                } else {
                    "\x1b[32mYES\x1b[0m"
                }
            );
            if let Some(ref dir) = ng.start_dir {
                println!("  Start Directory:     {}", dir.display());
            }
            println!(
                "  Proton Required:     {}",
                if ng.use_proton { "Yes" } else { "No (Native)" }
            );
            if let Some(ref tool) = ng.compat_tool {
                println!("  Compatibility Tool:  {}", tool);
            }
            let prefix = get_compat_data_path_for_appid(ng.appid);
            println!(
                "  Prefix (compatdata): {} {}",
                prefix.display(),
                if prefix.exists() {
                    "\x1b[32m[EXISTS]\x1b[0m"
                } else {
                    "\x1b[33m[NOT INITIALIZED YET]\x1b[0m"
                }
            );
            println!(
                "  Protontricks Target: $ protontricks {} <command>",
                ng.appid
            );
            let appid_key = ng.appid.to_string();
            let resolved =
                config.resolve_launch_config(&appid_key, Some(ng.appid), None, false, &ng.args);
            println!("\n\x1b[1mConfiguration:\x1b[0m");
            println!(
                "  Overlay Active:      {}",
                if resolved.overlay {
                    "\x1b[32mENABLED\x1b[0m"
                } else {
                    "\x1b[33mSUPPRESSED\x1b[0m"
                }
            );
            if let Some(ref w) = resolved.wrapper {
                println!("  Custom Wrapper:      {}", w);
            }
            if !resolved.extra_args.is_empty() {
                println!("  Resolved Args:       {}", resolved.extra_args.join(" "));
            }
        }
    }
}

pub fn check_and_print_runtime_deps(appid: u32, library_path: &Path) {
    let steam_root = find_steam_root();
    let compat_tools = steam_root
        .as_ref()
        .map(|r| parse_compat_tool_mapping(r))
        .unwrap_or_default();

    if let Some(tool) = compat_tools.get(&appid) {
        println!("  Compatibility Tool:  {}", tool);
        return;
    }

    // Dynamic runtime container inspection: check if game directory requires Steam Linux Runtime
    let mut required_runtime: Option<(u32, &str, &str)> = None;
    let manifest_path = library_path.join(format!("steamapps/appmanifest_{appid}.acf"));
    if let Ok(m_content) = std::fs::read_to_string(&manifest_path) {
        if let Some(installdir) = extract_vdf_value(&m_content, "installdir") {
            let game_dir = library_path.join("steamapps/common").join(installdir);
            if game_dir.join("game/bin/linuxsteamrt64").exists()
                || game_dir.join("bin/linuxsteamrt64").exists()
            {
                required_runtime = Some((
                    1628350,
                    "Steam Linux Runtime 3.0 (sniper)",
                    "SteamLinuxRuntime_sniper",
                ));
            }
        }
    }

    if let Some((rt_id, rt_name, rt_folder)) = required_runtime {
        println!("  Required Container:  {} [AppID {}]", rt_name, rt_id);

        let mut found_path = None;
        let all_libs = get_all_library_folders();
        for lib in all_libs {
            let manifest = lib.join(format!("steamapps/appmanifest_{}.acf", rt_id));
            if manifest.exists() {
                let common_dir = lib.join(format!("steamapps/common/{}", rt_folder));
                let pv_wrap = common_dir.join("pressure-vessel/bin/pressure-vessel-wrap");
                found_path = Some((lib, manifest, pv_wrap));
                break;
            }
        }

        if let Some((lib, _, pv)) = found_path {
            match check_library_mount(&lib) {
                MountInfo::Mounted { mountpoint, .. } => {
                    println!(
                        "  Runtime Location:    \x1b[32m[MOUNTED]\x1b[0m on {}",
                        mountpoint.display()
                    );
                }
                MountInfo::Unmounted { label, device, .. } => {
                    println!(
                        "  Runtime Partition:   \x1b[31m[UNMOUNTED]\x1b[0m label '{}' ({})",
                        label,
                        device.display()
                    );
                }
                _ => {}
            }

            if pv.exists() {
                println!(
                    "  Runtime Binary:      \x1b[32m[VERIFIED]\x1b[0m {}",
                    pv.display()
                );
                println!("  Overlay Variable:    $PRESSURE_VESSEL_REMOVE_GAME_OVERLAY supported");
            } else {
                println!(
                    "  Runtime Binary:      \x1b[33m[NOT FOUND]\x1b[0m {}",
                    pv.display()
                );
            }
        } else {
            println!("  Runtime Status:      \x1b[33m[NOT DETECTED]\x1b[0m in scanned libraries");
        }
    } else {
        println!("  Compatibility:       Native or Standard Steam Runtime");
    }
}
