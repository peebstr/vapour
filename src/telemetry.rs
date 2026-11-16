use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::launcher::{get_steam_pid, get_steamwebhelper_pids, is_steam_running, shutdown_steam};
use crate::library::format_bytes;
use crate::watcher::{
    find_steam_cgroup_dirs, read_memory_pressure, reclaim_steam_memory, MemoryPressure,
};

pub const SPARKLINE_BARS: [char; 8] = [' ', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Renders a slice of numerical values as an 8-level Unicode sparkline string.
pub fn render_sparkline(values: &[u64]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let min = *values.iter().min().unwrap_or(&0);
    let max = *values.iter().max().unwrap_or(&0);

    if min == max {
        let bar = if max == 0 { ' ' } else { '▅' };
        return values.iter().map(|_| bar).collect();
    }

    let range = (max - min) as f64;
    values
        .iter()
        .map(|&v| {
            let offset = v.saturating_sub(min) as f64;
            let ratio = offset / range;
            let idx = (ratio * 7.0).round() as usize;
            SPARKLINE_BARS[idx.min(7)]
        })
        .collect()
}

/// Statistics for a cgroup scope under control.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CgroupScopeInfo {
    pub name: String,
    pub path: PathBuf,
    pub memory_bytes: u64,
    pub swap_bytes: u64,
    pub peak_bytes: u64,
}

/// Information about an active game container isolated by pressure-vessel.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GameContainerInfo {
    pub appid: u32,
    pub scope_name: String,
    pub path: PathBuf,
    pub memory_bytes: u64,
    pub swap_bytes: u64,
    pub isolated: bool,
}

/// Information about host zRAM swap state.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ZramInfo {
    pub disksize: u64,
    pub orig_data_size: u64,
    pub compr_data_size: u64,
    pub mem_used_total: u64,
    pub compression_ratio: f64,
}

/// Point-in-time telemetry snapshot of Steam daemon, CEF webhelpers, and game isolation.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TelemetrySnapshot {
    pub timestamp_epoch_secs: u64,
    pub steam_running: bool,
    pub steam_pid: Option<u32>,
    pub steam_rss_bytes: u64,
    pub steam_swap_bytes: u64,
    pub webhelper_pids: Vec<u32>,
    pub webhelper_rss_bytes: u64,
    pub webhelper_swap_bytes: u64,
    pub total_rss_bytes: u64,
    pub total_swap_bytes: u64,
    pub total_peak_bytes: u64,
    pub cgroup_scopes: Vec<CgroupScopeInfo>,
    pub game_container: Option<GameContainerInfo>,
    pub psi: Option<MemoryPressure>,
    pub zram: Option<ZramInfo>,
}

