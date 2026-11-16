use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::error::VapourResult;
use crate::library::{find_steam_root, get_installed_games, home_dir};
use crate::non_steam::load_non_steam_games;

/// Summary of desktop entry generation results.
#[derive(Debug, Clone, Serialize, Default)]
pub struct DesktopSummary {
    pub created_or_updated: Vec<DesktopEntryRecord>,
    pub removed_stale: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesktopEntryRecord {
    pub appid: u32,
    pub name: String,
    pub path: PathBuf,
    pub icon_path: String,
}

/// Resolves the optimal icon path for an official Steam game or non-Steam shortcut.
pub fn resolve_game_icon(appid: u32, is_steam: bool, custom_icon: Option<&Path>) -> String {
    // 1. If a custom icon path is specified and exists on disk, use it
    if let Some(icon) = custom_icon {
        if icon.exists() {
            return icon.to_string_lossy().to_string();
        }
    }

    if is_steam {
        if let Some(root) = find_steam_root() {
            // Check Steam's modern librarycache (logo.png or icon.jpg)
            let cache_dir = root.join(format!("appcache/librarycache/{}", appid));
            for candidate in ["logo.png", "icon.jpg", "icon.png"] {
                let p = cache_dir.join(candidate);
                if p.exists() {
                    return p.to_string_lossy().to_string();
                }
            }
        }

        // Check system icon cache (~/.local/share/icons/hicolor/.../apps/steam_icon_<appid>.png)
        if let Some(home) = home_dir() {
            for res in ["256x256", "128x128", "64x64", "48x48", "32x32"] {
                let p = home.join(format!(
                    ".local/share/icons/hicolor/{}/apps/steam_icon_{}.png",
                    res, appid
                ));
                if p.exists() {
                    return p.to_string_lossy().to_string();
                }
            }
        }
    }

    // Default system fallback
    "applications-games".to_string()
}

/// Escapes a Desktop Entry string value (`\`, newline, tab, CR).
fn escape_desktop_value(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
}

/// Quotes an `Exec` program path per the spec when it contains spaces or reserved characters.
fn quote_exec_arg(path: &str) -> String {
    if path.chars().any(|c| " \t\n\"'\\><~|&;$*?#()`".contains(c)) {
        let mut out = String::from("\"");
        for c in path.chars() {
            if "\"`$\\".contains(c) {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out.replace('%', "%%")
    } else {
        path.replace('%', "%%")
    }
}

/// Formats the standard XDG .desktop file content.
pub fn format_desktop_entry(appid: u32, name: &str, icon: &str) -> String {
    // Absolute path: launchers do not necessarily share the user's shell PATH.
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "vapour".to_string());
    let name = escape_desktop_value(name);
    let exec = quote_exec_arg(&exe);
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Comment=Launch {name} with Vapour\n\
         Exec={exec} launch {appid}\n\
         Icon={icon}\n\
         Terminal=false\n\
         Categories=Game;\n\
         StartupNotify=false\n\
         X-Vapour-Managed=true\n"
    )
}

/// Resolves the target ~/.local/share/applications directory.
pub fn get_applications_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".local/share/applications"))
}

