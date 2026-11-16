use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::launcher::{
    is_steam_pipe_open, is_steam_running, is_valve_ipc_ready, resolve_steam_bin,
};
use crate::library::find_steam_root;
use crate::non_steam::{find_available_protons, get_active_user_data_dirs};

fn is_in_path(binary: &str) -> bool {
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let full = dir.join(binary);
            if full.is_file() {
                return true;
            }
        }
    }
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckItem {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub kernel: CheckItem,
    pub cgroup_v2: CheckItem,
    pub swap_zram: CheckItem,
    pub polkit_storage: CheckItem,
    pub valve_ipc: CheckItem,
    pub container_runtime: CheckItem,
    pub active_profile: CheckItem,
    pub compatibility_tools: CheckItem,
    pub wrappers: CheckItem,
    pub capabilities: Vec<CheckItem>,
    /// Filled only by `doctor --deep`.
    #[serde(default)]
    pub flag_checks: Vec<CheckItem>,
    pub all_healthy: bool,
}

impl DoctorReport {
    pub fn print_human(&self, plain: bool) {
        let (c_bold, c_cyan, c_green, c_yellow, c_red, c_reset) = if plain {
            ("", "", "", "", "", "")
        } else {
            (
                "\x1b[1m",
                "\x1b[1;36m",
                "\x1b[32m",
                "\x1b[33m",
                "\x1b[31m",
                "\x1b[0m",
            )
        };

        println!("{c_cyan}=== Vapour Environment Diagnostic Doctor ==={c_reset}\n");

        let items = [
            &self.kernel,
            &self.cgroup_v2,
            &self.swap_zram,
            &self.polkit_storage,
            &self.valve_ipc,
            &self.container_runtime,
            &self.active_profile,
            &self.compatibility_tools,
            &self.wrappers,
        ];

        for item in items
            .into_iter()
            .chain(self.capabilities.iter())
            .chain(self.flag_checks.iter())
        {
            let status_badge = match item.status {
                CheckStatus::Ok => format!("{c_green}[OK]{c_reset}"),
                CheckStatus::Warn => format!("{c_yellow}[WARN]{c_reset}"),
                CheckStatus::Fail => format!("{c_red}[FAIL]{c_reset}"),
            };

            println!(
                "  {status_badge} {c_bold}{:<24}{c_reset} {}",
                item.name, item.message
            );
            if let Some(ref d) = item.details {
                println!("       {d}");
            }
        }

        println!();
        if self.all_healthy {
            println!(
                "{c_green}{c_bold}[READY]{c_reset} All systems operational. Ready to launch games."
            );
        } else {
            println!("{c_yellow}{c_bold}[DEGRADED]{c_reset} Some features may have degraded performance or missing prerequisites.");
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

pub fn run_doctor_checks(deep: bool) -> DoctorReport {
    let kernel = check_kernel();
    let cgroup_v2 = check_cgroups();
    let swap_zram = check_swap();
    let polkit_storage = check_polkit();
    let valve_ipc = check_valve_ipc();
    let container_runtime = check_container_runtime();
    let active_profile = check_active_profile();
    let compatibility_tools = check_compatibility_tools();
    let wrappers = check_wrappers();
    let capabilities = check_capabilities();
    let flag_checks = if deep { check_flag_drift() } else { Vec::new() };

    let all_healthy = kernel.status != CheckStatus::Fail
        && cgroup_v2.status != CheckStatus::Fail
        && swap_zram.status != CheckStatus::Fail
        && polkit_storage.status != CheckStatus::Fail
        && container_runtime.status != CheckStatus::Fail;

    DoctorReport {
        kernel,
        cgroup_v2,
        swap_zram,
        polkit_storage,
        valve_ipc,
        container_runtime,
        active_profile,
        compatibility_tools,
        wrappers,
        capabilities,
        flag_checks,
        all_healthy,
    }
}

/// Flags that appear in none of the haystacks (plain byte-window search).
fn missing_flags<'a>(flags: &[&'a str], haystacks: &[&[u8]]) -> Vec<&'a str> {
    flags
        .iter()
        .copied()
        .filter(|f| {
            let n = f.as_bytes();
            !haystacks.iter().any(|h| h.windows(n.len()).any(|w| w == n))
        })
        .collect()
}

