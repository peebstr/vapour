use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{VapourError, VapourResult};
use crate::library::home_dir;
use crate::non_steam::write_atomic;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountInfo {
    Mounted {
        device: String,
        mountpoint: PathBuf,
        fstype: String,
    },
    Unmounted {
        label: String,
        device: PathBuf,
        target_path: PathBuf,
    },
    DirectoryMissing {
        path: PathBuf,
    },
}

/// Checks the mount status of a library directory.
pub fn check_library_mount(path: &Path) -> MountInfo {
    if path.exists() {
        if let Some((dev, mp, fs)) = get_mount_for_path(path) {
            return MountInfo::Mounted {
                device: dev,
                mountpoint: mp,
                fstype: fs,
            };
        }
        return MountInfo::Mounted {
            device: "unknown".into(),
            mountpoint: path.to_path_buf(),
            fstype: "unknown".into(),
        };
    }

    // Path does not exist. Check if it lives under a removable/external mount prefix like /run/media/
    if let Some((label, dev)) = find_unmounted_device_for_path(path) {
        return MountInfo::Unmounted {
            label,
            device: dev,
            target_path: path.to_path_buf(),
        };
    }

    MountInfo::DirectoryMissing {
        path: path.to_path_buf(),
    }
}

/// Unescapes octal sequences found in /proc/mounts (e.g. \040 -> space, \011 -> tab, \134 -> \).
pub fn unescape_octal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let mut octal = String::new();
            for _ in 0..3 {
                if let Some(&next_c) = chars.peek() {
                    if next_c.is_digit(8) {
                        if let Some(ch) = chars.next() {
                            octal.push(ch);
                        }
                    } else {
                        break;
                    }
                }
            }
            if octal.len() == 3 {
                if let Ok(b) = u8::from_str_radix(&octal, 8) {
                    out.push(b as char);
                    continue;
                }
            }
            out.push('\\');
            out.push_str(&octal);
        } else {
            out.push(c);
        }
    }
    out
}

/// Inspects /proc/mounts to find the filesystem and device of a mounted directory.
pub fn get_mount_for_path(path: &Path) -> Option<(String, PathBuf, String)> {
    let mounts_content = fs::read_to_string("/proc/mounts").ok()?;
    let path_canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let mut best_match: Option<(String, PathBuf, String)> = None;
    let mut longest_prefix_len = 0;

    for line in mounts_content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let device = unescape_octal(parts[0]);
            let mountpoint = PathBuf::from(unescape_octal(parts[1]));
            let fstype = parts[2];

            if path_canon.starts_with(&mountpoint) {
                let len = mountpoint.as_os_str().len();
                if len > longest_prefix_len {
                    longest_prefix_len = len;
                    best_match = Some((device, mountpoint, fstype.to_string()));
                }
            }
        }
    }

    best_match
}

/// Extracts partition label from paths like /run/media/<user>/<label>/...
pub fn extract_media_label(path: &Path) -> Option<String> {
    let path_str = path.to_str()?;
    for prefix in ["/run/media/", "/media/"] {
        if let Some(rest) = path_str.strip_prefix(prefix) {
            let components: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
            // Expected: <username>/<label>/...
            if components.len() >= 2 {
                return Some(components[1].to_string());
            } else if components.len() == 1 {
                return Some(components[0].to_string());
            }
        }
    }
    None
}

/// Finds the device corresponding to an unmounted media path.
pub fn find_unmounted_device_for_path(path: &Path) -> Option<(String, PathBuf)> {
    let label = extract_media_label(path)?;
    let by_label = PathBuf::from(format!("/dev/disk/by-label/{label}"));
    if by_label.exists() {
        return Some((label, by_label));
    }

    // Fallback: search /dev/disk/by-label for case-insensitive match
    if let Ok(entries) = fs::read_dir("/dev/disk/by-label") {
        let lower = label.to_lowercase();
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.to_lowercase() == lower {
                    return Some((name.to_string(), entry.path()));
                }
            }
        }
    }

    None
}