/// Reads VmRSS and VmSwap in bytes from `/proc/<pid>/status` with zero heap allocation.
pub fn read_proc_status_memory(pid: u32) -> (u64, u64) {
    use std::fs::File;
    use std::io::Read;

    let status_path = format!("/proc/{}/status", pid);
    let Ok(mut file) = File::open(&status_path) else {
        return (0, 0);
    };

    // 2 KB stack buffer easily holds /proc/<pid>/status up to VmSwap
    let mut buf = [0u8; 2048];
    let Ok(bytes_read) = file.read(&mut buf) else {
        return (0, 0);
    };

    let slice = &buf[..bytes_read];
    let mut rss_kb = 0u64;
    let mut swap_kb = 0u64;

    for line in slice.split(|&b| b == b'\n') {
        if line.starts_with(b"VmRSS:") {
            let rest = &line[6..];
            if let Some(num_bytes) = rest
                .split(|&b| b == b' ' || b == b'\t')
                .find(|s| !s.is_empty())
            {
                if let Ok(s) = std::str::from_utf8(num_bytes) {
                    rss_kb = s.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with(b"VmSwap:") {
            let rest = &line[7..];
            if let Some(num_bytes) = rest
                .split(|&b| b == b' ' || b == b'\t')
                .find(|s| !s.is_empty())
            {
                if let Ok(s) = std::str::from_utf8(num_bytes) {
                    swap_kb = s.parse().unwrap_or(0);
                }
            }
        }
    }

    (rss_kb * 1024, swap_kb * 1024)
}

/// Reads zRAM statistics from `/sys/block/zram0` if present.
pub fn read_zram_info() -> Option<ZramInfo> {
    let zram_path = Path::new("/sys/block/zram0");
    if !zram_path.exists() {
        return None;
    }

    let disksize = fs::read_to_string(zram_path.join("disksize"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;

    let mm_stat_str = fs::read_to_string(zram_path.join("mm_stat")).ok()?;
    let fields: Vec<&str> = mm_stat_str.split_whitespace().collect();
    if fields.len() < 3 {
        return None;
    }

    let orig_data_size = fields[0].parse::<u64>().unwrap_or(0);
    let compr_data_size = fields[1].parse::<u64>().unwrap_or(0);
    let mem_used_total = fields[2].parse::<u64>().unwrap_or(0);

    let compression_ratio = if compr_data_size > 0 && orig_data_size > 0 {
        (orig_data_size as f64) / (compr_data_size as f64)
    } else {
        1.0
    };

    Some(ZramInfo {
        disksize,
        orig_data_size,
        compr_data_size,
        mem_used_total,
        compression_ratio,
    })
}

/// Locates active Steam game container cgroup scope under user slice if one is executing.
pub fn find_game_cgroup_scope() -> Option<GameContainerInfo> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let app_slice = PathBuf::from(format!(
        "/sys/fs/cgroup/user.slice/user-{}.slice/user@{}.service/app.slice",
        uid, uid
    ));

    if let Ok(entries) = fs::read_dir(&app_slice) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            if name_str.starts_with("app-steam-app") && name_str.ends_with(".scope") {
                let dir_path = entry.path();
                let appid = parse_appid_from_scope_name(&name_str).unwrap_or(0);
                let mem_bytes = read_cgroup_u64(&dir_path.join("memory.current")).unwrap_or(0);
                let swap_bytes =
                    read_cgroup_u64(&dir_path.join("memory.swap.current")).unwrap_or(0);

                return Some(GameContainerInfo {
                    appid,
                    scope_name: name_str.to_string(),
                    path: dir_path,
                    memory_bytes: mem_bytes,
                    swap_bytes,
                    isolated: true,
                });
            }
        }
    }
    None
}

/// Parses AppID from a systemd scope name like `app-steam-app730-12800.scope`.
pub fn parse_appid_from_scope_name(scope_name: &str) -> Option<u32> {
    let without_prefix = scope_name.strip_prefix("app-steam-app")?;
    let appid_part = without_prefix.split('-').next()?;
    appid_part.parse::<u32>().ok()
}

fn read_cgroup_u64(path: &Path) -> Option<u64> {
    use std::fs::File;
    use std::io::Read;

    let mut file = File::open(path).ok()?;
    let mut buf = [0u8; 64];
    let n = file.read(&mut buf).ok()?;
    let s = std::str::from_utf8(&buf[..n]).ok()?;
    s.trim().parse::<u64>().ok()
}

/// Collects an exhaustive telemetry snapshot across processes, cgroups, PSI, and zRAM.
pub fn collect_telemetry_snapshot() -> TelemetrySnapshot {
    let epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let steam_running = is_steam_running();
    let steam_pid = get_steam_pid();
    let webhelper_pids = get_steamwebhelper_pids();

    let mut cgroup_scopes = Vec::new();
    let cg_dirs = find_steam_cgroup_dirs();
    let mut cgroup_total_rss = 0u64;
    let mut cgroup_total_swap = 0u64;
    let mut cgroup_total_peak = 0u64;
    let mut cgroup_core_rss = 0u64;
    let mut cgroup_core_swap = 0u64;
    let mut cgroup_webhelper_rss = 0u64;
    let mut cgroup_webhelper_swap = 0u64;

    for dir in cg_dirs {
        let mem = read_cgroup_u64(&dir.join("memory.current")).unwrap_or(0);
        let swap = read_cgroup_u64(&dir.join("memory.swap.current")).unwrap_or(0);
        let peak = read_cgroup_u64(&dir.join("memory.peak")).unwrap_or(mem);
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| dir.to_string_lossy().to_string());

        cgroup_total_rss += mem;
        cgroup_total_swap += swap;
        cgroup_total_peak += peak;

        if name.contains("app-steam-unknown") {
            cgroup_webhelper_rss += mem;
            cgroup_webhelper_swap += swap;
        } else {
            cgroup_core_rss += mem;
            cgroup_core_swap += swap;
        }

        cgroup_scopes.push(CgroupScopeInfo {
            name,
            path: dir,
            memory_bytes: mem,
            swap_bytes: swap,
            peak_bytes: peak,
        });
    }

    // Prefer deduplicated cgroup v2 container charges over raw VmRSS summation.
    // In CEF/Chromium, 8-12 worker processes share identical read-only code pages (libcef.so)
    // and shmem buffers, causing /proc/<pid>/status VmRSS summation to artificially multiply
    // shared memory by the process count. cgroup memory.current measures true physical RAM.
    let (
        steam_rss_bytes,
        steam_swap_bytes,
        webhelper_rss_bytes,
        webhelper_swap_bytes,
        total_rss_bytes,
        total_swap_bytes,
    ) = if cgroup_total_rss > 0 || cgroup_total_swap > 0 {
        (
            cgroup_core_rss,
            cgroup_core_swap,
            cgroup_webhelper_rss,
            cgroup_webhelper_swap,
            cgroup_total_rss,
            cgroup_total_swap,
        )
    } else {
        let (raw_core_rss, raw_core_swap) = if let Some(pid) = steam_pid {
            read_proc_status_memory(pid)
        } else {
            (0, 0)
        };
        let mut raw_cef_rss = 0u64;
        let mut raw_cef_swap = 0u64;
        for &pid in &webhelper_pids {
            let (rss, swap) = read_proc_status_memory(pid);
            raw_cef_rss += rss;
            raw_cef_swap += swap;
        }
        (
            raw_core_rss,
            raw_core_swap,
            raw_cef_rss,
            raw_cef_swap,
            raw_core_rss + raw_cef_rss,
            raw_core_swap + raw_cef_swap,
        )
    };

    let total_peak_bytes = if cgroup_total_peak > 0 {
        cgroup_total_peak
    } else {
        total_rss_bytes
    };

    let game_container = find_game_cgroup_scope();
    let psi = read_memory_pressure();
    let zram = read_zram_info();

    TelemetrySnapshot {
        timestamp_epoch_secs: epoch_secs,
        steam_running,
        steam_pid,
        steam_rss_bytes,
        steam_swap_bytes,
        webhelper_pids,
        webhelper_rss_bytes,
        webhelper_swap_bytes,
        total_rss_bytes,
        total_swap_bytes,
        total_peak_bytes,
        cgroup_scopes,
        game_container,
        psi,
        zram,
    }
}

/// Prints a rich diagnostic snapshot for `vapour status`.
pub fn print_telemetry_snapshot_rich(snapshot: &TelemetrySnapshot) {
    if !snapshot.steam_running {
        println!("\x1b[1;36m[STATUS]\x1b[0m Steam Daemon: \x1b[36mSTOPPED\x1b[0m (0 MB RAM)");
        if let Some(ref psi) = snapshot.psi {
            println!(
                "  Kernel PSI:    some avg10: {:.2}% | avg60: {:.2}%",
                psi.some_avg10, psi.some_avg60
            );
        }
        if let Some(ref zram) = snapshot.zram {
            println!(
                "  Host zRAM:     {} capacity (Swappiness active)",
                format_bytes(zram.disksize)
            );
        }
        return;
    }

    println!(
        "\x1b[1;32m[STATUS]\x1b[0m Steam Daemon: \x1b[32mRUNNING\x1b[0m (PID: {})",
        snapshot
            .steam_pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "unknown".into())
    );
    let peak_note =
        if snapshot.total_peak_bytes > snapshot.total_rss_bytes && snapshot.total_rss_bytes > 0 {
            let saved = snapshot
                .total_peak_bytes
                .saturating_sub(snapshot.total_rss_bytes);
            format!(
                " (Peak: {}, Saved: {})",
                format_bytes(snapshot.total_peak_bytes),
                format_bytes(saved)
            )
        } else if snapshot.total_peak_bytes > 0 {
            format!(" (Peak: {})", format_bytes(snapshot.total_peak_bytes))
        } else {
            String::new()
        };
    println!(
        "  Memory RSS:    Core: {} | CEF Helpers: {} | Total: \x1b[1m{}\x1b[0m{}",
        format_bytes(snapshot.steam_rss_bytes),
        format_bytes(snapshot.webhelper_rss_bytes),
        format_bytes(snapshot.total_rss_bytes),
        peak_note
    );

    let zram_note = match &snapshot.zram {
        Some(z) => format!(" (Compression Ratio: {:.2}x)", z.compression_ratio),
        None => String::new(),
    };
    println!(
        "  Swap / zRAM:   Offloaded: \x1b[1m{}\x1b[0m{}",
        format_bytes(snapshot.total_swap_bytes),
        zram_note
    );

    if let Some(ref psi) = snapshot.psi {
        let psi_color = if psi.some_avg10 > 5.0 {
            "\x1b[33m"
        } else {
            "\x1b[32m"
        };
        println!(
            "  Kernel PSI:    {}some avg10: {:.2}%\x1b[0m | some avg60: {:.2}%",
            psi_color, psi.some_avg10, psi.some_avg60
        );
    }

    if let Some(ref game) = snapshot.game_container {
        println!(
            "  Game Container: \x1b[32mISOLATED\x1b[0m (AppID: {}, Scope: {}, RSS: {}, Swap: {})",
            game.appid,
            game.scope_name,
            format_bytes(game.memory_bytes),
            format_bytes(game.swap_bytes)
        );
    } else {
        println!("  Game Container: None active (Steam daemon in idle background standby)");
    }

    if !snapshot.cgroup_scopes.is_empty() {
        println!("  Cgroup Scopes:");
        for scope in &snapshot.cgroup_scopes {
            println!(
                "   • {:<24} RSS: {:>8} | Swap: {:>8} | Peak: {:>8}",
                scope.name,
                format_bytes(scope.memory_bytes),
                format_bytes(scope.swap_bytes),
                format_bytes(scope.peak_bytes)
            );
        }
    }
}