/// `doctor --deep`: warns about Steam flags vapour passes that the installed client no longer contains.
fn check_flag_drift() -> Vec<CheckItem> {
    let Some(root) = find_steam_root() else {
        return Vec::new();
    };
    let files: Vec<Vec<u8>> = [
        "ubuntu12_32/steam",
        "ubuntu12_32/steamui.so",
        "linux64/steamclient.so",
    ]
    .iter()
    .filter_map(|rel| fs::read(root.join(rel)).ok())
    .collect();
    if files.is_empty() {
        return vec![CheckItem {
            name: "Steam flags".to_string(),
            status: CheckStatus::Warn,
            message: "no Steam client binaries found to scan".to_string(),
            details: None,
        }];
    }
    let hay: Vec<&[u8]> = files.iter().map(|f| f.as_slice()).collect();
    let missing = missing_flags(crate::launcher::STEAM_DAEMON_FLAGS, &hay);
    if missing.is_empty() {
        return vec![CheckItem {
            name: "Steam flags".to_string(),
            status: CheckStatus::Ok,
            message: "all flags vapour passes exist in the installed client".to_string(),
            details: None,
        }];
    }
    missing
        .into_iter()
        .map(|f| CheckItem {
            name: format!("Steam flag {f}"),
            status: CheckStatus::Warn,
            message: "flag not found in installed client; Steam may have removed it".to_string(),
            details: None,
        })
        .collect()
}

/// One line per kernel capability, detected by trying the feature. The kernel version above is informational.
fn check_capabilities() -> Vec<CheckItem> {
    let caps = crate::watcher::Caps::get();
    let item = |name: &str, ok: bool, yes: &str, no: &str| CheckItem {
        name: name.to_string(),
        status: if ok {
            CheckStatus::Ok
        } else {
            CheckStatus::Warn
        },
        message: if ok { yes } else { no }.to_string(),
        details: None,
    };
    vec![
        item(
            "Cap: memory.reclaim",
            caps.reclaim,
            "writable in app.slice",
            "not writable in app.slice (needs Linux 5.19+ and a delegated user cgroup)",
        ),
        item(
            "Cap: pidfd",
            caps.pidfd,
            "pidfd_open works",
            "pidfd_open failed; falling back to /proc polling",
        ),
        item(
            "Cap: PSI",
            caps.psi,
            "/proc/pressure/memory readable",
            "/proc/pressure/memory unreadable; adaptive reclaim disabled",
        ),
        item(
            "Cap: cgroup.events",
            caps.cgroup_events,
            "available",
            "unavailable; game exit uses PID polling",
        ),
        CheckItem {
            name: "Cap: swappiness=".to_string(),
            status: CheckStatus::Ok,
            message: match crate::watcher::Caps::reclaim_swappiness() {
                Some(true) => "accepted by memory.reclaim",
                Some(false) => "rejected by this kernel (plain reclaim used)",
                None => "probed on first reclaim (Linux 6.11+)",
            }
            .to_string(),
            details: None,
        },
    ]
}

fn check_kernel() -> CheckItem {
    let osrelease = fs::read_to_string("/proc/sys/kernel/osrelease")
        .unwrap_or_else(|_| "unknown".to_string())
        .trim()
        .to_string();

    let mut parts = osrelease.split('.');
    let major = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let minor_str = parts.next().unwrap_or("0");
    let minor = minor_str
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    let (status, verdict) = classify_kernel(major, minor);
    CheckItem {
        name: "Linux Kernel".to_string(),
        status,
        message: format!("Linux {osrelease} ({verdict})"),
        details: None,
    }
}

