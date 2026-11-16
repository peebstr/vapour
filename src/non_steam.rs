use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::VapourConfig;
use crate::error::{VapourError, VapourResult};
use crate::library::{find_steam_root, get_all_library_folders, home_dir};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityTool {
    pub name: String,
    pub proton_bin: PathBuf,
}

impl CompatibilityTool {
    /// Returns true if this Proton runner supports modern pure WoW64 mode
    /// (Proton 9.0+, Proton 10.0+, GE-Proton 9+, or CachyOS Proton).
    pub fn is_wow64(&self) -> bool {
        let lower = self.name.to_lowercase();
        if lower.contains("proton 9")
            || lower.contains("proton 10")
            || lower.contains("proton-9")
            || lower.contains("proton-10")
            || lower.contains("ge-proton9")
            || lower.contains("ge-proton10")
            || lower.contains("cachyos")
            || lower.contains("experimental")
        {
            return true;
        }

        if let Some(parent) = self.proton_bin.parent() {
            if parent.join("files/bin/wine64").exists() {
                return true;
            }
        }
        false
    }
}

/// Discovers all available Proton installations in Steam libraries and compatibility directories.
pub fn find_available_protons() -> Vec<CompatibilityTool> {
    let mut tools: Vec<CompatibilityTool> = Vec::new();

    // 1. Check custom and system compatibility tools (e.g. GE-Proton, Proton-CachyOS)
    let mut candidate_dirs = vec![
        PathBuf::from("/usr/share/steam/compatibilitytools.d"),
        PathBuf::from("/usr/local/share/steam/compatibilitytools.d"),
    ];
    if let Some(home) = home_dir() {
        candidate_dirs.push(home.join(".local/share/Steam/compatibilitytools.d"));
        candidate_dirs.push(home.join(".steam/root/compatibilitytools.d"));
        candidate_dirs.push(home.join(".steam/steam/compatibilitytools.d"));
    }

    for candidate_dir in candidate_dirs {
        if let Ok(entries) = fs::read_dir(candidate_dir) {
            for entry in entries.flatten() {
                let proton_bin = entry.path().join("proton");
                if proton_bin.is_file() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !tools.iter().any(|t| t.proton_bin == proton_bin) {
                        tools.push(CompatibilityTool { name, proton_bin });
                    }
                }
            }
        }
    }

    // 2. Check installed Valve Protons in all Steam libraries
    for lib in get_all_library_folders() {
        let common = lib.join("steamapps/common");
        if let Ok(entries) = fs::read_dir(common) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("Proton") {
                    let proton_bin = entry.path().join("proton");
                    if proton_bin.is_file() && !tools.iter().any(|t| t.proton_bin == proton_bin) {
                        tools.push(CompatibilityTool { name, proton_bin });
                    }
                }
            }
        }
    }

    // Sort descending so newer/GE Protons appear first
    tools.sort_by(|a, b| b.name.cmp(&a.name));
    tools
}

/// Computes the Steam Non-Steam AppID: (CRC32(target + name) | 0x80000000)
pub fn calculate_steam_shortcut_appid(target: &str, name: &str) -> u32 {
    let mut data = Vec::with_capacity(target.len() + name.len());
    data.extend_from_slice(target.as_bytes());
    data.extend_from_slice(name.as_bytes());

    let mut crc: u32 = 0xFFFFFFFF;
    for byte in data {
        let mut b = (crc ^ (byte as u32)) & 0xFF;
        for _ in 0..8 {
            if (b & 1) != 0 {
                b = (b >> 1) ^ 0xEDB88320;
            } else {
                b >>= 1;
            }
        }
        crc = (crc >> 8) ^ b;
    }
    let crc_final = !crc;
    crc_final | 0x80000000
}

/// Helper to strip wrapping quotes from VDF strings
pub fn strip_quotes(s: &str) -> String {
    s.trim().trim_matches('"').trim().to_string()
}

/// Atomically writes content to path using a temporary file and rename to avoid corrupting VDF files.
pub fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension(format!("tmp.{}", std::process::id()));
    let write_synced = || -> std::io::Result<()> {
        use std::io::Write;
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(content)?;
        f.sync_all()
    };
    if let Err(e) = write_synced() {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

/// Parses loginusers.vdf to locate the currently active Steam account (MostRecent = 1 or largest timestamp).
pub fn parse_active_account_id_from_loginusers(content: &str) -> Option<u32> {
    let mut current_steamid: Option<u64> = None;
    let mut current_timestamp: u64 = 0;
    let mut is_most_recent = false;

    let mut best_steamid: Option<u64> = None;
    let mut best_timestamp: u64 = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('"') {
            let parts: Vec<&str> = trimmed
                .split('"')
                .filter(|s| !s.trim().is_empty())
                .collect();
            if let Some(first) = parts.first() {
                if let Ok(id) = first.parse::<u64>() {
                    if is_most_recent {
                        if let Some(sid) = current_steamid {
                            return Some((sid & 0xFFFFFFFF) as u32);
                        }
                    }
                    if current_timestamp > best_timestamp {
                        best_timestamp = current_timestamp;
                        best_steamid = current_steamid;
                    }
                    current_steamid = Some(id);
                    current_timestamp = 0;
                    is_most_recent = false;
                    continue;
                }
            }
        }
        if trimmed.starts_with("\"MostRecent\"") {
            let parts: Vec<&str> = trimmed.split('"').collect();
            if parts.len() >= 4 && parts[3] == "1" {
                is_most_recent = true;
            }
        } else if trimmed.starts_with("\"timestamp\"") {
            let parts: Vec<&str> = trimmed.split('"').collect();
            if parts.len() >= 4 {
                if let Ok(ts) = parts[3].parse::<u64>() {
                    current_timestamp = ts;
                }
            }
        }
    }

    if is_most_recent {
        if let Some(sid) = current_steamid {
            return Some((sid & 0xFFFFFFFF) as u32);
        }
    }
    if current_timestamp > best_timestamp {
        best_steamid = current_steamid;
    }

    best_steamid.map(|sid| (sid & 0xFFFFFFFF) as u32)
}

/// Finds the target Steam userdata directories. Prioritizes the active user from loginusers.vdf.
pub fn get_active_user_data_dirs() -> Vec<PathBuf> {
    let steam_root = match find_steam_root() {
        Some(r) => r,
        None => return Vec::new(),
    };
    let userdata = steam_root.join("userdata");
    if !userdata.is_dir() {
        return Vec::new();
    }

    let loginusers_path = steam_root.join("config/loginusers.vdf");
    if let Ok(content) = fs::read_to_string(&loginusers_path) {
        if let Some(active_account_id) = parse_active_account_id_from_loginusers(&content) {
            let active_dir = userdata.join(active_account_id.to_string());
            if active_dir.is_dir() {
                return vec![active_dir];
            }
        }
    }

    // Fallback: collect all non-0 numeric directories in userdata
    let mut dirs = Vec::new();
    if let Ok(entries) = fs::read_dir(&userdata) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name != "0" && name.chars().all(|c| c.is_ascii_digit()) && entry.path().is_dir() {
                dirs.push(entry.path());
            }
        }
    }
    if dirs.is_empty() {
        let zero = userdata.join("0");
        if zero.is_dir() {
            dirs.push(zero);
        }
    }
    dirs
}

/// Represents a raw shortcut extracted from Steam's binary shortcuts.vdf
#[derive(Debug, Clone, Default)]
pub struct RawShortcut {
    pub appid: Option<u32>,
    pub app_name: String,
    pub exe: String,
    pub start_dir: String,
    pub launch_options: String,
    pub is_hidden: bool,
}

/// Validates basic header tag structure of binary shortcuts.vdf data.
pub fn validate_shortcuts_vdf(data: &[u8]) -> VapourResult<()> {
    if data.is_empty() {
        return Ok(());
    }
    if data[0] != 0x00 {
        return Err(VapourError::VdfCorruption {
            offset: 0,
            reason: format!("Expected root byte 0x00, found 0x{:02x}", data[0]),
        });
    }
    Ok(())
}