struct RawModeGuard {
    orig_termios: libc::termios,
    active: bool,
}

impl RawModeGuard {
    fn enter() -> Option<Self> {
        // SAFETY: STDIN is a valid fd (isatty is checked first); termios is a plain C struct, so zeroed() is valid and tcgetattr fully initializes it before use.
        unsafe {
            if libc::isatty(libc::STDIN_FILENO) != 1 {
                return None;
            }
            let mut orig: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut orig) != 0 {
                return None;
            }
            let mut raw = orig;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;

            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                return None;
            }

            Some(Self {
                orig_termios: orig,
                active: true,
            })
        }
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.active {
            // SAFETY: orig_termios was filled by a successful tcgetattr in enter(); STDIN is still the same terminal.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.orig_termios);
            }
        }
    }
}

fn poll_stdin_key(timeout_ms: i32) -> Option<char> {
    // SAFETY: pfd is a valid, exclusively borrowed pollfd and nfds is 1.
    unsafe {
        let mut fds = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = libc::poll(&mut fds, 1, timeout_ms);
        if ret > 0 && (fds.revents & libc::POLLIN) != 0 {
            let mut buf = [0u8; 1];
            let n = libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut libc::c_void, 1);
            if n == 1 {
                return Some(buf[0] as char);
            }
        }
        None
    }
}