/// Kernel floors: `memory.reclaim` needs 5.19, the `swappiness=` bias needs 6.11.
fn classify_kernel(major: u32, minor: u32) -> (CheckStatus, &'static str) {
    if (major, minor) < (5, 19) {
        (CheckStatus::Fail, "memory.reclaim requires 5.19")
    } else if (major, minor) < (6, 11) {
        (
            CheckStatus::Warn,
            "memory.reclaim works; swappiness= bias needs 6.11",
        )
    } else {
        (
            CheckStatus::Ok,
            "memory.reclaim and swappiness= bias available",
        )
    }
}

fn check_cgroups() -> CheckItem {
    let controllers_file = Path::new("/sys/fs/cgroup/cgroup.controllers");
    if !controllers_file.exists() {
        return CheckItem {
            name: "Cgroups Hierarchy".to_string(),
            status: CheckStatus::Fail,
            message: "Cgroup v1 detected or cgroups not mounted".to_string(),
            details: Some(
                "Vapour requires unified cgroup v2. Boot with systemd.unified_cgroup_hierarchy=1"
                    .to_string(),
            ),
        };
    }

    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let user_slice_candidates = [
        format!("/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/memory.reclaim"),
        format!("/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/memory.reclaim"),
        format!("/sys/fs/cgroup/user.slice/user-{uid}.slice/app.slice/memory.reclaim"),
        format!("/sys/fs/cgroup/user.slice/user-{uid}.slice/memory.reclaim"),
    ];

    let mut writable_path = None;
    for cand in &user_slice_candidates {
        let p = Path::new(cand);
        if p.exists() {
            if let Ok(file) = fs::OpenOptions::new().write(true).open(p) {
                drop(file);
                writable_path = Some(p.to_path_buf());
                break;
            }
        }
    }

    if let Some(p) = writable_path {
        CheckItem {
            name: "Cgroups v2 Reclaim".to_string(),
            status: CheckStatus::Ok,
            message: format!("Writable user slice interface: {}", p.display()),
            details: None,
        }
    } else {
        CheckItem {
            name: "Cgroups v2 Reclaim".to_string(),
            status: CheckStatus::Warn,
            message: "Unified v2 active, but user slice memory.reclaim not directly pre-writable".to_string(),
            details: Some("Transient scopes via systemd-run will allocate writable delegates at daemon birth.".to_string()),
        }
    }
}

fn check_swap() -> CheckItem {
    let swaps_content = fs::read_to_string("/proc/swaps").unwrap_or_default();
    let mut total_swap_kb = 0u64;
    let mut zram_active = false;

    for line in swaps_content.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 4 {
            if let Ok(size) = cols[2].parse::<u64>() {
                total_swap_kb += size;
            }
            if cols[0].contains("zram") {
                zram_active = true;
            }
        }
    }

    if !zram_active && Path::new("/sys/block/zram0").exists() {
        zram_active = true;
    }

    let swappiness = fs::read_to_string("/proc/sys/vm/swappiness")
        .unwrap_or_default()
        .trim()
        .to_string();

    let total_mb = total_swap_kb / 1024;

    if total_mb > 0 {
        let zram_tag = if zram_active {
            "zRAM active"
        } else {
            "Physical swap"
        };
        let swappiness_num = swappiness.parse::<u32>().unwrap_or(60);
        let details = if zram_active && swappiness_num < 100 {
            Some(format!(
                "vm.swappiness is {swappiness_num}. For zRAM, swappiness >= 100 or 150 is recommended (sysctl vm.swappiness=150) to aggressively page out idle Steam background webviews."
            ))
        } else {
            None
        };
        CheckItem {
            name: "Memory / Swap".to_string(),
            status: CheckStatus::Ok,
            message: format!("{total_mb} MB configured ({zram_tag}, swappiness: {swappiness})"),
            details,
        }
    } else {
        CheckItem {
            name: "Memory / Swap".to_string(),
            status: CheckStatus::Warn,
            message: "0 MB Swap / zRAM configured".to_string(),
            details: Some(
                "Without swap or zRAM, memory.reclaim cannot page out cold Steam heap. Configure zRAM for optimal memory reclamation."
                    .to_string(),
            ),
        }
    }
}