/// Generates .desktop files for all installed Steam and non-Steam games.
pub fn generate_desktop_entries(dry_run: bool, clean_stale: bool) -> VapourResult<DesktopSummary> {
    let mut summary = DesktopSummary::default();
    let Some(app_dir) = get_applications_dir() else {
        return Ok(summary);
    };

    if !dry_run {
        let _ = fs::create_dir_all(&app_dir);
    }

    let mut current_appids = std::collections::HashSet::new();
    let config = crate::config::VapourConfig::load();

    // 1. Process Official Steam Games
    if let Ok(games) = get_installed_games(false) {
        for game in games {
            current_appids.insert(game.appid);
            let icon = resolve_game_icon(game.appid, true, None);
            let content = format_desktop_entry(game.appid, &game.name, &icon);
            let file_path = app_dir.join(format!("vapour-steam-{}.desktop", game.appid));

            if !dry_run {
                let _ = crate::non_steam::write_atomic(&file_path, content.as_bytes());
            }

            summary.created_or_updated.push(DesktopEntryRecord {
                appid: game.appid,
                name: game.name,
                path: file_path,
                icon_path: icon,
            });
        }
    }

    // 2. Process Non-Steam Games
    for ns in load_non_steam_games(&config) {
        current_appids.insert(ns.appid);
        let icon = if let Some(parent) = ns.exe.parent() {
            let candidate_png = parent.join("icon.png");
            let candidate_ico = parent.join("icon.ico");
            if candidate_png.exists() {
                candidate_png.to_string_lossy().to_string()
            } else if candidate_ico.exists() {
                candidate_ico.to_string_lossy().to_string()
            } else {
                ns.exe.to_string_lossy().to_string()
            }
        } else {
            ns.exe.to_string_lossy().to_string()
        };

        let content = format_desktop_entry(ns.appid, &ns.name, &icon);
        let file_path = app_dir.join(format!("vapour-nonsteam-{}.desktop", ns.appid));

        if !dry_run {
            let _ = crate::non_steam::write_atomic(&file_path, content.as_bytes());
        }

        summary.created_or_updated.push(DesktopEntryRecord {
            appid: ns.appid,
            name: ns.name,
            path: file_path,
            icon_path: icon,
        });
    }

    // 3. Remove stale entries if requested
    if clean_stale {
        if let Ok(entries) = fs::read_dir(&app_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let file_name = entry.file_name().to_string_lossy().to_string();

                if file_name.starts_with("vapour-") && file_name.ends_with(".desktop") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        if content.contains("X-Vapour-Managed=true") {
                            let appid = extract_appid_from_desktop_filename(&file_name);
                            if let Some(id) = appid {
                                if !current_appids.contains(&id) {
                                    if !dry_run {
                                        let _ = fs::remove_file(&path);
                                    }
                                    summary.removed_stale.push(path);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 4. Update desktop database if available
    if !dry_run && !summary.created_or_updated.is_empty() {
        let _ = Command::new("update-desktop-database")
            .arg(&app_dir)
            .output();
    }

    Ok(summary)
}

/// Parses AppID from filenames like `vapour-steam-730.desktop` or `vapour-nonsteam-2165744007.desktop`.
pub fn extract_appid_from_desktop_filename(name: &str) -> Option<u32> {
    let without_ext = name.strip_suffix(".desktop")?;
    let parts: Vec<&str> = without_ext.split('-').collect();
    if parts.len() >= 3 {
        parts[2].parse::<u32>().ok()
    } else {
        None
    }
}

/// Prints a human-readable summary of desktop entry actions.
pub fn print_desktop_summary(summary: &DesktopSummary, dry_run: bool) {
    let action_str = if dry_run {
        "Generated (Dry Run)"
    } else {
        "Generated & Registered"
    };

    println!(
        "\x1b[1;32m[DESKTOP]\x1b[0m {} XDG Application Entries {}:",
        summary.created_or_updated.len(),
        action_str
    );
    println!();

    for entry in &summary.created_or_updated {
        println!(
            "  • AppID {:<8} {:<32} -> {}",
            entry.appid,
            entry.name,
            entry.path.display()
        );
    }

    if !summary.removed_stale.is_empty() {
        println!();
        println!(
            "\x1b[33m[CLEANUP]\x1b[0m Removed {} stale desktop entries:",
            summary.removed_stale.len()
        );
        for p in &summary.removed_stale {
            println!("  • Removed: {}", p.display());
        }
    }
    println!();
    println!(
        "Applications are now launchable via system menus (GNOME, KDE, Rofi, Fuzzel) with Vapour."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_desktop_entry_fields() {
        let content = format_desktop_entry(730, "Counter-Strike 2", "/path/to/icon.png");
        assert!(content.contains("[Desktop Entry]"));
        assert!(content.contains("Name=Counter-Strike 2"));
        assert!(content.contains(" launch 730\n"));
        assert!(content
            .lines()
            .any(|l| l.starts_with("Exec=") && !l.starts_with("Exec=vapour")));
        assert!(content.contains("StartupNotify=false"));
        assert!(content.contains("Icon=/path/to/icon.png"));
        assert!(content.contains("X-Vapour-Managed=true"));
    }

    #[test]
    fn escape_desktop_value_should_escape_backslash_and_control_chars() {
        assert_eq!(escape_desktop_value("a\\b\nc\td\re"), "a\\\\b\\nc\\td\\re");
    }

    #[test]
    fn quote_exec_arg_should_quote_only_when_needed() {
        assert_eq!(quote_exec_arg("/usr/bin/vapour"), "/usr/bin/vapour");
        assert_eq!(quote_exec_arg("/my apps/vapour"), "\"/my apps/vapour\"");
    }

    #[test]
    fn test_extract_appid_from_desktop_filename() {
        assert_eq!(
            extract_appid_from_desktop_filename("vapour-steam-730.desktop"),
            Some(730)
        );
        assert_eq!(
            extract_appid_from_desktop_filename("vapour-nonsteam-2165744007.desktop"),
            Some(2165744007)
        );
        assert_eq!(
            extract_appid_from_desktop_filename("other-app.desktop"),
            None
        );
    }
}