/// One `key = value` field inside a shortcut entry.
struct Field {
    kind: u8,
    /// Lowercased key.
    key: String,
    /// Byte range of the value (string text without its NUL, or the raw 4/8 int bytes).
    value: std::ops::Range<usize>,
}

/// One shortcut entry (`0x00 <index> 0x00 fields... 0x08`).
struct Entry {
    /// Byte range after the index string through the closing 0x08 (what delete re-emits).
    body: std::ops::Range<usize>,
    /// Offset of the closing 0x08, or the body start if the entry is truncated.
    end_marker: usize,
    appid: Option<u32>,
    fields: Vec<Field>,
}

/// Reads a NUL-terminated string at `at`: (text range, offset after the NUL, was terminated).
fn read_cstr(data: &[u8], at: usize) -> (std::ops::Range<usize>, usize, bool) {
    let at = at.min(data.len());
    match data[at..].iter().position(|&b| b == 0) {
        Some(n) => (at..at + n, at + n + 1, true),
        None => (at..data.len(), data.len(), false),
    }
}

/// Skips a nested dict whose opening tag and key were already consumed; returns the offset after its 0x08.
fn skip_nested(data: &[u8], mut at: usize) -> usize {
    let mut depth = 1;
    while at < data.len() && depth > 0 {
        let tag = data[at];
        at += 1;
        match tag {
            0x08 => depth -= 1,
            0x00 => {
                depth += 1;
                at = read_cstr(data, at).1;
            }
            0x01 => {
                at = read_cstr(data, read_cstr(data, at).1).1;
            }
            0x02 => at = (read_cstr(data, at).1 + 4).min(data.len()),
            0x07 => at = (read_cstr(data, at).1 + 8).min(data.len()),
            _ => {}
        }
    }
    at
}

/// Walks a binary shortcuts.vdf: returns (offset of first entry, entries, offset where entries stopped).
/// `None` if the root is not a `shortcuts` map. A truncated last entry is still returned.
fn walk_entries(data: &[u8]) -> Option<(usize, Vec<Entry>, usize)> {
    if data.first() != Some(&0x00) {
        return None;
    }
    let (name, mut cur, terminated) = read_cstr(data, 1);
    if !terminated || !data[name].eq_ignore_ascii_case(b"shortcuts") {
        return None;
    }
    let first = cur;
    let mut entries = Vec::new();

    while cur < data.len() && data[cur] == 0x00 {
        cur = read_cstr(data, cur + 1).1; // skip entry index
        let body_start = cur;
        let mut entry = Entry {
            body: body_start..body_start,
            end_marker: body_start,
            appid: None,
            fields: Vec::new(),
        };

        while cur < data.len() {
            let kind = data[cur];
            cur += 1;
            if kind == 0x08 {
                entry.end_marker = cur - 1;
                break;
            }
            let (key_range, next, terminated) = read_cstr(data, cur);
            cur = next;
            if !terminated {
                break;
            }
            let key = String::from_utf8_lossy(&data[key_range]).to_lowercase();
            let value = match kind {
                0x00 => {
                    let start = cur;
                    cur = skip_nested(data, cur);
                    start..cur
                }
                0x01 => {
                    let (r, next, _) = read_cstr(data, cur);
                    cur = next;
                    r
                }
                0x02 if cur + 4 <= data.len() => {
                    let r = cur..cur + 4;
                    if key == "appid" {
                        entry.appid = Some(u32::from_le_bytes(data[r.clone()].try_into().ok()?));
                    }
                    cur += 4;
                    r
                }
                0x02 => cur..cur,
                0x07 => {
                    let end = (cur + 8).min(data.len());
                    let r = cur..end;
                    cur = end;
                    r
                }
                _ => break,
            };
            entry.fields.push(Field { kind, key, value });
        }

        entry.body.end = cur;
        entries.push(entry);
    }

    Some((first, entries, cur))
}

/// Parses Steam's binary shortcuts.vdf format.
/// VDF Binary Format:
/// - 0x00: Map / Nested Object (null-terminated key name, values until 0x08)
/// - 0x01: String (null-terminated key name, null-terminated string value)
/// - 0x02: Int32 (null-terminated key name, 4 bytes little-endian)
/// - 0x07: Int64 (null-terminated key name, 8 bytes little-endian)
/// - 0x08: End of current object
pub fn parse_shortcuts_vdf(data: &[u8]) -> Vec<RawShortcut> {
    let Some((_, entries, _)) = walk_entries(data) else {
        return Vec::new();
    };

    let mut shortcuts = Vec::new();
    for entry in entries {
        let mut shortcut = RawShortcut::default();
        for f in &entry.fields {
            let raw = &data[f.value.clone()];
            match (f.kind, f.key.as_str()) {
                (0x01, "appname") => shortcut.app_name = String::from_utf8_lossy(raw).to_string(),
                (0x01, "exe") => shortcut.exe = strip_quotes(&String::from_utf8_lossy(raw)),
                (0x01, "startdir") => {
                    shortcut.start_dir = strip_quotes(&String::from_utf8_lossy(raw))
                }
                (0x01, "launchoptions") => {
                    shortcut.launch_options = String::from_utf8_lossy(raw).to_string()
                }
                (0x02, "appid") => shortcut.appid = entry.appid,
                (0x02, "ishidden") => shortcut.is_hidden = raw.iter().any(|&b| b != 0),
                _ => {}
            }
        }
        if !shortcut.app_name.is_empty() || !shortcut.exe.is_empty() {
            shortcuts.push(shortcut);
        }
    }
    shortcuts
}

/// Modifies binary shortcuts.vdf data in-place to update the Exe, StartDir, and/or LaunchOptions for target_appid.
/// Preserves all original tags, metadata, icons, and binary order.
pub fn update_shortcut_fields_binary_vdf(
    data: &[u8],
    target_appid: u32,
    new_exe: Option<&str>,
    new_start_dir: Option<&str>,
    new_launch_options: Option<&str>,
) -> Option<Vec<u8>> {
    let (_, entries, _) = walk_entries(data)?;
    let entry = entries.iter().find(|e| e.appid == Some(target_appid))?;

    // Last string field with this key wins, like the original sequential scan.
    let string_field = |key: &str| {
        entry
            .fields
            .iter()
            .rev()
            .find(|f| f.kind == 0x01 && f.key == key)
            .map(|f| f.value.clone())
    };
    let quoted = |v: &str| {
        if v.starts_with('"') && v.ends_with('"') {
            v.to_string()
        } else {
            format!("\"{v}\"")
        }
    };

    let mut replacements: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    if let (Some(v), Some(r)) = (new_exe, string_field("exe")) {
        replacements.push((r.start, r.end, quoted(v).into_bytes()));
    }
    if let (Some(v), Some(r)) = (new_start_dir, string_field("startdir")) {
        replacements.push((r.start, r.end, quoted(v).into_bytes()));
    }
    if let Some(opts) = new_launch_options {
        if let Some(r) = string_field("launchoptions") {
            replacements.push((r.start, r.end, opts.as_bytes().to_vec()));
        } else {
            let mut ins = vec![0x01];
            ins.extend_from_slice(b"LaunchOptions\x00");
            ins.extend_from_slice(opts.as_bytes());
            ins.push(0x00);
            replacements.push((entry.end_marker, entry.end_marker, ins));
        }
    }

    // Invariant: splice from the bottom so earlier offsets stay valid.
    replacements.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut result = data.to_vec();
    for (start, end, new_bytes) in replacements {
        result.splice(start..end, new_bytes);
    }
    Some(result)
}

/// Reads a shortcuts.vdf; `Ok(None)` when it does not exist.
fn read_shortcuts_file(path: &Path) -> VapourResult<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(d) => Ok(Some(d)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(VapourError::Storage(format!(
            "Failed to read {}: {e}",
            path.display()
        ))),
    }
}