fn check_polkit() -> CheckItem {
    let udisksctl_found = is_in_path("udisksctl")
        || Command::new("udisksctl")
            .arg("help")
            .output()
            .map(|o| o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty())
            .unwrap_or(false);

    if udisksctl_found {
        CheckItem {
            name: "Storage / Polkit".to_string(),
            status: CheckStatus::Ok,
            message: "udisksctl available (unprivileged drive mounting ready)".to_string(),
            details: None,
        }
    } else {
        CheckItem {
            name: "Storage / Polkit".to_string(),
            status: CheckStatus::Warn,
            message: "udisksctl not found in PATH".to_string(),
            details: Some(
                "Install 'udisks2' to allow non-root mounting of external game drives.".to_string(),
            ),
        }
    }
}

fn check_valve_ipc() -> CheckItem {
    let is_running = is_steam_running();
    let is_pipe_ready = is_steam_pipe_open();
    let is_shm_ready = is_valve_ipc_ready();

    if is_running && is_pipe_ready {
        CheckItem {
            name: "Steam IPC Daemon".to_string(),
            status: CheckStatus::Ok,
            message: "Steam daemon active and pipe listening (~/.steam/steam.pipe)".to_string(),
            details: Some(format!(
                "Shared Memory IPC: {}",
                if is_shm_ready { "Ready" } else { "Pending" }
            )),
        }
    } else if is_running {
        CheckItem {
            name: "Steam IPC Daemon".to_string(),
            status: CheckStatus::Warn,
            message: "Steam process detected, but pipe is not accepting writes".to_string(),
            details: Some("Steam may be launching, updating, or shutting down.".to_string()),
        }
    } else {
        let steam_bin = resolve_steam_bin();
        CheckItem {
            name: "Steam IPC Daemon".to_string(),
            status: CheckStatus::Ok,
            message: format!(
                "Daemon stopped (0 MB RAM). Launch binary verified: {}",
                steam_bin.display()
            ),
            details: None,
        }
    }
}

/// Reports whether game scoping is opted in (this only reads the environment; it does not prove Steam honours it).
fn wrapper_scope_note() -> String {
    let in_env = std::env::var("STEAM_LAUNCH_WRAPPER_SCOPE").is_ok();
    let in_conf = crate::library::home_dir()
        .map(|h| h.join(".config/environment.d/vapour.conf"))
        .and_then(|p| fs::read_to_string(p).ok())
        .is_some_and(|c| c.contains("STEAM_LAUNCH_WRAPPER_SCOPE"));
    if in_env || in_conf {
        "STEAM_LAUNCH_WRAPPER_SCOPE is set (environment or environment.d/vapour.conf).".to_string()
    } else {
        "STEAM_LAUNCH_WRAPPER_SCOPE is not set; a Steam started outside vapour may not isolate games into scopes.".to_string()
    }
}

fn check_container_runtime() -> CheckItem {
    let has_systemd_run = is_in_path("systemd-run")
        || Command::new("systemd-run")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

    let steam_root = find_steam_root();
    let has_runtime = steam_root
        .as_ref()
        .map(|r| {
            r.join("ubuntu12_32/steam-runtime").exists()
                || r.join("ubuntu12_64/steam-runtime").exists()
                || r.join("steamrt64").exists()
                || r.join("steamapps/common")
                    .read_dir()
                    .map(|entries| {
                        entries.flatten().any(|e| {
                            e.file_name()
                                .to_string_lossy()
                                .contains("SteamLinuxRuntime")
                        })
                    })
                    .unwrap_or(false)
        })
        .unwrap_or(false);

    let is_64bit = steam_root
        .as_ref()
        .map(|r| {
            r.join("steamrt64/steam").exists()
                && !r.join(".steam-disable-steamrt64-client").exists()
        })
        .unwrap_or(false);

    if has_systemd_run && has_runtime {
        if is_64bit {
            CheckItem {
                name: "Container Isolation".to_string(),
                status: CheckStatus::Ok,
                message: "systemd-run available and 64-bit Steam client (steamrt64) installed"
                    .to_string(),
                details: Some(format!("steamrt64/steam found. {}", wrapper_scope_note())),
            }
        } else {
            CheckItem {
                name: "Container Isolation".to_string(),
                status: CheckStatus::Ok,
                message: "systemd-run available and a Steam Linux Runtime is installed".to_string(),
                details: Some(wrapper_scope_note()),
            }
        }
    } else if has_systemd_run {
        CheckItem {
            name: "Container Isolation".to_string(),
            status: CheckStatus::Ok,
            message: "systemd-run available (no Steam Linux Runtime found)".to_string(),
            details: Some(wrapper_scope_note()),
        }
    } else {
        CheckItem {
            name: "Container Isolation".to_string(),
            status: CheckStatus::Warn,
            message: "systemd-run unavailable in session".to_string(),
            details: Some(
                "Games will run inside Steam's cgroup instead of an isolated container scope."
                    .to_string(),
            ),
        }
    }
}