/// Mounts a block device using udisksctl, exactly reproducing Nautilus behavior.
/// Builds the `udisksctl mount` arguments. Non-interactive runs must never wait on polkit.
fn udisksctl_args(device: &Path, interactive: bool) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["mount".into(), "-b".into(), device.into()];
    if !interactive {
        args.push("--no-user-interaction".into());
    }
    args
}

pub fn mount_device_with_udisksctl(device: &Path, interactive: bool) -> VapourResult<String> {
    println!(
        "\x1b[36m[MOUNT]\x1b[0m Invoking udisksctl mount on {:?}...",
        device
    );
    if interactive {
        println!("        (A polkit authentication prompt may appear if elevation is required)");
    }

    let output = Command::new("udisksctl")
        .args(udisksctl_args(device, interactive))
        .output()
        .map_err(|e| VapourError::Storage(format!("Failed to run udisksctl: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        let msg = stdout.trim();
        println!("\x1b[32m[OK]\x1b[0m {}", msg);
        Ok(msg.to_string())
    } else {
        Err(VapourError::Storage(format!(
            "udisksctl failed: {}",
            stderr.trim()
        )))
    }
}

/// Path to vapour's custom libraries config.
pub fn custom_libraries_file() -> Option<PathBuf> {
    let home = home_dir()?;
    let dir = home.join(".config/vapour");
    let _ = fs::create_dir_all(&dir);
    Some(dir.join("libraries.txt"))
}

/// Loads user-registered custom Steam libraries.
pub fn load_custom_libraries() -> Vec<PathBuf> {
    let Some(file) = custom_libraries_file() else {
        return Vec::new();
    };
    if let Ok(content) = fs::read_to_string(&file) {
        content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(PathBuf::from)
            .collect()
    } else {
        Vec::new()
    }
}

/// Adds a custom library to vapour's registry.
pub fn add_custom_library(path: &Path) -> VapourResult<()> {
    let file = custom_libraries_file()
        .ok_or_else(|| VapourError::Storage("Cannot determine home directory".into()))?;
    let mut libs = load_custom_libraries();
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if libs.contains(&canon) {
        return Ok(());
    }
    libs.push(canon);

    let content = libs
        .into_iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    write_atomic(&file, content.as_bytes())
        .map_err(|e| VapourError::Storage(format!("Failed to write {}: {}", file.display(), e)))
}

/// Removes a custom library from vapour's registry.
pub fn remove_custom_library(path: &Path) -> VapourResult<()> {
    let file = custom_libraries_file()
        .ok_or_else(|| VapourError::Storage("Cannot determine home directory".into()))?;
    let libs = load_custom_libraries();
    let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    let filtered: Vec<PathBuf> = libs
        .into_iter()
        .filter(|p| p != &target && p != path)
        .collect();
    let content = filtered
        .into_iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    write_atomic(&file, content.as_bytes())
        .map_err(|e| VapourError::Storage(format!("Failed to write {}: {}", file.display(), e)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn udisksctl_args_should_add_no_user_interaction_when_non_interactive() {
        let dev = std::path::Path::new("/dev/sdb1");
        let off = super::udisksctl_args(dev, false);
        let on = super::udisksctl_args(dev, true);
        assert!(off.iter().any(|a| a == "--no-user-interaction"));
        assert!(!on.iter().any(|a| a == "--no-user-interaction"));
    }

    use super::*;

    #[test]
    fn test_unescape_octal() {
        assert_eq!(
            unescape_octal(r"/run/media/user/Games\040Drive"),
            "/run/media/user/Games Drive"
        );
        assert_eq!(
            unescape_octal(r"/run/media/user/Tab\011Drive"),
            "/run/media/user/Tab\tDrive"
        );
        assert_eq!(unescape_octal(r"normal/path"), "normal/path");
    }

    #[test]
    fn test_extract_media_label() {
        let p = Path::new("/run/media/user/win/Program Files (x86)/Steam");
        assert_eq!(extract_media_label(p), Some("win".to_string()));

        let p2 = Path::new("/media/user/Games/SteamLibrary");
        assert_eq!(extract_media_label(p2), Some("Games".to_string()));

        let p3 = Path::new("/home/user/.local/share/Steam");
        assert_eq!(extract_media_label(p3), None);
    }
}