/// Backs up `old` (if any) then atomically writes `new`. No backup means no write.
fn write_shortcuts_with_backup(path: &Path, old: &[u8], new: &[u8]) -> VapourResult<()> {
    if !old.is_empty() {
        let bak = path.with_extension("vdf.bak");
        fs::write(&bak, old).map_err(|e| {
            VapourError::Storage(format!("Failed to write backup {}: {e}", bak.display()))
        })?;
    }
    write_atomic(path, new)
        .map_err(|e| VapourError::Storage(format!("Failed to write {}: {e}", path.display())))
}

/// Updates the LaunchOptions for a shortcut AppID in all userdata shortcuts.vdf files.
pub fn update_shortcut_launch_options_in_all_vdf(
    appid: u32,
    new_launch_options: &str,
) -> VapourResult<bool> {
    let mut updated_any = false;

    for user_dir in get_active_user_data_dirs() {
        let shortcuts_path = user_dir.join("config/shortcuts.vdf");
        let Some(data) = read_shortcuts_file(&shortcuts_path)? else {
            continue;
        };
        if let Some(new_data) =
            update_shortcut_fields_binary_vdf(&data, appid, None, None, Some(new_launch_options))
        {
            write_shortcuts_with_backup(&shortcuts_path, &data, &new_data)?;
            updated_any = true;
        }
    }

    Ok(updated_any)
}

/// Updates the Exe and StartDir path for a shortcut AppID in all userdata shortcuts.vdf files.
pub fn update_shortcut_in_all_vdf(
    appid: u32,
    new_exe: &Path,
    new_start_dir: Option<&Path>,
) -> VapourResult<bool> {
    let mut updated_any = false;
    let exe_str = new_exe.to_string_lossy();
    let start_dir_str = new_start_dir
        .map(|p| p.to_string_lossy().to_string())
        .or_else(|| new_exe.parent().map(|p| p.to_string_lossy().to_string()))
        .unwrap_or_default();

    for user_dir in get_active_user_data_dirs() {
        let shortcuts_path = user_dir.join("config/shortcuts.vdf");
        let Some(data) = read_shortcuts_file(&shortcuts_path)? else {
            continue;
        };
        if let Some(new_data) = update_shortcut_fields_binary_vdf(
            &data,
            appid,
            Some(&exe_str),
            Some(&start_dir_str),
            None,
        ) {
            write_shortcuts_with_backup(&shortcuts_path, &data, &new_data)?;
            updated_any = true;
        }
    }

    Ok(updated_any)
}

/// Serializes a single shortcut into Steam binary VDF entry format.
pub fn serialize_shortcut_entry(
    index: usize,
    appid: u32,
    name: &str,
    exe: &str,
    start_dir: &str,
    launch_options: &str,
) -> Vec<u8> {
    let mut buf = Vec::new();
    // 0x00 <index> \x00
    buf.push(0x00);
    buf.extend_from_slice(index.to_string().as_bytes());
    buf.push(0x00);

    // 0x02 appid \x00 <4 bytes>
    buf.push(0x02);
    buf.extend_from_slice(b"appid\x00");
    buf.extend_from_slice(&appid.to_le_bytes());

    // 0x01 AppName \x00 <name> \x00
    buf.push(0x01);
    buf.extend_from_slice(b"AppName\x00");
    buf.extend_from_slice(name.as_bytes());
    buf.push(0x00);

    // 0x01 Exe \x00 "<exe>" \x00
    let formatted_exe = if exe.starts_with('"') && exe.ends_with('"') {
        exe.to_string()
    } else {
        format!("\"{}\"", exe)
    };
    buf.push(0x01);
    buf.extend_from_slice(b"Exe\x00");
    buf.extend_from_slice(formatted_exe.as_bytes());
    buf.push(0x00);

    // 0x01 StartDir \x00 <start_dir> \x00
    buf.push(0x01);
    buf.extend_from_slice(b"StartDir\x00");
    buf.extend_from_slice(start_dir.as_bytes());
    buf.push(0x00);

    // 0x01 icon \x00 \x00
    buf.push(0x01);
    buf.extend_from_slice(b"icon\x00\x00");

    // 0x01 ShortcutPath \x00 \x00
    buf.push(0x01);
    buf.extend_from_slice(b"ShortcutPath\x00\x00");

    // 0x01 LaunchOptions \x00 <opts> \x00
    buf.push(0x01);
    buf.extend_from_slice(b"LaunchOptions\x00");
    buf.extend_from_slice(launch_options.as_bytes());
    buf.push(0x00);

    // 0x02 IsHidden \x00 0
    buf.push(0x02);
    buf.extend_from_slice(b"IsHidden\x00");
    buf.extend_from_slice(&0u32.to_le_bytes());

    // 0x02 AllowDesktopConfig \x00 1
    buf.push(0x02);
    buf.extend_from_slice(b"AllowDesktopConfig\x00");
    buf.extend_from_slice(&1u32.to_le_bytes());

    // 0x02 AllowOverlay \x00 1
    buf.push(0x02);
    buf.extend_from_slice(b"AllowOverlay\x00");
    buf.extend_from_slice(&1u32.to_le_bytes());

    // 0x02 OpenVR \x00 0
    buf.push(0x02);
    buf.extend_from_slice(b"OpenVR\x00");
    buf.extend_from_slice(&0u32.to_le_bytes());

    // 0x02 Devkit \x00 0
    buf.push(0x02);
    buf.extend_from_slice(b"Devkit\x00");
    buf.extend_from_slice(&0u32.to_le_bytes());

    // 0x01 DevkitGameID \x00 \x00
    buf.push(0x01);
    buf.extend_from_slice(b"DevkitGameID\x00\x00");

    // 0x02 DevkitOverrideAppID \x00 0
    buf.push(0x02);
    buf.extend_from_slice(b"DevkitOverrideAppID\x00");
    buf.extend_from_slice(&0u32.to_le_bytes());

    // 0x02 LastPlayTime \x00 0
    buf.push(0x02);
    buf.extend_from_slice(b"LastPlayTime\x00");
    buf.extend_from_slice(&0u32.to_le_bytes());

    // 0x01 FlatpakAppID \x00 \x00
    buf.push(0x01);
    buf.extend_from_slice(b"FlatpakAppID\x00\x00");

    // 0x01 sortas \x00 \x00
    buf.push(0x01);
    buf.extend_from_slice(b"sortas\x00\x00");

    // 0x00 tags \x00 0x08
    buf.push(0x00);
    buf.extend_from_slice(b"tags\x00\x08");

    // 0x08 end entry
    buf.push(0x08);

    buf
}

/// Appends or updates a shortcut in binary shortcuts.vdf data.
pub fn append_shortcut_to_binary_vdf(
    data: &[u8],
    appid: u32,
    name: &str,
    exe: &str,
    start_dir: &str,
    launch_options: &str,
) -> Option<Vec<u8>> {
    if let Some(updated) =
        update_shortcut_fields_binary_vdf(data, appid, Some(exe), Some(start_dir), None)
    {
        return Some(updated);
    }

    let (cut, count) = if data.is_empty() {
        (0, 0)
    } else {
        // Non-empty but unrecognizable data: refuse rather than overwrite the user's shortcuts.
        if data.len() < 12 {
            return None;
        }
        let (first, entries, _) = walk_entries(data)?;
        (entries.last().map_or(first, |e| e.body.end), entries.len())
    };

    let mut result = if data.is_empty() {
        b"\x00shortcuts\x00".to_vec()
    } else {
        data[..cut].to_vec()
    };
    result.extend_from_slice(&serialize_shortcut_entry(
        count,
        appid,
        name,
        exe,
        start_dir,
        launch_options,
    ));
    result.extend_from_slice(&[0x08, 0x08]);
    Some(result)
}

