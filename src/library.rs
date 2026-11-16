use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
    Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum SortOrder {
    #[default]
    Recent,
    Size,
    Name,
}

/// Returns the current Unix epoch timestamp in seconds.
pub fn current_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Formats a byte count into a human-readable string (e.g. 14.2 GB, 850.5 MB).
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Formats a past Unix epoch timestamp into a human-friendly relative duration.
pub fn format_relative_time(epoch_secs: u64, now_secs: u64) -> String {
    if epoch_secs == 0 {
        return "Never played".to_string();
    }
    if now_secs <= epoch_secs {
        return "Just now".to_string();
    }
    let delta = now_secs - epoch_secs;
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    const MONTH: u64 = 30 * DAY;
    const YEAR: u64 = 365 * DAY;

    if delta < MINUTE {
        "Just now".to_string()
    } else if delta < HOUR {
        let mins = delta / MINUTE;
        if mins == 1 {
            "1 min ago".to_string()
        } else {
            format!("{mins} mins ago")
        }
    } else if delta < DAY {
        let hrs = delta / HOUR;
        if hrs == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{hrs} hours ago")
        }
    } else if delta < MONTH {
        let days = delta / DAY;
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{days} days ago")
        }
    } else if delta < YEAR {
        let months = delta / MONTH;
        if months == 1 {
            "1 month ago".to_string()
        } else {
            format!("{months} months ago")
        }
    } else {
        let yrs = delta / YEAR;
        if yrs == 1 {
            "1 year ago".to_string()
        } else {
            format!("{yrs} years ago")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Game {
    pub appid: u32,
    pub name: String,
    pub installdir: String,
    pub library_path: PathBuf,
    pub size_on_disk: u64,
    pub last_played: u64,
    pub buildid: u64,
    pub is_unmounted: bool,
    pub unmounted_label: Option<String>,
}

/// Sorts a slice of games according to the specified SortOrder.
pub fn sort_games(games: &mut [Game], order: SortOrder) {
    match order {
        SortOrder::Recent => {
            games.sort_by(|a, b| {
                b.last_played
                    .cmp(&a.last_played)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        }
        SortOrder::Size => {
            games.sort_by(|a, b| {
                b.size_on_disk
                    .cmp(&a.size_on_disk)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        }
        SortOrder::Name => {
            games.sort_by_key(|a| a.name.to_lowercase());
        }
    }
}

impl fmt::Display for Game {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut meta = Vec::new();
        if self.size_on_disk > 0 {
            meta.push(format_bytes(self.size_on_disk));
        }
        if self.last_played > 0 {
            meta.push(format_relative_time(self.last_played, current_epoch_secs()));
        }

        let meta_tag = if meta.is_empty() {
            String::new()
        } else {
            format!(" [{}]", meta.join(" | "))
        };

        if self.is_unmounted {
            let label = self.unmounted_label.as_deref().unwrap_or("External");
            write!(
                f,
                "{:<36} (AppID: {}){meta_tag} [UNMOUNTED: {}]",
                self.name, self.appid, label
            )
        } else {
            write!(f, "{:<36} (AppID: {}){meta_tag}", self.name, self.appid)
        }
    }
}

/// Returns the user's home directory.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Locates Steam's base installation directory on Linux.
pub fn find_steam_root() -> Option<PathBuf> {
    let home = home_dir()?;
    let candidates = [
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".steam/root"),
    ];

    for path in &candidates {
        if path.join("steamapps/libraryfolders.vdf").exists() {
            return Some(path.clone());
        }
    }
    None
}

/// Parses libraryfolders.vdf to locate all Steam library directories.
pub fn parse_library_folders(vdf_content: &str) -> Vec<PathBuf> {
    let mut folders = Vec::new();
    for line in vdf_content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("\"path\"") {
            let parts: Vec<&str> = trimmed.split('"').collect();
            // Expected line: "path" "..." -> parts will be ["", "path", "   ", "<folder>", ...]
            if parts.len() >= 4 {
                let folder_str = parts[3];
                let path = PathBuf::from(folder_str);
                folders.push(path);
            }
        }
    }
    folders
}

/// Extracts a simple string key-value pair from a VDF/ACF file content.
pub fn extract_vdf_value<'a>(content: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(&needle) {
            let parts: Vec<&str> = trimmed.split('"').collect();
            if parts.len() >= 4 {
                return Some(parts[3]);
            }
        }
    }
    None
}