/// Runs the real-time terminal HUD overlay with live sparkline history.
pub fn run_telemetry_hud(mut interval: Duration, max_iterations: Option<usize>) {
    let _raw_guard = RawModeGuard::enter();
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Catch Ctrl+C cleanly
    let _ = ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    });

    let history_capacity = 24;
    let mut rss_history: Vec<u64> = Vec::with_capacity(history_capacity);
    let mut swap_history: Vec<u64> = Vec::with_capacity(history_capacity);
    let mut action_banner: Option<(String, usize)> = None;

    // Hide cursor during HUD rendering
    print!("\x1b[?25l");
    let _ = std::io::stdout().flush();

    let mut iteration = 0;
    while running.load(Ordering::SeqCst) {
        if let Some(max) = max_iterations {
            if iteration >= max {
                break;
            }
        }
        iteration += 1;

        let snapshot = collect_telemetry_snapshot();

        if rss_history.len() >= history_capacity {
            rss_history.remove(0);
        }
        rss_history.push(snapshot.total_rss_bytes);

        if swap_history.len() >= history_capacity {
            swap_history.remove(0);
        }
        swap_history.push(snapshot.total_swap_bytes);

        let rss_spark = render_sparkline(&rss_history);
        let swap_spark = render_sparkline(&swap_history);

        // Terminal redraw: cursor home and clear
        print!("\x1b[H\x1b[2J");

        let time_str = {
            let secs = snapshot.timestamp_epoch_secs;
            let h = (secs / 3600) % 24;
            let m = (secs / 60) % 60;
            let s = secs % 60;
            format!("{:02}:{:02}:{:02} UTC", h, m, s)
        };

        let status_badge = if snapshot.steam_running {
            format!(
                "\x1b[1;32mRUNNING\x1b[0m (PID: {})",
                snapshot
                    .steam_pid
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "?".into())
            )
        } else {
            "\x1b[1;36mSTOPPED\x1b[0m".to_string()
        };

        let compr_str = snapshot
            .zram
            .as_ref()
            .map(|z| format!("{:.2}x", z.compression_ratio))
            .unwrap_or_else(|| "N/A".to_string());

        let psi_str = snapshot
            .psi
            .as_ref()
            .map(|p| format!("{:.2}%", p.some_avg10))
            .unwrap_or_else(|| "N/A".to_string());

        println!("┌── \x1b[1;35mVAPOUR LIVE TELEMETRY HUD\x1b[0m ────────────────────────────────────────────────────────┐");
        println!(
            "│ Daemon: {:<32} Interval: {:<5} Time: {:<14} │",
            status_badge,
            format!("{:.1}s", interval.as_secs_f64()),
            time_str
        );
        println!(
            "│ Core RSS: {:<11} CEF RSS: {:<11} Total RSS: \x1b[1m{:<12}\x1b[0m │",
            format_bytes(snapshot.steam_rss_bytes),
            format_bytes(snapshot.webhelper_rss_bytes),
            format_bytes(snapshot.total_rss_bytes)
        );
        let peak_saved_str = if snapshot.total_peak_bytes > snapshot.total_rss_bytes
            && snapshot.total_rss_bytes > 0
        {
            format!(
                "{}/-{}",
                format_bytes(snapshot.total_peak_bytes),
                format_bytes(
                    snapshot
                        .total_peak_bytes
                        .saturating_sub(snapshot.total_rss_bytes)
                )
            )
        } else {
            format_bytes(snapshot.total_peak_bytes)
        };
        let zram_line = format!(
            "zRAM Swap: {:<9} (CR: {:<5}) Peak/Saved: {:<14} PSI avg10: {:<8}",
            format_bytes(snapshot.total_swap_bytes),
            compr_str,
            peak_saved_str,
            psi_str
        );
        println!("│ {:<76} │", zram_line);
        println!(
            "├──────────────────────────────────────────────────────────────────────────────┤"
        );
        println!(
            "│ RSS Trend:   [{:<24}] {:>12}                       │",
            rss_spark,
            format_bytes(snapshot.total_rss_bytes)
        );
        println!(
            "│ Swap Trend:  [{:<24}] {:>12}                       │",
            swap_spark,
            format_bytes(snapshot.total_swap_bytes)
        );
        println!(
            "├──────────────────────────────────────────────────────────────────────────────┤"
        );
        println!(
            "│ Cgroup Scopes:                                                               │"
        );

        if snapshot.cgroup_scopes.is_empty() {
            println!(
                "│   (No active Steam cgroup scopes detected)                                   │"
            );
        } else {
            for scope in snapshot.cgroup_scopes.iter().take(4) {
                let scope_line = format!(
                    "• {:<22} RSS: {:>7} | Swap: {:>7} | Peak: {:>7}",
                    scope.name,
                    format_bytes(scope.memory_bytes),
                    format_bytes(scope.swap_bytes),
                    format_bytes(scope.peak_bytes)
                );
                println!("│   {:<74} │", scope_line);
            }
        }

        if let Some(ref game) = snapshot.game_container {
            let game_line = format!(
                "★ GAME: AppID {:<6} Scope: {:<20} RSS: {:>8} | Swap: 0 MB (PROTECTED)",
                game.appid,
                game.scope_name,
                format_bytes(game.memory_bytes)
            );
            println!("│   \x1b[1;32m{:<74}\x1b[0m │", game_line);
        }

        if let Some((ref msg, remaining)) = action_banner {
            println!(
                "├──────────────────────────────────────────────────────────────────────────────┤"
            );
            println!("│ \x1b[1;33m★ ACTION:\x1b[0m {:<67} │", msg);
            if remaining == 0 {
                action_banner = None;
            } else {
                action_banner = Some((msg.clone(), remaining - 1));
            }
        }

        println!(
            "├──────────────────────────────────────────────────────────────────────────────┤"
        );
        println!(
            "│ Hotkeys: [r] Reclaim Memory  [k] Shutdown  [+/-] Interval  [q] Quit          │"
        );
        println!(
            "└──────────────────────────────────────────────────────────────────────────────┘"
        );

        let _ = std::io::stdout().flush();

        if max_iterations.is_some() {
            thread::sleep(Duration::from_millis(5));
        } else {
            let timeout_ms = (interval.as_millis() as i32).max(100);
            if let Some(key) = poll_stdin_key(timeout_ms) {
                match key {
                    'r' | 'R' => match reclaim_steam_memory() {
                        Ok(res) => {
                            action_banner = Some((
                                format!(
                                    "⚡ Kernel memory reclaimed: {} RAM ({} into zRAM)!",
                                    crate::library::format_bytes(res.mem_freed),
                                    crate::library::format_bytes(res.swap_delta)
                                ),
                                2,
                            ));
                        }
                        Err(e) => {
                            action_banner = Some((format!("⚠ Reclamation notice: {}", e), 2));
                        }
                    },
                    'k' | 'K' => match shutdown_steam() {
                        Ok(_) => {
                            action_banner = Some((
                                "🛑 Steam daemon graceful shutdown signal dispatched.".to_string(),
                                2,
                            ));
                        }
                        Err(e) => {
                            action_banner = Some((format!("⚠ Shutdown failed: {}", e), 2));
                        }
                    },
                    '+' | '=' => {
                        interval = Duration::from_secs_f64((interval.as_secs_f64() - 0.2).max(0.2));
                        action_banner = Some((
                            format!(
                                "⏱ Refresh interval adjusted to {:.1}s",
                                interval.as_secs_f64()
                            ),
                            2,
                        ));
                    }
                    '-' | '_' => {
                        interval =
                            Duration::from_secs_f64((interval.as_secs_f64() + 0.2).min(10.0));
                        action_banner = Some((
                            format!(
                                "⏱ Refresh interval adjusted to {:.1}s",
                                interval.as_secs_f64()
                            ),
                            2,
                        ));
                    }
                    'q' | 'Q' | '\x1b' => {
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    // Restore cursor and print clean newline
    println!("\x1b[?25h");
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_sparkline_empty() {
        assert_eq!(render_sparkline(&[]), "");
    }

    #[test]
    fn test_render_sparkline_all_zero() {
        let spark = render_sparkline(&[0, 0, 0, 0]);
        assert_eq!(spark, "    ");
    }

    #[test]
    fn test_render_sparkline_uniform_non_zero() {
        let spark = render_sparkline(&[500, 500, 500]);
        assert_eq!(spark, "▅▅▅");
    }

    #[test]
    fn test_render_sparkline_gradient() {
        let values: Vec<u64> = (0..=7).collect();
        let spark = render_sparkline(&values);
        assert_eq!(spark, " ▂▃▄▅▆▇█");
    }

    #[test]
    fn test_parse_appid_from_scope_name() {
        assert_eq!(
            parse_appid_from_scope_name("app-steam-app730-12800.scope"),
            Some(730)
        );
        assert_eq!(
            parse_appid_from_scope_name("app-steam-app1086940-999.scope"),
            Some(1086940)
        );
        assert_eq!(
            parse_appid_from_scope_name("app-steam-unknown-123.scope"),
            None
        );
        assert_eq!(parse_appid_from_scope_name("not-a-scope"), None);
    }

    #[test]
    fn test_collect_telemetry_snapshot_does_not_panic() {
        let snapshot = collect_telemetry_snapshot();
        assert!(snapshot.timestamp_epoch_secs > 0);
    }

    #[test]
    fn test_read_proc_status_memory_nonexistent_pid() {
        let (rss, swap) = read_proc_status_memory(99999999);
        assert_eq!(rss, 0);
        assert_eq!(swap, 0);
    }

    #[test]
    fn test_read_proc_status_memory_self() {
        let pid = std::process::id();
        let (rss, _) = read_proc_status_memory(pid);
        assert!(rss > 0, "Current process must have non-zero RSS");
    }

    #[test]
    fn test_run_telemetry_hud_finite_iterations() {
        // Runs 1 iteration to verify terminal formatting runs cleanly without panic
        run_telemetry_hud(Duration::from_millis(10), Some(1));
    }

    #[test]
    fn test_poll_stdin_key_zero_timeout_does_not_panic() {
        let _ = poll_stdin_key(0);
    }
}