/// Deletes a shortcut by AppID from binary shortcuts.vdf data and re-indexes remaining entries.
pub fn delete_shortcut_from_binary_vdf(data: &[u8], target_appid: u32) -> Option<Vec<u8>> {
    let (_, entries, _) = walk_entries(data)?;
    if !entries.iter().any(|e| e.appid.unwrap_or(0) == target_appid) {
        return None;
    }

    let mut result = b"\x00shortcuts\x00".to_vec();
    let kept = entries
        .iter()
        .filter(|e| e.appid.unwrap_or(0) != target_appid);
    for (new_idx, e) in kept.enumerate() {
        result.push(0x00);
        result.extend_from_slice(new_idx.to_string().as_bytes());
        result.push(0x00);
        result.extend_from_slice(&data[e.body.clone()]);
    }
    result.extend_from_slice(&[0x08, 0x08]);
    Some(result)
}

/// Appends a shortcut into Steam's shortcuts.vdf for active user profiles.
pub fn append_shortcut_to_all_vdf(
    appid: u32,
    name: &str,
    exe: &str,
    start_dir: &str,
    launch_options: &str,
) -> VapourResult<bool> {
    let mut updated_any = false;

    for user_dir in get_active_user_data_dirs() {
        let shortcuts_path = user_dir.join("config/shortcuts.vdf");
        let data = read_shortcuts_file(&shortcuts_path)?.unwrap_or_default();

        let Some(new_data) =
            append_shortcut_to_binary_vdf(&data, appid, name, exe, start_dir, launch_options)
        else {
            eprintln!("[WARN] skipping corrupt {}", shortcuts_path.display());
            continue;
        };

        write_shortcuts_with_backup(&shortcuts_path, &data, &new_data)?;
        updated_any = true;
    }

    Ok(updated_any)
}

/// Removes a shortcut from Steam's shortcuts.vdf for active user profiles.
pub fn delete_shortcut_from_all_vdf(appid: u32) -> VapourResult<bool> {
    let mut deleted_any = false;

    for user_dir in get_active_user_data_dirs() {
        let shortcuts_path = user_dir.join("config/shortcuts.vdf");
        let Some(data) = read_shortcuts_file(&shortcuts_path)? else {
            continue;
        };
        if let Some(new_data) = delete_shortcut_from_binary_vdf(&data, appid) {
            write_shortcuts_with_backup(&shortcuts_path, &data, &new_data)?;
            deleted_any = true;
        }
    }

    Ok(deleted_any)
}

/// Finds the start (index of '{') and end (index of matching '}') of a named VDF block.
pub fn find_block_bounds(content: &str, block_name: &str) -> Option<(usize, usize)> {
    let needle = format!("\"{}\"", block_name);
    let start_pos = content.find(&needle)?;
    let open_rel = content[start_pos..].find('{')?;
    let open_idx = start_pos + open_rel;

    let mut depth = 0;
    let mut in_quote = false;
    let mut prev_char = ' ';

    for (i, c) in content[open_idx..].char_indices() {
        let abs_idx = open_idx + i;
        if c == '"' && prev_char != '\\' {
            in_quote = !in_quote;
        } else if !in_quote {
            if c == '{' {
                depth += 1;
            } else if c == '}' {
                depth -= 1;
                if depth == 0 {
                    return Some((open_idx, abs_idx));
                }
            }
        }
        prev_char = c;
    }
    None
}

/// Updates or inserts a compatibility tool mapping for an AppID in Steam's config/config.vdf
/// Strictly bounded within the CompatToolMapping block to prevent corruption of other sections.
pub fn set_compat_tool_in_config_vdf(
    steam_root: &Path,
    appid: u32,
    tool_name: &str,
) -> VapourResult<()> {
    let config_file = steam_root.join("config/config.vdf");
    if !config_file.is_file() {
        return Ok(());
    }
    let content = fs::read_to_string(&config_file).map_err(|e| {
        VapourError::VdfParse(format!("Failed to read {}: {}", config_file.display(), e))
    })?;

    let Some((block_open, block_close)) = find_block_bounds(&content, "CompatToolMapping") else {
        return Ok(());
    };

    let inner_block = &content[block_open + 1..block_close];
    let appid_needle = format!("\"{}\"", appid);
    let new_entry = format!(
        "\n\t\t\t\t\t\"{}\"\n\t\t\t\t\t{{\n\t\t\t\t\t\t\"name\"\t\t\"{}\"\n\t\t\t\t\t\t\"config\"\t\t\"\"\n\t\t\t\t\t\t\"priority\"\t\t\"250\"\n\t\t\t\t\t}}",
        appid, tool_name
    );

    let new_content = if let Some(found_rel) = inner_block.find(&appid_needle) {
        let entry_start = block_open + 1 + found_rel;
        if let Some(entry_brace) = content[entry_start..block_close].find('}') {
            let entry_end = entry_start + entry_brace + 1;
            format!(
                "{}{}{}",
                &content[..entry_start],
                new_entry.trim_start(),
                &content[entry_end..]
            )
        } else {
            content
        }
    } else {
        format!(
            "{}{}{}",
            &content[..=block_open],
            new_entry,
            &content[block_open + 1..]
        )
    };

    write_atomic(&config_file, new_content.as_bytes()).map_err(|e| {
        VapourError::Storage(format!("Failed to write {}: {}", config_file.display(), e))
    })?;
    Ok(())
}

/// Removes a compatibility tool mapping for an AppID from Steam's config/config.vdf
/// Strictly bounded within the CompatToolMapping block to prevent corruption of other sections.
pub fn remove_compat_tool_from_config_vdf(steam_root: &Path, appid: u32) -> VapourResult<()> {
    let config_file = steam_root.join("config/config.vdf");
    if !config_file.is_file() {
        return Ok(());
    }
    let content = fs::read_to_string(&config_file).map_err(|e| {
        VapourError::VdfParse(format!("Failed to read {}: {}", config_file.display(), e))
    })?;

    let Some((block_open, block_close)) = find_block_bounds(&content, "CompatToolMapping") else {
        return Ok(());
    };

    let inner_block = &content[block_open + 1..block_close];
    let appid_needle = format!("\"{}\"", appid);
    if let Some(found_rel) = inner_block.find(&appid_needle) {
        let entry_start = block_open + 1 + found_rel;
        let mut actual_start = entry_start;
        while actual_start > block_open + 1
            && (content.as_bytes()[actual_start - 1] == b'\t'
                || content.as_bytes()[actual_start - 1] == b' ')
        {
            actual_start -= 1;
        }
        if actual_start > block_open + 1 && content.as_bytes()[actual_start - 1] == b'\n' {
            actual_start -= 1;
        }

        if let Some(entry_brace) = content[entry_start..block_close].find('}') {
            let entry_end = entry_start + entry_brace + 1;
            let new_content = format!("{}{}", &content[..actual_start], &content[entry_end..]);
            write_atomic(&config_file, new_content.as_bytes()).map_err(|e| {
                VapourError::Storage(format!("Failed to write {}: {e}", config_file.display()))
            })?;
        }
    }

    Ok(())
}

/// Invariant 1: official Steam games can never be deleted from the CLI.
fn ensure_not_official(steam_games: &[crate::library::Game], q_lower: &str) -> VapourResult<()> {
    for sg in steam_games {
        if sg.appid.to_string() == q_lower || sg.name.to_lowercase() == q_lower {
            return Err(VapourError::Usage(format!(
                "'{}' (AppID: {}) is an official Steam game. Official Steam games cannot be deleted from the CLI.",
                sg.name, sg.appid
            )));
        }
    }
    Ok(())
}