#[derive(Debug, Clone, Default)]
pub struct AppManifestState {
    pub state_flags: u32,
    pub bytes_to_download: u64,
    pub bytes_downloaded: u64,
    pub bytes_to_stage: u64,
    pub bytes_staged: u64,
}

impl AppManifestState {
    pub fn is_updating(&self) -> bool {
        // StateFlags bitmask:
        // 8 = Update Required, 16 = Update Queued, 32 = Update Running,
        // 128 = Update Started, 512 = Staging/Committing
        (self.state_flags & (8 | 16 | 32 | 128 | 512)) != 0
            || (self.bytes_to_download > 0 && self.bytes_downloaded < self.bytes_to_download)
            || (self.bytes_to_stage > 0 && self.bytes_staged < self.bytes_to_stage)
    }

    pub fn is_running(&self) -> bool {
        // 1024 = App Running
        (self.state_flags & 1024) != 0
    }
}

/// Reads the authoritative state of a game directly from its appmanifest on disk.
pub fn get_app_manifest_state(appid: u32) -> Option<AppManifestState> {
    for lib in get_all_library_folders() {
        let manifest_path = lib.join(format!("steamapps/appmanifest_{}.acf", appid));
        if manifest_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                let state_flags = extract_vdf_value(&content, "StateFlags")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                let bytes_to_download = extract_vdf_value(&content, "BytesToDownload")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let bytes_downloaded = extract_vdf_value(&content, "BytesDownloaded")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let bytes_to_stage = extract_vdf_value(&content, "BytesToStage")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let bytes_staged = extract_vdf_value(&content, "BytesStaged")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);

                return Some(AppManifestState {
                    state_flags,
                    bytes_to_download,
                    bytes_downloaded,
                    bytes_to_stage,
                    bytes_staged,
                });
            }
        }
    }
    None
}

/// Parses an appmanifest_<appid>.acf file into a Game struct if it is a playable game.
pub fn parse_appmanifest(path: &Path, library_path: &Path) -> Option<Game> {
    let content = fs::read_to_string(path).ok()?;
    let appid_str = extract_vdf_value(&content, "appid")?;
    let appid = appid_str.parse::<u32>().ok()?;
    let name = extract_vdf_value(&content, "name")?.to_string();
    let installdir = extract_vdf_value(&content, "installdir")
        .unwrap_or("")
        .to_string();

    let size_on_disk = extract_vdf_value(&content, "SizeOnDisk")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let last_played = extract_vdf_value(&content, "LastPlayed")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let buildid = extract_vdf_value(&content, "buildid")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    if is_blacklisted_utility(appid, &name) {
        return None;
    }

    Some(Game {
        appid,
        name,
        installdir,
        library_path: library_path.to_path_buf(),
        size_on_disk,
        last_played,
        buildid,
        is_unmounted: false,
        unmounted_label: None,
    })
}

/// Valve runtimes, Proton builds and redistributables. Single source of truth.
/// IDs checked against the community Proton appid guide
/// (steamcommunity.com/sharedfiles/filedetails/?id=2013973376) and Valve's steam-for-linux issues.
pub const VALVE_TOOLS: &[(u32, &str)] = &[
    (228980, "Steamworks Common Redistributables"),
    (1070560, "Steam Linux Runtime 1.0 (scout)"),
    (1391110, "Steam Linux Runtime 2.0 (soldier)"),
    (1628350, "Steam Linux Runtime 3.0 (sniper)"),
    (4183110, "Steam Linux Runtime 4.0"),
    (218010, "Steam Linux Runtime"),
    (1245040, "Proton 5.0"),
    (1580130, "Proton 6.3"),
    (1887720, "Proton 7.0"),
    (2348590, "Proton 8.0"),
    (2805730, "Proton 9.0"),
    (3658110, "Proton 10.0"),
    (4628710, "Proton 11.0"),
    (1493710, "Proton Experimental"),
    (2180100, "Proton Hotfix"),
];

/// Dedicated workshop tools / SDK components (not Valve runtimes).
const SDK_TOOL_IDS: &[u32] = &[2279720, 2279721];

pub fn valve_tool_name(appid: u32) -> Option<&'static str> {
    VALVE_TOOLS
        .iter()
        .find(|(id, _)| *id == appid)
        .map(|(_, n)| *n)
}