fn check_active_profile() -> CheckItem {
    let active_dirs = get_active_user_data_dirs();
    if !active_dirs.is_empty() {
        let first = &active_dirs[0];
        let account_id = first
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        CheckItem {
            name: "Active Steam User".to_string(),
            status: CheckStatus::Ok,
            message: format!("AccountID: {} ({})", account_id, first.display()),
            details: None,
        }
    } else {
        CheckItem {
            name: "Active Steam User".to_string(),
            status: CheckStatus::Warn,
            message: "No active Steam user profile discovered in userdata".to_string(),
            details: Some(
                "Log in to Steam at least once to create a valid userdata profile.".to_string(),
            ),
        }
    }
}

fn check_compatibility_tools() -> CheckItem {
    let tools = find_available_protons();
    if !tools.is_empty() {
        let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        CheckItem {
            name: "Proton Runners".to_string(),
            status: CheckStatus::Ok,
            message: format!("{} compatibility runner(s) found", tools.len()),
            details: Some(format!("Discovered: {}", names.join(", "))),
        }
    } else {
        CheckItem {
            name: "Proton Runners".to_string(),
            status: CheckStatus::Warn,
            message: "No Proton installations discovered in Steam or compatibilitytools.d"
                .to_string(),
            details: Some(
                "Direct non-Steam Windows execution requires GE-Proton or Valve Proton."
                    .to_string(),
            ),
        }
    }
}

fn check_wrappers() -> CheckItem {
    let candidates = ["gamemoderun", "mangohud", "gamescope"];
    let found: Vec<&str> = candidates.into_iter().filter(|c| is_in_path(c)).collect();

    if !found.is_empty() {
        CheckItem {
            name: "Gaming Wrappers".to_string(),
            status: CheckStatus::Ok,
            message: format!("Detected: {}", found.join(", ")),
            details: None,
        }
    } else {
        CheckItem {
            name: "Gaming Wrappers".to_string(),
            status: CheckStatus::Ok,
            message: "None detected (gamemoderun, mangohud, gamescope)".to_string(),
            details: Some("Optional: install gamemode, mangohud, or gamescope if configured in settings.toml.".to_string()),
        }
    }
}

#[cfg(test)]
mod kernel_tests {
    use super::*;

    #[test]
    fn missing_flags_should_report_only_absent_flags() {
        let hay: [&[u8]; 2] = [b"xx -silent yy", b"-nojoy"];
        assert_eq!(
            missing_flags(&["-silent", "-nojoy", "-gone"], &hay),
            vec!["-gone"]
        );
    }

    #[test]
    fn classify_kernel_should_use_reclaim_and_swappiness_floors() {
        assert_eq!(classify_kernel(5, 14).0, CheckStatus::Fail);
        assert_eq!(classify_kernel(5, 19).0, CheckStatus::Warn);
        assert_eq!(classify_kernel(6, 10).0, CheckStatus::Warn);
        assert_eq!(classify_kernel(6, 11).0, CheckStatus::Ok);
        assert_eq!(classify_kernel(7, 3).0, CheckStatus::Ok);
    }
}