/// Deletes a non-Steam game entry, its Steam shortcuts.vdf entry, and its Proton prefix.
/// Explicitly forbids deleting official Steam games.
pub fn delete_non_steam_game(
    query_or_appid: &str,
    config: &mut VapourConfig,
) -> VapourResult<NonSteamGame> {
    // 1. Guard check: Make sure this is not an official Steam game!
    let steam_games = crate::library::get_installed_games(false).unwrap_or_default();
    let q_lower = query_or_appid.trim().to_lowercase();
    ensure_not_official(&steam_games, &q_lower)?;

    // 2. Locate the non-Steam game
    let non_steam_list = load_non_steam_games(config);
    let matched = non_steam_list.into_iter().find(|ng| {
        ng.appid.to_string() == q_lower
            || ng.name.to_lowercase() == q_lower
            || ng.name.to_lowercase().contains(&q_lower)
    });

    let game = match matched {
        Some(g) => g,
        None => {
            return Err(VapourError::GameNotFound(format!(
                "No non-Steam game found matching '{}'.",
                query_or_appid
            )))
        }
    };

    let appid = game.appid;
    let appid_key = appid.to_string();

    // VDF edits first, then registries, then the prefix last, so a failure never
    // leaves a deleted prefix behind a live entry.
    // 3. Remove from Steam shortcuts.vdf
    delete_shortcut_from_all_vdf(appid)?;

    // 4. Remove from CompatToolMapping in config.vdf
    let steam_root = find_steam_root().unwrap_or_else(|| {
        home_dir()
            .map(|h| h.join(".local/share/Steam"))
            .unwrap_or_else(|| PathBuf::from("."))
    });
    remove_compat_tool_from_config_vdf(&steam_root, appid)?;

    // 5. Remove from non_steam.txt
    if let Some(file) = non_steam_config_file() {
        let mut list = load_non_steam_txt();
        let orig_len = list.len();
        list.retain(|g| g.appid != appid);
        if list.len() != orig_len {
            save_non_steam_txt(&file, &list)?;
        }
    }

    // 6. Remove from settings.toml
    config.games.remove(&appid_key);
    config.save()?;

    // 7. Delete associated Proton prefix (steamapps/compatdata/<appid>)
    let prefix = get_compat_data_path_for_appid(appid);
    if prefix.exists() && prefix.is_dir() {
        if let Err(e) = fs::remove_dir_all(&prefix) {
            eprintln!(
                "\x1b[33m[WARN]\x1b[0m Could not remove prefix at {}: {}",
                prefix.display(),
                e
            );
        } else {
            println!(
                "\x1b[32m[OK]\x1b[0m Deleted Proton prefix at: {}",
                prefix.display()
            );
        }
    }

    Ok(game)
}

/// Parses Steam's config.vdf to find compatibility tool assignments in CompatToolMapping
pub fn parse_compat_tool_mapping(steam_root: &Path) -> HashMap<u32, String> {
    let mut mapping = HashMap::new();
    let config_file = steam_root.join("config/config.vdf");
    let Ok(content) = fs::read_to_string(&config_file) else {
        return mapping;
    };

    let Some(compat_start) = content.find("\"CompatToolMapping\"") else {
        return mapping;
    };

    let search_slice = &content[compat_start..];
    let Some(brace_start) = search_slice.find('{') else {
        return mapping;
    };

    let mut depth = 0;
    let mut section_end = brace_start;
    for (i, c) in search_slice[brace_start..].char_indices() {
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                section_end = brace_start + i;
                break;
            }
        }
    }

    let section = &search_slice[brace_start + 1..section_end];
    let mut current_appid: Option<u32> = None;

    for line in section.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('"') {
            let parts: Vec<&str> = trimmed
                .split('"')
                .filter(|s| !s.trim().is_empty())
                .collect();
            if parts.len() == 1 {
                if let Ok(id) = parts[0].parse::<u32>() {
                    current_appid = Some(id);
                }
            } else if parts.len() >= 2 && parts[0] == "name" {
                if let Some(id) = current_appid {
                    mapping.insert(id, parts[1].to_string());
                }
            }
        } else if trimmed == "}" {
            current_appid = None;
        }
    }

    mapping
}