/// Returns true if an AppID or name belongs to a Steam runtime, tool, or non-game package.
pub fn is_blacklisted_utility(appid: u32, name: &str) -> bool {
    if valve_tool_name(appid).is_some() || SDK_TOOL_IDS.contains(&appid) {
        return true;
    }

    let lower = name.to_lowercase();
    lower.starts_with("steam linux runtime")
        || lower.starts_with("proton ")
        || lower.starts_with("steamworks common")
        || lower.contains("dedicated server")
}

use crate::mount::{
    check_library_mount, load_custom_libraries, mount_device_with_udisksctl, MountInfo,
};

#[derive(Debug, Clone)]
pub struct LibraryEntry {
    pub path: PathBuf,
    pub apps: Vec<u32>,
}

/// Parses libraryfolders.vdf into a list of LibraryEntry structs containing paths and appids.
pub fn parse_library_folders_full(vdf_content: &str) -> Vec<LibraryEntry> {
    let mut entries = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_apps = Vec::new();
    let mut in_apps_block = false;

    for line in vdf_content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("\"path\"") {
            let parts: Vec<&str> = trimmed.split('"').collect();
            if parts.len() >= 4 {
                if let Some(p) = current_path.take() {
                    entries.push(LibraryEntry {
                        path: p,
                        apps: std::mem::take(&mut current_apps),
                    });
                }
                current_path = Some(PathBuf::from(parts[3]));
            }
        } else if trimmed == "\"apps\"" {
            in_apps_block = true;
        } else if in_apps_block {
            if trimmed == "}" {
                in_apps_block = false;
            } else if trimmed.starts_with('"') {
                let parts: Vec<&str> = trimmed.split('"').collect();
                if parts.len() >= 4 {
                    if let Ok(appid) = parts[1].parse::<u32>() {
                        current_apps.push(appid);
                    }
                }
            }
        }
    }

    if let Some(p) = current_path {
        entries.push(LibraryEntry {
            path: p,
            apps: current_apps,
        });
    }

    entries
}

/// Loads persistent game names cache for displaying games when external drives are unmounted.
pub fn load_game_cache() -> std::collections::HashMap<u32, (String, String)> {
    let mut map = std::collections::HashMap::new();
    let Some(home) = home_dir() else { return map };
    let cache_file = home.join(".cache/vapour/games.txt");
    if let Ok(content) = fs::read_to_string(&cache_file) {
        for line in content.lines() {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 3 {
                if let Ok(appid) = parts[0].trim().parse::<u32>() {
                    map.insert(
                        appid,
                        (parts[1].trim().to_string(), parts[2].trim().to_string()),
                    );
                }
            }
        }
    }
    map
}

/// Saves persistent game names cache.
pub fn save_game_cache(cache: &std::collections::HashMap<u32, (String, String)>) {
    let Some(home) = home_dir() else { return };
    let dir = home.join(".cache/vapour");
    let _ = fs::create_dir_all(&dir);
    let cache_file = dir.join("games.txt");
    let mut lines = Vec::new();
    for (appid, (name, installdir)) in cache {
        lines.push(format!("{}|{}|{}", appid, name, installdir));
    }
    let _ = crate::non_steam::write_atomic(&cache_file, lines.join("\n").as_bytes());
}

/// Discovers all Steam library paths (Steam root + libraryfolders.vdf + custom libraries).
/// Canonicalizes paths to prevent duplicates caused by symlinks (~/.steam/steam vs ~/.local/share/Steam).
pub fn get_all_library_folders() -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    if let Some(steam_root) = find_steam_root() {
        let library_vdf = steam_root.join("steamapps/libraryfolders.vdf");
        if let Ok(content) = fs::read_to_string(&library_vdf) {
            for folder in parse_library_folders(&content) {
                let canon = fs::canonicalize(&folder).unwrap_or(folder);
                if !libraries.contains(&canon) {
                    libraries.push(canon);
                }
            }
        }
        let root_canon = fs::canonicalize(&steam_root).unwrap_or(steam_root);
        if !libraries.contains(&root_canon) {
            libraries.push(root_canon);
        }
    }

    // Merge custom registered libraries
    for custom in load_custom_libraries() {
        let canon = fs::canonicalize(&custom).unwrap_or(custom);
        if !libraries.contains(&canon) {
            libraries.push(canon);
        }
    }

    libraries
}