/// Reads the assigned compatibility tool from compatdata/<appid>/config_info or version
pub fn read_tool_from_compatdata(compatdata_dir: &Path) -> Option<String> {
    let config_info = compatdata_dir.join("config_info");
    if let Ok(content) = fs::read_to_string(&config_info) {
        let lines: Vec<&str> = content.lines().collect();
        for line in &lines {
            if line.contains("compatibilitytools.d/") {
                if let Some(after) = line.split("compatibilitytools.d/").nth(1) {
                    if let Some(tool_name) = after.split('/').next() {
                        return Some(tool_name.to_string());
                    }
                }
            }
        }
        if let Some(first) = lines.first() {
            let trimmed = first.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    let version_file = compatdata_dir.join("version");
    if let Ok(ver) = fs::read_to_string(&version_file) {
        let trimmed = ver.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NonSteamGame {
    pub appid: u32,
    pub name: String,
    pub exe: PathBuf,
    pub start_dir: Option<PathBuf>,
    pub use_proton: bool,
    pub compat_tool: Option<String>,
    pub launch_options: String,
    pub args: Vec<String>,
    pub from_steam_shortcut: bool,
    pub is_path_missing: bool,
}

pub fn non_steam_config_file() -> Option<PathBuf> {
    let home = home_dir()?;
    let dir = home.join(".config/vapour");
    let _ = fs::create_dir_all(&dir);
    Some(dir.join("non_steam.txt"))
}

pub fn get_compat_data_path_for_appid(appid: u32) -> PathBuf {
    let steam_root = find_steam_root().unwrap_or_else(|| {
        home_dir()
            .map(|h| h.join(".local/share/Steam"))
            .unwrap_or_else(|| PathBuf::from("."))
    });
    steam_root
        .join("steamapps/compatdata")
        .join(appid.to_string())
}

/// Loads all Non-Steam games:
/// 1. Auto-detects Steam shortcuts from `shortcuts.vdf` in all userdata profiles.
/// 2. Auto-detects existing prefix directories from `steamapps/compatdata/<appid>`.
/// 3. Loads custom entries from `~/.config/vapour/non_steam.txt`.
/// 4. Merges persistent user overrides from `~/.config/vapour/settings.toml` (e.g. updated paths or custom Proton tools).
pub fn load_non_steam_games(config: &VapourConfig) -> Vec<NonSteamGame> {
    let mut games: Vec<NonSteamGame> = Vec::new();

    let steam_root = find_steam_root().unwrap_or_else(|| {
        home_dir()
            .map(|h| h.join(".local/share/Steam"))
            .unwrap_or_else(|| PathBuf::from("."))
    });

    let compat_mappings = parse_compat_tool_mapping(&steam_root);

    // 1. Scan Steam active userdata shortcuts
    for user_dir in get_active_user_data_dirs() {
        let shortcuts_path = user_dir.join("config/shortcuts.vdf");
        if shortcuts_path.is_file() {
            if let Ok(data) = fs::read(&shortcuts_path) {
                if validate_shortcuts_vdf(&data).is_ok() {
                    for raw in parse_shortcuts_vdf(&data) {
                        if raw.is_hidden {
                            continue;
                        }
                        let appid = raw.appid.unwrap_or_else(|| {
                            calculate_steam_shortcut_appid(&raw.exe, &raw.app_name)
                        });

                        // Prevent duplicate shortcut IDs
                        if games.iter().any(|g| g.appid == appid) {
                            continue;
                        }

                        let compat_data_dir = steam_root
                            .join("steamapps/compatdata")
                            .join(appid.to_string());
                        let detected_tool = compat_mappings
                            .get(&appid)
                            .cloned()
                            .or_else(|| read_tool_from_compatdata(&compat_data_dir));

                        let mut exe = PathBuf::from(&raw.exe);
                        let mut start_dir = if raw.start_dir.is_empty() {
                            exe.parent().map(|p| p.to_path_buf())
                        } else {
                            Some(PathBuf::from(&raw.start_dir))
                        };

                        // Check VapourConfig overrides
                        let appid_key = appid.to_string();
                        let game_cfg = config.get_game_config(&appid_key);
                        if let Some(ref override_exe) = game_cfg.exe_path {
                            exe = override_exe.clone();
                        }
                        if let Some(ref override_sd) = game_cfg.start_dir {
                            start_dir = Some(override_sd.clone());
                        }

                        let final_tool = game_cfg.compat_tool.clone().or(detected_tool);
                        let is_windows_exe = exe
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
                        let use_proton = final_tool.is_some() || is_windows_exe;

                        let final_launch_options = game_cfg
                            .launch_options
                            .clone()
                            .unwrap_or(raw.launch_options);
                        // Extra args come from settings at launch (config.extra_args); copying them here would double them.
                        let args = Vec::new();

                        let is_path_missing = !exe.exists();

                        games.push(NonSteamGame {
                            appid,
                            name: raw.app_name,
                            exe,
                            start_dir,
                            use_proton,
                            compat_tool: final_tool,
                            launch_options: final_launch_options,
                            args,
                            from_steam_shortcut: true,
                            is_path_missing,
                        });
                    }
                }
            }
        }
    }

    // 2. Scan compatdata folders across all Steam libraries for any non-Steam prefixes (ID >= 0x80000000)
    for lib in get_all_library_folders() {
        let compat_dir = lib.join("steamapps/compatdata");
        if let Ok(entries) = fs::read_dir(compat_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Ok(id) = name.parse::<u32>() {
                    if id >= 0x80000000 && !games.iter().any(|g| g.appid == id) {
                        let detected_tool = compat_mappings
                            .get(&id)
                            .cloned()
                            .or_else(|| read_tool_from_compatdata(&entry.path()));

                        let appid_key = id.to_string();
                        let game_cfg = config.get_game_config(&appid_key);

                        let exe = game_cfg
                            .exe_path
                            .clone()
                            .unwrap_or_else(|| PathBuf::from(""));
                        let start_dir = game_cfg
                            .start_dir
                            .clone()
                            .or_else(|| exe.parent().map(|p| p.to_path_buf()));
                        let final_tool = game_cfg.compat_tool.clone().or(detected_tool);
                        let is_path_missing = !exe.exists();

                        games.push(NonSteamGame {
                            appid: id,
                            name: format!("Non-Steam App {}", id),
                            exe,
                            start_dir,
                            use_proton: true,
                            compat_tool: final_tool,
                            launch_options: game_cfg.launch_options.clone().unwrap_or_default(),
                            args: Vec::new(),
                            from_steam_shortcut: false,
                            is_path_missing,
                        });
                    }
                }
            }
        }
    }

    // 3. Load custom games from ~/.config/vapour/non_steam.txt
    for custom in load_non_steam_txt() {
        if !games
            .iter()
            .any(|g| g.name == custom.name || g.exe == custom.exe)
        {
            let appid_key = custom.appid.to_string();
            let game_cfg = config.get_game_config(&appid_key);
            let mut g = custom;
            if let Some(ref override_exe) = game_cfg.exe_path {
                g.exe = override_exe.clone();
            }
            if let Some(ref override_sd) = game_cfg.start_dir {
                g.start_dir = Some(override_sd.clone());
            }
            if let Some(ref override_tool) = game_cfg.compat_tool {
                g.compat_tool = Some(override_tool.clone());
            }
            if let Some(ref override_opts) = game_cfg.launch_options {
                g.launch_options = override_opts.clone();
            }
            // Args from settings (extra_args or launch_options) replace the txt args, avoiding duplicates.
            // An entry holding only e.g. compat_tool must keep them.
            if !game_cfg.extra_args.is_empty() || game_cfg.launch_options.is_some() {
                g.args.clear();
            }
            g.is_path_missing = !g.exe.exists();
            games.push(g);
        }
    }

    // Sort by name
    games.sort_by_key(|a| a.name.to_lowercase());
    games
}

/// Reads games configured in ~/.config/vapour/non_steam.txt
fn load_non_steam_txt() -> Vec<NonSteamGame> {
    let Some(file) = non_steam_config_file() else {
        return Vec::new();
    };
    let Ok(content) = fs::read_to_string(&file) else {
        return Vec::new();
    };

    let mut games = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = trimmed.split('|').map(|s| s.trim()).collect();
        if parts.len() >= 2 {
            let name = parts[0].to_string();
            let exe = PathBuf::from(parts[1]);
            let use_proton = parts
                .get(2)
                .map(|s| *s == "true" || *s == "1")
                .unwrap_or_else(|| exe.extension().is_some_and(|ext| ext == "exe"));
            let args = parts
                .get(3)
                .map(|s| s.split_whitespace().map(String::from).collect())
                .unwrap_or_default();

            let appid = calculate_steam_shortcut_appid(parts[1], &name);
            let start_dir = exe.parent().map(|p| p.to_path_buf());
            let is_path_missing = !exe.exists();

            games.push(NonSteamGame {
                appid,
                name,
                exe,
                start_dir,
                use_proton,
                compat_tool: None,
                launch_options: String::new(),
                args,
                from_steam_shortcut: false,
                is_path_missing,
            });
        }
    }
    games
}

/// Appends a new non-Steam game entry
pub fn add_non_steam_game(
    name: &str,
    exe: &Path,
    use_proton: bool,
    compat_tool: Option<String>,
    args: &[String],
    config: &mut VapourConfig,
) -> VapourResult<u32> {
    let exe_str = exe.to_string_lossy().to_string();
    let appid = calculate_steam_shortcut_appid(&exe_str, name);

    // Save in settings.toml
    let appid_key = appid.to_string();
    let mut game_cfg = config.get_game_config(&appid_key);
    game_cfg.exe_path = Some(exe.to_path_buf());
    if let Some(parent) = exe.parent() {
        game_cfg.start_dir = Some(parent.to_path_buf());
    }
    game_cfg.compat_tool = compat_tool.clone();
    game_cfg.extra_args = args.to_vec();

    config.games.insert(appid_key, game_cfg);
    config.save()?;

    // Also append to non_steam.txt
    if let Some(file) = non_steam_config_file() {
        let mut list = load_non_steam_txt();
        if !list.iter().any(|g| g.appid == appid) {
            list.push(NonSteamGame {
                appid,
                name: name.to_string(),
                exe: exe.to_path_buf(),
                start_dir: exe.parent().map(|p| p.to_path_buf()),
                use_proton,
                compat_tool: compat_tool.clone(),
                launch_options: String::new(),
                args: args.to_vec(),
                from_steam_shortcut: true,
                is_path_missing: !exe.exists(),
            });
            save_non_steam_txt(&file, &list)?;
        }
    }

    // Append to Steam's shortcuts.vdf so it appears natively in the Steam client
    let start_dir_str = exe
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    append_shortcut_to_all_vdf(appid, name, &exe_str, &start_dir_str, &args.join(" "))?;

    // Assign compatibility tool in Steam's config.vdf if selected
    if let Some(ref tool) = compat_tool {
        let steam_root = find_steam_root().unwrap_or_else(|| {
            home_dir()
                .map(|h| h.join(".local/share/Steam"))
                .unwrap_or_else(|| PathBuf::from("."))
        });
        let _ = set_compat_tool_in_config_vdf(&steam_root, appid, tool);
    }

    Ok(appid)
}

fn save_non_steam_txt(file: &Path, games: &[NonSteamGame]) -> VapourResult<()> {
    let mut lines = Vec::new();
    lines.push("# Vapour Non-Steam Games Registry".to_string());
    lines.push("# Format: Name | ExePath | use_proton | extra_args".to_string());
    for g in games {
        lines.push(format!(
            "{} | {} | {} | {}",
            g.name,
            g.exe.display(),
            if g.use_proton { "true" } else { "false" },
            g.args.join(" ")
        ));
    }
    lines.push(String::new());

    write_atomic(file, lines.join("\n").as_bytes())
        .map_err(|e| VapourError::Storage(format!("Failed to write {}: {}", file.display(), e)))
}

/// Executes a non-Steam game.
/// For Proton games, sets up the exact environment used by Steam and Protontricks:
/// Executes a non-Steam game directly using Proton or native runner.
pub fn launch_non_steam(
    game: &NonSteamGame,
    config: &crate::config::ResolvedLaunchConfig,
    dry_run: bool,
) -> VapourResult<Option<u32>> {
    use crate::runner::{get_runner_for_target, LaunchTarget};
    let target = LaunchTarget::DirectNonSteam { game: game.clone() };
    let runner = get_runner_for_target(&target, true);
    let res = runner.launch(&target, config, dry_run)?;
    Ok(res.pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compatibility_tool_wow64() {
        let p9 = CompatibilityTool {
            name: "Proton 9.0".to_string(),
            proton_bin: PathBuf::from("/mock/proton"),
        };
        assert!(p9.is_wow64());

        let p10 = CompatibilityTool {
            name: "Proton 10.0".to_string(),
            proton_bin: PathBuf::from("/mock/proton"),
        };
        assert!(p10.is_wow64());

        let ge = CompatibilityTool {
            name: "GE-Proton9-23".to_string(),
            proton_bin: PathBuf::from("/mock/proton"),
        };
        assert!(ge.is_wow64());

        let cachy = CompatibilityTool {
            name: "proton-cachyos-slr".to_string(),
            proton_bin: PathBuf::from("/mock/proton"),
        };
        assert!(cachy.is_wow64());

        let legacy = CompatibilityTool {
            name: "Proton 7.0".to_string(),
            proton_bin: PathBuf::from("/mock/proton"),
        };
        assert!(!legacy.is_wow64());
    }

    #[test]
    fn test_crc32_and_appid() {
        // AppID formula: (CRC32(target + name) | 0x80000000)
        let appid =
            calculate_steam_shortcut_appid("/home/user/Games/TestGame/game.exe", "TestGame");
        // Bit 31 must be set
        assert!(appid >= 0x80000000);
    }

    #[test]
    fn test_strip_quotes() {
        assert_eq!(strip_quotes("\"/path/to/game.exe\""), "/path/to/game.exe");
        assert_eq!(
            strip_quotes("  \"/path/to/game.exe\"  "),
            "/path/to/game.exe"
        );
        assert_eq!(strip_quotes("/path/to/game.exe"), "/path/to/game.exe");
    }

    #[test]
    fn test_parse_binary_vdf_synthetic() {
        // Build minimal synthetic binary VDF for shortcuts
        let mut buf = Vec::new();
        // 0x00 shortcuts \x00
        buf.push(0x00);
        buf.extend_from_slice(b"shortcuts\x00");
        // 0x00 0 \x00
        buf.push(0x00);
        buf.extend_from_slice(b"0\x00");
        // 0x02 appid \x00 <4 bytes>
        buf.push(0x02);
        buf.extend_from_slice(b"appid\x00");
        buf.extend_from_slice(&12345678u32.to_le_bytes());
        // 0x01 AppName \x00 My Cool Game \x00
        buf.push(0x01);
        buf.extend_from_slice(b"AppName\x00");
        buf.extend_from_slice(b"My Cool Game\x00");
        // 0x01 Exe \x00 "/path/game.exe" \x00
        buf.push(0x01);
        buf.extend_from_slice(b"Exe\x00");
        buf.extend_from_slice(b"\"/path/game.exe\"\x00");
        // 0x08 end entry
        buf.push(0x08);
        // 0x08 end shortcuts
        buf.push(0x08);

        let parsed = parse_shortcuts_vdf(&buf);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].appid, Some(12345678));
        assert_eq!(parsed[0].app_name, "My Cool Game");
        assert_eq!(parsed[0].exe, "/path/game.exe");

        // Test updating the binary VDF
        let updated = update_shortcut_fields_binary_vdf(
            &buf,
            12345678,
            Some("/new/path/game.exe"),
            Some("/new/path"),
            None,
        );
        assert!(updated.is_some());
        let updated_buf = updated.unwrap();
        let re_parsed = parse_shortcuts_vdf(&updated_buf);
        assert_eq!(re_parsed.len(), 1);
        assert_eq!(re_parsed[0].appid, Some(12345678));
        assert_eq!(re_parsed[0].app_name, "My Cool Game");
        assert_eq!(re_parsed[0].exe, "/new/path/game.exe");

        // Test updating launch options (inserted when key was missing)
        let with_opts = update_shortcut_fields_binary_vdf(
            &updated_buf,
            12345678,
            None,
            None,
            Some("WINEDLLOVERRIDES=\"winhttp=n,b\" %command%"),
        );
        assert!(with_opts.is_some());
        let with_opts_buf = with_opts.unwrap();
        let parsed_with_opts = parse_shortcuts_vdf(&with_opts_buf);
        assert_eq!(parsed_with_opts.len(), 1);
        assert_eq!(
            parsed_with_opts[0].launch_options,
            "WINEDLLOVERRIDES=\"winhttp=n,b\" %command%"
        );

        // Test updating launch options again (in-place replacement)
        let replaced_opts = update_shortcut_fields_binary_vdf(
            &with_opts_buf,
            12345678,
            None,
            None,
            Some("-novid -fullscreen"),
        );
        assert!(replaced_opts.is_some());
        let replaced_opts_buf = replaced_opts.unwrap();
        let parsed_replaced = parse_shortcuts_vdf(&replaced_opts_buf);
        assert_eq!(parsed_replaced.len(), 1);
        assert_eq!(parsed_replaced[0].launch_options, "-novid -fullscreen");

        // Test appending a second game
        let appended = append_shortcut_to_binary_vdf(
            &replaced_opts_buf,
            87654321,
            "Second Game",
            "/games/second.exe",
            "/games",
            "-silent",
        )
        .unwrap();
        let parsed_appended = parse_shortcuts_vdf(&appended);
        assert_eq!(parsed_appended.len(), 2);
        assert_eq!(parsed_appended[1].appid, Some(87654321));
        assert_eq!(parsed_appended[1].app_name, "Second Game");
        assert_eq!(parsed_appended[1].exe, "/games/second.exe");

        // Test deleting the first game
        let deleted = delete_shortcut_from_binary_vdf(&appended, 12345678);
        assert!(deleted.is_some());
        let parsed_deleted = parse_shortcuts_vdf(&deleted.unwrap());
        assert_eq!(parsed_deleted.len(), 1);
        assert_eq!(parsed_deleted[0].appid, Some(87654321));
        assert_eq!(parsed_deleted[0].app_name, "Second Game");
    }

    #[test]
    fn delete_shortcut_should_return_err_when_backup_dir_readonly() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: getuid has no preconditions and cannot fail.
        if unsafe { libc::getuid() } == 0 {
            return; // root ignores directory permissions
        }
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("shortcuts.vdf");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let res = write_shortcuts_with_backup(&path, b"old", b"new");
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(res.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
    }

    #[test]
    fn append_shortcut_should_refuse_when_data_is_corrupt() {
        assert!(append_shortcut_to_binary_vdf(b"\x05garbage", 1, "n", "/e", "/d", "").is_none());
    }

    #[test]
    fn append_shortcut_should_create_file_when_data_empty() {
        let out = append_shortcut_to_binary_vdf(b"", 1, "n", "/e", "/d", "").unwrap();
        assert_eq!(parse_shortcuts_vdf(&out).len(), 1);
    }

    #[test]
    fn test_delete_official_steam_game_guard() {
        // Pure check: never touches the real Steam folders.
        let game = crate::library::Game {
            appid: 730,
            name: "Counter-Strike 2".into(),
            installdir: String::new(),
            library_path: PathBuf::new(),
            size_on_disk: 0,
            last_played: 0,
            buildid: 0,
            is_unmounted: false,
            unmounted_label: None,
        };
        let err = ensure_not_official(std::slice::from_ref(&game), "730").unwrap_err();
        assert!(err.to_string().contains("official Steam game"));
        assert!(ensure_not_official(&[game], "999").is_ok());
    }

    #[test]
    fn test_parse_active_account_id_from_loginusers() {
        let sample = r#"
"users"
{
	"76561197972611406"
	{
		"AccountName"		"testuser"
		"PersonaName"		"tester"
		"RememberPassword"		"1"
		"MostRecent"		"1"
		"timestamp"		"1793606800"
	}
}
"#;
        let id = parse_active_account_id_from_loginusers(sample);
        assert_eq!(id, Some(12345678));
    }

    #[test]
    fn test_find_block_bounds() {
        let sample = r#"
"Software"
{
	"Valve"
	{
		"Steam"
		{
			"CompatToolMapping"
			{
				"123"
				{
					"name" "proton"
				}
			}
			"OtherSetting" "1"
		}
	}
}
"#;
        let bounds = find_block_bounds(sample, "CompatToolMapping");
        assert!(bounds.is_some());
        let (open_idx, close_idx) = bounds.unwrap();
        let block = &sample[open_idx..=close_idx];
        assert!(block.starts_with('{'));
        assert!(block.ends_with('}'));
        assert!(block.contains("\"123\""));
        assert!(!block.contains("OtherSetting"));
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_fuzz_parse_shortcuts_vdf_arbitrary_bytes(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
            // Must never panic on arbitrary byte sequences
            let _ = parse_shortcuts_vdf(&data);
        }

        #[test]
        fn test_fuzz_validate_shortcuts_vdf(data in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = validate_shortcuts_vdf(&data);
        }

        #[test]
        fn test_fuzz_update_shortcut_binary_vdf(
            data in proptest::collection::vec(any::<u8>(), 0..2048),
            appid in any::<u32>(),
            exe in "\\PC*",
            start_dir in "\\PC*"
        ) {
            let _ = update_shortcut_fields_binary_vdf(&data, appid, Some(&exe), Some(&start_dir), None);
        }
    }
}

/// Golden tests pinning the exact byte output of the binary shortcuts.vdf editors.
#[cfg(test)]
mod golden_vdf_tests {
    use super::*;

    fn kv_s(k: &str, v: &str) -> Vec<u8> {
        [&[1u8][..], k.as_bytes(), &[0], v.as_bytes(), &[0]].concat()
    }
    fn kv_i(k: &str, v: u32) -> Vec<u8> {
        [&[2u8][..], k.as_bytes(), &[0], &v.to_le_bytes()].concat()
    }
    fn kv_q(k: &str, v: u64) -> Vec<u8> {
        [&[7u8][..], k.as_bytes(), &[0], &v.to_le_bytes()].concat()
    }
    fn tags() -> Vec<u8> {
        let mut b = [&[0u8][..], b"tags\0"].concat();
        b.extend(kv_s("0", "fav"));
        b.extend(kv_i("1", 7));
        b.push(8);
        b
    }
    fn entry(
        idx: usize,
        appid: u32,
        name: &str,
        exe: &str,
        dir: &str,
        opts: Option<&str>,
    ) -> Vec<u8> {
        let mut b = [&[0u8][..], idx.to_string().as_bytes(), &[0]].concat();
        b.extend(kv_i("appid", appid));
        b.extend(kv_s("AppName", name));
        b.extend(kv_s("Exe", exe));
        b.extend(kv_s("StartDir", dir));
        if let Some(o) = opts {
            b.extend(kv_s("LaunchOptions", o));
        }
        b.extend(tags());
        b.extend(kv_q("LastPlayTime64", 0x1122334455667788));
        b.extend(kv_i("IsHidden", 0));
        b.push(8);
        b
    }
    fn file(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut b = b"\x00shortcuts\x00".to_vec();
        for e in entries {
            b.extend(e);
        }
        b.extend([8, 8]);
        b
    }
    fn fixture_b() -> Vec<u8> {
        file(&[
            entry(0, 100, "One", "\"/a\"", "\"/\"", Some("-a")),
            entry(1, 200, "Two", "\"/b\"", "\"/\"", None),
        ])
    }
    fn fixture_c() -> Vec<u8> {
        let mut b = b"\x00shortcuts\x00".to_vec();
        b.extend(serialize_shortcut_entry(0, 300, "C0", "/c0", "/", "-x"));
        b.extend(serialize_shortcut_entry(1, 400, "C1", "/c1", "/", ""));
        b.extend([8, 8]);
        b
    }

    #[test]
    fn golden_parse_should_read_nested_and_int64_fields() {
        let p = parse_shortcuts_vdf(&fixture_b());
        let got: Vec<_> = p
            .iter()
            .map(|s| {
                (
                    s.appid,
                    s.app_name.clone(),
                    s.exe.clone(),
                    s.launch_options.clone(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (Some(100), "One".into(), "/a".into(), "-a".into()),
                (Some(200), "Two".into(), "/b".into(), String::new()),
            ]
        );
        assert!(parse_shortcuts_vdf(b"").is_empty());
        assert_eq!(parse_shortcuts_vdf(&fixture_c()).len(), 2);
    }

    #[test]
    fn golden_update_should_replace_fields_in_place() {
        let out = update_shortcut_fields_binary_vdf(
            &fixture_b(),
            100,
            Some("/new"),
            Some("/nd"),
            Some("-z"),
        )
        .unwrap();
        let want = file(&[
            entry(0, 100, "One", "\"/new\"", "\"/nd\"", Some("-z")),
            entry(1, 200, "Two", "\"/b\"", "\"/\"", None),
        ]);
        assert_eq!(out, want);
    }

    #[test]
    fn golden_update_should_insert_launch_options_when_missing() {
        let out =
            update_shortcut_fields_binary_vdf(&fixture_b(), 200, None, None, Some("-new")).unwrap();
        // Inserted right before the entry's closing 0x08.
        let mut second = entry(1, 200, "Two", "\"/b\"", "\"/\"", None);
        second.pop();
        second.extend(kv_s("LaunchOptions", "-new"));
        second.push(8);
        let want = file(&[entry(0, 100, "One", "\"/a\"", "\"/\"", Some("-a")), second]);
        assert_eq!(out, want);
        assert!(
            update_shortcut_fields_binary_vdf(&fixture_b(), 999, None, None, Some("x")).is_none()
        );
        assert!(update_shortcut_fields_binary_vdf(b"", 1, None, None, Some("x")).is_none());
    }

    #[test]
    fn golden_delete_should_remove_entry_and_reindex() {
        let out = delete_shortcut_from_binary_vdf(&fixture_b(), 100).unwrap();
        let want = file(&[entry(0, 200, "Two", "\"/b\"", "\"/\"", None)]);
        assert_eq!(out, want);
        assert!(delete_shortcut_from_binary_vdf(&fixture_b(), 999).is_none());
        assert!(delete_shortcut_from_binary_vdf(b"", 100).is_none());

        let c = delete_shortcut_from_binary_vdf(&fixture_c(), 300).unwrap();
        let mut want_c = b"\x00shortcuts\x00".to_vec();
        want_c.extend(serialize_shortcut_entry(0, 400, "C1", "/c1", "/", ""));
        want_c.extend([8, 8]);
        assert_eq!(c, want_c);
    }

    #[test]
    fn golden_append_should_add_entry_after_last() {
        let out =
            append_shortcut_to_binary_vdf(&fixture_b(), 500, "Five", "/e", "/", "-q").unwrap();
        let mut want = fixture_b();
        want.truncate(want.len() - 2);
        want.extend(serialize_shortcut_entry(2, 500, "Five", "/e", "/", "-q"));
        want.extend([8, 8]);
        assert_eq!(out, want);

        let out_c = append_shortcut_to_binary_vdf(&fixture_c(), 600, "Six", "/s", "/", "").unwrap();
        let mut want_c = fixture_c();
        want_c.truncate(want_c.len() - 2);
        want_c.extend(serialize_shortcut_entry(2, 600, "Six", "/s", "/", ""));
        want_c.extend([8, 8]);
        assert_eq!(out_c, want_c);
    }

    #[test]
    fn golden_append_should_build_file_from_empty() {
        let out = append_shortcut_to_binary_vdf(b"", 1, "A", "/a", "/", "").unwrap();
        let mut want = b"\x00shortcuts\x00".to_vec();
        want.extend(serialize_shortcut_entry(0, 1, "A", "/a", "/", ""));
        want.extend([8, 8]);
        assert_eq!(out, want);
    }

    #[test]
    fn golden_append_should_update_existing_appid_instead_of_duplicating() {
        let out =
            append_shortcut_to_binary_vdf(&fixture_b(), 100, "One", "/moved", "/m", "").unwrap();
        let want =
            update_shortcut_fields_binary_vdf(&fixture_b(), 100, Some("/moved"), Some("/m"), None)
                .unwrap();
        assert_eq!(out, want);
    }
}