/// Scans all configured Steam libraries and returns a list of installed games.
/// Non-blocking: Games on unmounted external drives are marked with `is_unmounted = true`
/// so the user is only prompted if they choose to launch that specific game.
pub fn get_installed_games(interactive_mount: bool) -> Result<Vec<Game>, String> {
    let libraries = get_all_library_folders();
    if libraries.is_empty() {
        return Err("Could not locate Steam installation directory or libraries".into());
    }

    let mut cache = load_game_cache();
    let mut games = Vec::new();

    let mut lib_entries = Vec::new();
    if let Some(steam_root) = find_steam_root() {
        let library_vdf = steam_root.join("steamapps/libraryfolders.vdf");
        if let Ok(content) = fs::read_to_string(&library_vdf) {
            lib_entries = parse_library_folders_full(&content);
        }
    }

    for lib in libraries {
        let status = check_library_mount(&lib);
        match status {
            MountInfo::Mounted { .. } => {
                let steamapps = lib.join("steamapps");
                let entries = match fs::read_dir(&steamapps) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(file_name) = path.file_name().and_then(|f| f.to_str()) {
                        if file_name.starts_with("appmanifest_") && file_name.ends_with(".acf") {
                            if let Some(game) = parse_appmanifest(&path, &lib) {
                                cache.insert(
                                    game.appid,
                                    (game.name.clone(), game.installdir.clone()),
                                );
                                games.push(game);
                            }
                        }
                    }
                }
            }
            MountInfo::Unmounted { label, device, .. } => {
                if interactive_mount {
                    let confirm = inquire::Confirm::new(&format!(
                        "Library drive '{}' ({}) is unmounted. Mount now via udisksctl?",
                        label,
                        lib.display()
                    ))
                    .with_default(false)
                    .prompt()
                    .unwrap_or(false);

                    if confirm && mount_device_with_udisksctl(&device, true).is_ok() {
                        let steamapps = lib.join("steamapps");
                        if let Ok(entries) = fs::read_dir(&steamapps) {
                            for entry in entries.flatten() {
                                let path = entry.path();
                                if let Some(file_name) = path.file_name().and_then(|f| f.to_str()) {
                                    if file_name.starts_with("appmanifest_")
                                        && file_name.ends_with(".acf")
                                    {
                                        if let Some(game) = parse_appmanifest(&path, &lib) {
                                            cache.insert(
                                                game.appid,
                                                (game.name.clone(), game.installdir.clone()),
                                            );
                                            games.push(game);
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                    }
                }

                // In non-blocking mode or if declined: list games from libraryfolders.vdf marked as unmounted
                let matching_apps = lib_entries
                    .iter()
                    .find(|entry| {
                        entry.path == lib
                            || fs::canonicalize(&entry.path)
                                .map(|p| p == lib)
                                .unwrap_or(false)
                    })
                    .map(|entry| entry.apps.clone())
                    .unwrap_or_default();

                for appid in matching_apps {
                    if is_blacklisted_utility(appid, "") {
                        continue;
                    }
                    let (name, installdir) = cache
                        .get(&appid)
                        .cloned()
                        .unwrap_or_else(|| (format!("Steam App {}", appid), String::new()));
                    games.push(Game {
                        appid,
                        name,
                        installdir,
                        library_path: lib.clone(),
                        size_on_disk: 0,
                        last_played: 0,
                        buildid: 0,
                        is_unmounted: true,
                        unmounted_label: Some(label.clone()),
                    });
                }
            }
            MountInfo::DirectoryMissing { .. } => {
                continue;
            }
        }
    }

    save_game_cache(&cache);

    games.sort_by_key(|a| a.name.to_lowercase());
    games.dedup_by_key(|g| g.appid);
    sort_games(&mut games, SortOrder::Recent);

    Ok(games)
}

/// Configures installed Steam games to update only on launch ("AutoUpdateBehavior" "1").
/// This stops Steam from auto-downloading large background updates for other games on startup.
pub fn configure_on_demand_updates() -> usize {
    let libraries = get_all_library_folders();
    let mut updated_count = 0;

    for lib in libraries {
        let steamapps = lib.join("steamapps");
        let Ok(entries) = fs::read_dir(&steamapps) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(file_name) = path.file_name().and_then(|f| f.to_str()) {
                if file_name.starts_with("appmanifest_") && file_name.ends_with(".acf") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        if content.contains("\"AutoUpdateBehavior\"\t\t\"0\"")
                            || content.contains("\"AutoUpdateBehavior\" \"0\"")
                            || content.contains("\"AutoUpdateBehavior\"\t\"0\"")
                        {
                            let new_content = content
                                .replace(
                                    "\"AutoUpdateBehavior\"\t\t\"0\"",
                                    "\"AutoUpdateBehavior\"\t\t\"1\"",
                                )
                                .replace(
                                    "\"AutoUpdateBehavior\" \"0\"",
                                    "\"AutoUpdateBehavior\" \"1\"",
                                )
                                .replace(
                                    "\"AutoUpdateBehavior\"\t\"0\"",
                                    "\"AutoUpdateBehavior\"\t\"1\"",
                                );
                            if crate::non_steam::write_atomic(&path, new_content.as_bytes()).is_ok()
                            {
                                updated_count += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    updated_count
}

/// Parses localconfig.vdf text content and extracts the native Steam LaunchOptions for a given AppID.
pub fn parse_localconfig_launch_options(content: &str, appid: u32) -> Option<String> {
    let target_app = format!("\"{}\"", appid);
    let mut in_apps_section = false;
    let mut apps_depth: i32 = 0;

    let mut in_target_app = false;
    let mut target_depth: i32 = 0;

    for line in content.lines() {
        let trimmed = line.trim();

        if !in_apps_section {
            if trimmed.eq_ignore_ascii_case("\"apps\"") || trimmed.eq_ignore_ascii_case("\"Apps\"")
            {
                in_apps_section = true;
                apps_depth = if trimmed.ends_with('{') { 1 } else { 0 };
            }
            continue;
        }

        let open_count = trimmed.chars().filter(|&c| c == '{').count() as i32;
        let close_count = trimmed.chars().filter(|&c| c == '}').count() as i32;

        if !in_target_app {
            apps_depth += open_count - close_count;
            if apps_depth <= 0 && (open_count > 0 || close_count > 0) {
                in_apps_section = false;
                continue;
            }

            if trimmed == target_app
                || trimmed.starts_with(&format!("{} ", target_app))
                || trimmed.starts_with(&format!("{}\t", target_app))
            {
                in_target_app = true;
                target_depth = if trimmed.contains('{') { 1 } else { 0 };
            }
        } else {
            target_depth += open_count - close_count;
            if target_depth <= 0 && (open_count > 0 || close_count > 0) {
                in_target_app = false;
                continue;
            }

            if trimmed.starts_with("\"LaunchOptions\"") {
                let parts: Vec<&str> = trimmed.split('"').collect();
                if parts.len() >= 4 {
                    let val = parts[3].trim().to_string();
                    if !val.is_empty() {
                        return Some(val);
                    }
                }
            }
        }
    }

    None
}

/// Resolves native Steam LaunchOptions for an AppID by reading localconfig.vdf from active Steam userdata directories.
pub fn get_steam_localconfig_launch_options(appid: u32) -> Option<String> {
    for user_dir in crate::non_steam::get_active_user_data_dirs() {
        let localconfig_path = user_dir.join("config/localconfig.vdf");
        if let Ok(content) = fs::read_to_string(&localconfig_path) {
            if let Some(opts) = parse_localconfig_launch_options(&content, appid) {
                return Some(opts);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn valve_tools_should_have_unique_ids() {
        let mut ids: Vec<u32> = super::VALVE_TOOLS.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), super::VALVE_TOOLS.len());
    }

    use super::*;

    #[test]
    fn test_extract_vdf_value() {
        let sample = r#"
"AppState"
{
	"appid"		"480"
	"name"		"SpaceWar"
	"installdir"		"SpaceWar"
}
"#;
        assert_eq!(extract_vdf_value(sample, "appid"), Some("480"));
        assert_eq!(extract_vdf_value(sample, "name"), Some("SpaceWar"));
        assert_eq!(extract_vdf_value(sample, "installdir"), Some("SpaceWar"));
        assert_eq!(extract_vdf_value(sample, "nonexistent"), None);
    }

    #[test]
    fn test_blacklist_filters_runtimes() {
        assert!(is_blacklisted_utility(
            228980,
            "Steamworks Common Redistributables"
        ));
        assert!(is_blacklisted_utility(
            1628350,
            "Steam Linux Runtime 3.0 (sniper)"
        ));
        assert!(is_blacklisted_utility(999999, "Proton Hotfix"));
        assert!(!is_blacklisted_utility(480, "SpaceWar"));
    }

    #[test]
    fn test_parse_library_folders_full() {
        let sample = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/home/user/.local/share/Steam"
		"apps"
		{
		}
	}
	"1"
	{
		"path"		"/mnt/games/SteamLibrary"
		"apps"
		{
			"730"		"73418336548"
			"480"		"100000"
		}
	}
}
"#;
        let entries = parse_library_folders_full(sample);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].path,
            PathBuf::from("/home/user/.local/share/Steam")
        );
        assert!(entries[0].apps.is_empty());
        assert_eq!(entries[1].path, PathBuf::from("/mnt/games/SteamLibrary"));
        assert_eq!(entries[1].apps, vec![730, 480]);
    }

    #[test]
    fn test_parse_localconfig_launch_options() {
        let sample = r#"
"UserLocalConfigStore"
{
	"Software"
	{
		"Valve"
		{
			"Steam"
			{
				"apps"
				{
					"550"
					{
						"LastPlayed"		"1689640730"
					}
					"730"
					{
						"LastPlayed"		"1793613055"
						"LaunchOptions"		"gamemoderun gamescope -W 1920 -- %command% -novid"
					}
				}
			}
		}
	}
}
"#;
        assert_eq!(
            parse_localconfig_launch_options(sample, 730),
            Some("gamemoderun gamescope -W 1920 -- %command% -novid".to_string())
        );
        assert_eq!(parse_localconfig_launch_options(sample, 550), None);
        assert_eq!(parse_localconfig_launch_options(sample, 9999), None);
    }

    #[test]
    fn test_live_localconfig_read_cs2() {
        if let Some(opts) = get_steam_localconfig_launch_options(730) {
            assert!(opts.contains("gamemoderun"));
            assert!(opts.contains("gamescope"));
        }
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10.0 MB");
        assert_eq!(format_bytes(15 * 1024 * 1024 * 1024), "15.0 GB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024 * 1024), "2.00 TB");
    }

    #[test]
    fn test_format_relative_time() {
        assert_eq!(format_relative_time(0, 1000), "Never played");
        assert_eq!(format_relative_time(1000, 1000), "Just now");
        assert_eq!(format_relative_time(1000, 1030), "Just now");
        assert_eq!(format_relative_time(1000, 1060), "1 min ago");
        assert_eq!(format_relative_time(1000, 1120), "2 mins ago");
        assert_eq!(format_relative_time(1000, 1000 + 3600), "1 hour ago");
        assert_eq!(format_relative_time(1000, 1000 + 7200), "2 hours ago");
        assert_eq!(format_relative_time(1000, 1000 + 86400), "1 day ago");
        assert_eq!(format_relative_time(1000, 1000 + 86400 * 5), "5 days ago");
        assert_eq!(format_relative_time(1000, 1000 + 86400 * 45), "1 month ago");
        assert_eq!(format_relative_time(1000, 1000 + 86400 * 400), "1 year ago");
    }

    #[test]
    fn test_sort_games() {
        let g1 = Game {
            appid: 1,
            name: "Alpha Game".to_string(),
            installdir: "alpha".to_string(),
            library_path: PathBuf::from("/lib"),
            size_on_disk: 1000,
            last_played: 10,
            buildid: 1,
            is_unmounted: false,
            unmounted_label: None,
        };
        let g2 = Game {
            appid: 2,
            name: "Beta Game".to_string(),
            installdir: "beta".to_string(),
            library_path: PathBuf::from("/lib"),
            size_on_disk: 5000,
            last_played: 50,
            buildid: 2,
            is_unmounted: false,
            unmounted_label: None,
        };
        let g3 = Game {
            appid: 3,
            name: "Gamma Game".to_string(),
            installdir: "gamma".to_string(),
            library_path: PathBuf::from("/lib"),
            size_on_disk: 3000,
            last_played: 30,
            buildid: 3,
            is_unmounted: false,
            unmounted_label: None,
        };

        let mut list = vec![g1.clone(), g2.clone(), g3.clone()];
        sort_games(&mut list, SortOrder::Recent);
        assert_eq!(list[0].appid, 2); // 50
        assert_eq!(list[1].appid, 3); // 30
        assert_eq!(list[2].appid, 1); // 10

        sort_games(&mut list, SortOrder::Size);
        assert_eq!(list[0].appid, 2); // 5000
        assert_eq!(list[1].appid, 3); // 3000
        assert_eq!(list[2].appid, 1); // 1000

        sort_games(&mut list, SortOrder::Name);
        assert_eq!(list[0].appid, 1); // Alpha
        assert_eq!(list[1].appid, 2); // Beta
        assert_eq!(list[2].appid, 3); // Gamma
    }
}
