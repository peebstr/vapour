use std::ffi::{c_char, c_int, c_long, c_ulong, c_void, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::config::DaemonConfig;
use crate::error::{VapourError, VapourResult};
use crate::launcher::is_steam_running;
use crate::session::{
    decide_on_exit, steam_origin, ExitCfg, GameSession, OnExit, SessionEnd, SessionPolicy,
    SteamOrigin,
};

/// Daemon reclaim schedule: immediately on detection, then deep compaction at T+25 s.
const DAEMON_RECLAIM_AT: &[Duration] = &[Duration::from_secs(0), Duration::from_secs(25)];

/// Embedded 512x512 PNG branding mark for desktop notifications and system icons.
pub static VAPOUR_LOGO_PNG: &[u8] = include_bytes!("../assets/vapour-logo.png");

/// Ensures the Vapour icon exists in the user's local XDG icon cache (`~/.local/share/icons/hicolor/512x512/apps/vapour.png`)
/// and `~/.local/share/vapour/vapour-logo.png`, returning the path to pass to notify-send.
pub fn ensure_vapour_icon() -> Option<PathBuf> {
    let home = crate::library::home_dir()?;
    let icon_dir = home.join(".local/share/icons/hicolor/512x512/apps");
    let icon_path = icon_dir.join("vapour.png");

    if !icon_path.exists() {
        let _ = fs::create_dir_all(&icon_dir);
        let _ = crate::non_steam::write_atomic(&icon_path, VAPOUR_LOGO_PNG);
    }

    if icon_path.exists() {
        Some(icon_path)
    } else {
        let fallback_dir = home.join(".local/share/vapour");
        let fallback_path = fallback_dir.join("vapour-logo.png");
        if !fallback_path.exists() {
            let _ = fs::create_dir_all(&fallback_dir);
            let _ = crate::non_steam::write_atomic(&fallback_path, VAPOUR_LOGO_PNG);
        }
        if fallback_path.exists() {
            Some(fallback_path)
        } else {
            None
        }
    }
}

/// Escapes special XML / Pango markup characters to ensure safe rendering in notify-send.
pub fn escape_markup(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Sends a desktop notification via standard Freedesktop / notify-send.
/// Safely no-ops if notify-send is not installed or the session bus is unreachable.
/// Reaps the child process in a background thread to prevent zombie (<defunct>) accumulation.
pub fn send_notification(summary: &str, body: &str, icon: Option<&str>) {
    let safe_summary = escape_markup(summary);
    let safe_body = escape_markup(body);
    let ic = icon
        .map(|s| s.to_string())
        .or_else(|| ensure_vapour_icon().map(|p| p.to_string_lossy().to_string()))
        .unwrap_or_else(|| "steam".to_string());

    thread::spawn(move || {
        let mut cmd = Command::new("notify-send");
        cmd.arg("-a").arg("Vapour");
        cmd.arg("-i").arg(ic);
        cmd.arg("--").arg(safe_summary).arg(safe_body);
        if let Ok(mut child) = cmd.spawn() {
            let _ = child.wait();
        }
    });
}

// ============================================================================
// CLI Session Supervision Guard (Mutual Exclusion Protocol)
// ============================================================================

/// RAII session guard acquired by the CLI (`vapour launch` / `vapour <game>`).
/// Prevents the background autopilot supervisor daemon from interfering with
/// in-flight CLI launches, window state, or lifecycle management.
#[derive(Debug)]
pub struct CliSessionGuard {
    path: PathBuf,
}

impl CliSessionGuard {
    pub fn acquire(appid: u32) -> Self {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let path = PathBuf::from(format!("/dev/shm/u{uid}-vapour-cli-{appid}.pid"));
        let pid = std::process::id();
        // Exempt from write_atomic: ephemeral /dev/shm session lock, gone on reboot by design.
        let _ = fs::write(&path, pid.to_string());
        Self { path }
    }
}

impl Drop for CliSessionGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Checks whether an AppID is currently being supervised by an active vapour CLI instance.
pub fn is_cli_supervised(appid: u32) -> bool {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let path = PathBuf::from(format!("/dev/shm/u{uid}-vapour-cli-{appid}.pid"));
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            if Path::new(&format!("/proc/{pid}")).exists() {
                return true;
            }
        }
        // Clean up stale marker file if CLI process died unexpectedly
        let _ = fs::remove_file(&path);
    }
    false
}

// ============================================================================
// Dynamic X11 Wire Protocol Controller (Zero Link-Time Dependencies)
// ============================================================================

#[repr(C)]
struct XClientMessageEvent {
    r#type: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut c_void,
    window: c_ulong,
    message_type: c_ulong,
    format: c_int,
    data: [c_long; 5],
}

type XOpenDisplayFn = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type XCloseDisplayFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type XDefaultRootWindowFn = unsafe extern "C" fn(*mut c_void) -> c_ulong;
type XInternAtomFn = unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_ulong;
type XSendEventFn = unsafe extern "C" fn(*mut c_void, c_ulong, c_int, c_long, *mut c_void) -> c_int;
type XFlushFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type XFreeFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type XGetWindowPropertyFn = unsafe extern "C" fn(
    *mut c_void,
    c_ulong,
    c_ulong,
    c_long,
    c_long,
    c_int,
    c_ulong,
    *mut c_ulong,
    *mut c_int,
    *mut c_ulong,
    *mut c_ulong,
    *mut *mut c_char,
) -> c_int;
type XErrorHandlerFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type XSetErrorHandlerFn = unsafe extern "C" fn(Option<XErrorHandlerFn>) -> Option<XErrorHandlerFn>;
type XSyncFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type XGetWMProtocolsFn =
    unsafe extern "C" fn(*mut c_void, c_ulong, *mut *mut c_ulong, *mut c_int) -> c_int;

/// Swallows X protocol errors (e.g. BadWindow when a window vanishes mid-scan). Xlib's default
/// handler calls `exit()`, which would kill the whole daemon.
unsafe extern "C" fn ignore_x_error(_dpy: *mut c_void, _ev: *mut c_void) -> c_int {
    0
}

type XQueryTreeFn = unsafe extern "C" fn(
    *mut c_void,
    c_ulong,
    *mut c_ulong,
    *mut c_ulong,
    *mut *mut c_ulong,
    *mut std::ffi::c_uint,
) -> c_int;

struct X11Lib {
    handle: *mut c_void,
    open_display: XOpenDisplayFn,
    close_display: XCloseDisplayFn,
    default_root_window: XDefaultRootWindowFn,
    intern_atom: XInternAtomFn,
    send_event: XSendEventFn,
    flush: XFlushFn,
    free: XFreeFn,
    get_window_property: XGetWindowPropertyFn,
    query_tree: XQueryTreeFn,
    set_error_handler: XSetErrorHandlerFn,
    sync: XSyncFn,
    get_wm_protocols: XGetWMProtocolsFn,
}

impl Drop for X11Lib {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: handle was returned non-null by dlopen in try_load and is closed exactly once, here.
            unsafe {
                libc::dlclose(self.handle);
            }
        }
    }
}

impl X11Lib {
    fn try_load() -> Option<Self> {
        // SAFETY: dlopen/dlsym get valid NUL-terminated CStrings and null results are rejected; each symbol is transmuted to the exact libX11 prototype it is looked up under, and the handle outlives them via X11Lib::drop.
        unsafe {
            let lib_name = CString::new("libX11.so.6").ok()?;
            let handle = libc::dlopen(lib_name.as_ptr(), libc::RTLD_LAZY);
            if handle.is_null() {
                return None;
            }

            let load_sym = |name: &str| -> Option<*mut c_void> {
                let sym_name = CString::new(name).ok()?;
                let sym = libc::dlsym(handle, sym_name.as_ptr());
                if sym.is_null() {
                    None
                } else {
                    Some(sym)
                }
            };

            Some(Self {
                handle,
                open_display: std::mem::transmute::<*mut c_void, XOpenDisplayFn>(load_sym(
                    "XOpenDisplay",
                )?),
                close_display: std::mem::transmute::<*mut c_void, XCloseDisplayFn>(load_sym(
                    "XCloseDisplay",
                )?),
                default_root_window: std::mem::transmute::<*mut c_void, XDefaultRootWindowFn>(
                    load_sym("XDefaultRootWindow")?,
                ),
                intern_atom: std::mem::transmute::<*mut c_void, XInternAtomFn>(load_sym(
                    "XInternAtom",
                )?),
                send_event: std::mem::transmute::<*mut c_void, XSendEventFn>(load_sym(
                    "XSendEvent",
                )?),
                flush: std::mem::transmute::<*mut c_void, XFlushFn>(load_sym("XFlush")?),
                free: std::mem::transmute::<*mut c_void, XFreeFn>(load_sym("XFree")?),
                get_window_property: std::mem::transmute::<*mut c_void, XGetWindowPropertyFn>(
                    load_sym("XGetWindowProperty")?,
                ),
                query_tree: std::mem::transmute::<*mut c_void, XQueryTreeFn>(load_sym(
                    "XQueryTree",
                )?),
                set_error_handler: std::mem::transmute::<*mut c_void, XSetErrorHandlerFn>(
                    load_sym("XSetErrorHandler")?,
                ),
                sync: std::mem::transmute::<*mut c_void, XSyncFn>(load_sym("XSync")?),
                get_wm_protocols: std::mem::transmute::<*mut c_void, XGetWMProtocolsFn>(load_sym(
                    "XGetWMProtocols",
                )?),
            })
        }
    }
}

/// Checks whether an X11 window belongs to a game binary, overlay, or in-flight launch modal.
/// Used to prevent sending WM_DELETE_WINDOW to dialogs or active game surfaces.
pub fn is_launch_or_game_window(
    class_str: &str,
    title_str: Option<&str>,
    target_game: Option<&str>,
    target_appid: Option<u32>,
) -> bool {
    let lower_class = class_str.to_lowercase();
    if lower_class.contains("steam_app")
        || lower_class.contains("gamescope")
        || lower_class.contains("gameoverlayui")
    {
        return true;
    }

    if let Some(t) = title_str {
        let lower_t = t.to_lowercase();
        if lower_t.contains("updating")
            || lower_t.contains("launching")
            || lower_t.contains("preparing")
            || lower_t.contains("starting")
            || lower_t.contains("installing")
            || lower_t.contains("verifying")
            || lower_t.contains("connecting")
            || lower_t.contains("cloud sync")
            || lower_t.contains("crash")
            || lower_t.contains("warning")
            || target_game.is_some_and(|g| lower_t.contains(&g.to_lowercase()))
            || target_appid.is_some_and(|id| lower_t.contains(&id.to_string()))
        {
            return true;
        }
    }

    false
}

/// Queries the X11 / Xwayland server on $DISPLAY for open Steam client windows and unmaps them
/// into the system tray via standard WM_DELETE_WINDOW client messages without terminating Steam.
///
/// Fully desktop-agnostic: operates identically across all Wayland compositors (Hyprland, Sway, KDE, GNOME, Cosmic)
/// and traditional X11 window managers via the Xwayland bridge without maintaining per-compositor IPC code.
/// Supports both traditional EWMH (_NET_CLIENT_LIST) and Xwayland/window tree discovery (XQueryTree).
///
/// Guarantees immunity for modal dialogs (_NET_WM_WINDOW_TYPE_DIALOG), startup splash screens,
/// in-flight update verification prompts, and active game windows.
/// Returns Ok(count_of_windows_closed).
pub fn close_steam_gui_to_tray(
    target_game_name: Option<&str>,
    target_appid: Option<u32>,
) -> VapourResult<usize> {
    let display_var = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
    let Some(x11) = X11Lib::try_load() else {
        return Ok(0);
    };

    let c_display = CString::new(display_var)
        .map_err(|_| VapourError::Daemon("Invalid DISPLAY variable".into()))?;
    // SAFETY: dpy is checked non-null before use; every call matches libX11's documented prototype; property buffers come from XGetWindowProperty/XQueryTree with the reported item counts (nitems/nchildren) and are each released once with XFree; WM_PROTOCOLS arrays from XGetWMProtocols are read within the returned count and freed once with XFree; ignore_x_error has the XErrorHandler signature and is uninstalled (previous handler restored) after an XSync and before XCloseDisplay; the Display is closed before returning.
    unsafe {
        let dpy = (x11.open_display)(c_display.as_ptr());
        if dpy.is_null() {
            return Ok(0);
        }

        // Install before any window query so a vanishing window cannot kill the daemon.
        let prev_handler = (x11.set_error_handler)(Some(ignore_x_error));

        let root = (x11.default_root_window)(dpy);
        let atom_client_list = (x11.intern_atom)(dpy, c"_NET_CLIENT_LIST".as_ptr(), 0);
        let atom_wm_class = (x11.intern_atom)(dpy, c"WM_CLASS".as_ptr(), 0);
        let atom_wm_protocols = (x11.intern_atom)(dpy, c"WM_PROTOCOLS".as_ptr(), 0);
        let atom_wm_delete = (x11.intern_atom)(dpy, c"WM_DELETE_WINDOW".as_ptr(), 0);
        let atom_net_wm_name = (x11.intern_atom)(dpy, c"_NET_WM_NAME".as_ptr(), 0);
        let atom_wm_name = (x11.intern_atom)(dpy, c"WM_NAME".as_ptr(), 0);
        let atom_net_wm_window_type = (x11.intern_atom)(dpy, c"_NET_WM_WINDOW_TYPE".as_ptr(), 0);
        let atom_type_dialog = (x11.intern_atom)(dpy, c"_NET_WM_WINDOW_TYPE_DIALOG".as_ptr(), 0);
        let atom_type_splash = (x11.intern_atom)(dpy, c"_NET_WM_WINDOW_TYPE_SPLASH".as_ptr(), 0);
        let atom_type_notification =
            (x11.intern_atom)(dpy, c"_NET_WM_WINDOW_TYPE_NOTIFICATION".as_ptr(), 0);

        let mut actual_type = 0;
        let mut actual_format = 0;
        let mut nitems = 0;
        let mut bytes_after = 0;
        let mut prop_data: *mut c_char = std::ptr::null_mut();

        let status = (x11.get_window_property)(
            dpy,
            root,
            atom_client_list,
            0,
            1024,
            0,
            0, // AnyPropertyType
            &mut actual_type,
            &mut actual_format,
            &mut nitems,
            &mut bytes_after,
            &mut prop_data,
        );

        let mut candidate_windows: Vec<c_ulong> = Vec::new();

        if status == 0 && !prop_data.is_null() && actual_format == 32 && nitems > 0 {
            let slice = std::slice::from_raw_parts(prop_data.cast::<c_ulong>(), nitems as usize);
            candidate_windows.extend_from_slice(slice);
            (x11.free)(prop_data.cast());
        } else {
            if !prop_data.is_null() {
                (x11.free)(prop_data.cast());
            }
            // Fallback for Xwayland / non-EWMH: query the full window tree
            let mut root_ret = 0;
            let mut parent_ret = 0;
            let mut children: *mut c_ulong = std::ptr::null_mut();
            let mut nchildren: std::ffi::c_uint = 0;

            let tree_status = (x11.query_tree)(
                dpy,
                root,
                &mut root_ret,
                &mut parent_ret,
                &mut children,
                &mut nchildren,
            );

            if tree_status != 0 && !children.is_null() && nchildren > 0 {
                let slice = std::slice::from_raw_parts(children, nchildren as usize);
                candidate_windows.extend_from_slice(slice);
                (x11.free)(children.cast());
            }
        }

        let mut closed_count = 0;

        for win_id in candidate_windows {
            // 1. Query WM_CLASS
            let mut class_type = 0;
            let mut class_format = 0;
            let mut class_nitems = 0;
            let mut class_bytes_after = 0;
            let mut class_prop: *mut c_char = std::ptr::null_mut();

            let class_status = (x11.get_window_property)(
                dpy,
                win_id,
                atom_wm_class,
                0,
                1024,
                0,
                0,
                &mut class_type,
                &mut class_format,
                &mut class_nitems,
                &mut class_bytes_after,
                &mut class_prop,
            );

            if class_status != 0 || class_prop.is_null() || class_nitems == 0 {
                if !class_prop.is_null() {
                    (x11.free)(class_prop.cast());
                }
                continue;
            }

            let slice = std::slice::from_raw_parts(class_prop.cast::<u8>(), class_nitems as usize);
            let class_str = String::from_utf8_lossy(slice).to_lowercase();
            (x11.free)(class_prop.cast());

            // Target only Steam desktop client webviews (steam, steamwebhelper).
            // Reject games (steam_app_<id>), gamescope, and overlay UIs.
            let is_steam = class_str.contains("steam") || class_str.contains("steamwebhelper");
            if !is_steam {
                continue;
            }

            // 2. Query _NET_WM_WINDOW_TYPE: protect modal dialogs, splash screens, and notification bubbles
            let mut type_type = 0;
            let mut type_format = 0;
            let mut type_nitems = 0;
            let mut type_bytes_after = 0;
            let mut type_prop: *mut c_char = std::ptr::null_mut();

            let type_status = (x11.get_window_property)(
                dpy,
                win_id,
                atom_net_wm_window_type,
                0,
                32,
                0,
                0,
                &mut type_type,
                &mut type_format,
                &mut type_nitems,
                &mut type_bytes_after,
                &mut type_prop,
            );

            if type_status == 0 && !type_prop.is_null() && type_format == 32 && type_nitems > 0 {
                let atoms =
                    std::slice::from_raw_parts(type_prop.cast::<c_ulong>(), type_nitems as usize);
                let is_modal_or_splash = atoms.iter().any(|&a| {
                    a == atom_type_dialog || a == atom_type_splash || a == atom_type_notification
                });
                (x11.free)(type_prop.cast());
                if is_modal_or_splash {
                    continue; // Never send WM_DELETE_WINDOW to dialogs or splashes!
                }
            } else if !type_prop.is_null() {
                (x11.free)(type_prop.cast());
            }

            // 3. Query Window Title (_NET_WM_NAME or WM_NAME): protect active launch & update modals
            let mut title_opt: Option<String> = None;
            for atom_title in [atom_net_wm_name, atom_wm_name] {
                let mut title_type = 0;
                let mut title_format = 0;
                let mut title_nitems = 0;
                let mut title_bytes_after = 0;
                let mut title_prop: *mut c_char = std::ptr::null_mut();

                let title_status = (x11.get_window_property)(
                    dpy,
                    win_id,
                    atom_title,
                    0,
                    512,
                    0,
                    0,
                    &mut title_type,
                    &mut title_format,
                    &mut title_nitems,
                    &mut title_bytes_after,
                    &mut title_prop,
                );

                if title_status == 0 && !title_prop.is_null() && title_nitems > 0 {
                    let title_slice =
                        std::slice::from_raw_parts(title_prop.cast::<u8>(), title_nitems as usize);
                    let t = String::from_utf8_lossy(title_slice).to_lowercase();
                    (x11.free)(title_prop.cast());
                    title_opt = Some(t);
                    break;
                } else if !title_prop.is_null() {
                    (x11.free)(title_prop.cast());
                }
            }

            if is_launch_or_game_window(
                &class_str,
                title_opt.as_deref(),
                target_game_name,
                target_appid,
            ) {
                continue; // Skip in-flight launch, game, or update dialogs
            }

            // Only send WM_DELETE_WINDOW if the window lists it in WM_PROTOCOLS (ICCCM 4.2.8.1).
            let mut protos: *mut c_ulong = std::ptr::null_mut();
            let mut nprotos: c_int = 0;
            let got = (x11.get_wm_protocols)(dpy, win_id, &mut protos, &mut nprotos);
            let supports_delete = got != 0
                && !protos.is_null()
                && nprotos > 0
                && std::slice::from_raw_parts(protos, nprotos as usize).contains(&atom_wm_delete);
            if !protos.is_null() {
                (x11.free)(protos.cast());
            }
            if !supports_delete {
                continue;
            }

            // Window is verified to be the top-level main Steam desktop UI. Send WM_DELETE_WINDOW.
            let mut ev = XClientMessageEvent {
                r#type: 33, // ClientMessage
                serial: 0,
                send_event: 1,
                display: dpy,
                window: win_id,
                message_type: atom_wm_protocols,
                format: 32,
                data: [atom_wm_delete as c_long, 0, 0, 0, 0],
            };

            let res = (x11.send_event)(
                dpy,
                win_id,
                0,
                0, // No event mask
                std::ptr::addr_of_mut!(ev).cast(),
            );
            if res != 0 {
                closed_count += 1;
            }
        }

        (x11.flush)(dpy);
        // Drain pending replies/errors while our handler is still installed, then restore.
        (x11.sync)(dpy, 0);
        (x11.set_error_handler)(prev_handler);
        (x11.close_display)(dpy);

        Ok(closed_count)
    }
}

/// Restores the Steam GUI window back to the screen after a game completes.
pub fn restore_steam_gui() {
    thread::spawn(|| {
        if let Ok(mut child) = Command::new("steam").arg("steam://open/games").spawn() {
            let _ = child.wait();
        }
    });
}

// ============================================================================
// Event-Driven Scope & Game Detection
// ============================================================================

pub use crate::session::{
    cgroup_is_vapour_scope, detect_active_game_scope, is_steam_headless, should_declare_exit,
};

/// Identifies active game PIDs by scanning `/proc` for processes with SteamAppId or SteamGameId.
pub fn detect_game_pid_from_proc(expected_appid: Option<u32>) -> Option<(u32, u32)> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return None;
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = name_str.parse::<u32>() else {
            continue;
        };

        let environ_path = entry.path().join("environ");
        if let Ok(bytes) = fs::read(&environ_path) {
            let env_str = String::from_utf8_lossy(&bytes);
            for var in env_str.split('\0') {
                let id_val = var
                    .strip_prefix("SteamAppId=")
                    .or_else(|| var.strip_prefix("SteamGameId="))
                    .or_else(|| var.strip_prefix("STEAM_COMPAT_APP_ID="));
                if let Some(val) = id_val {
                    if let Ok(id) = val.parse::<u32>() {
                        if id != 0 && (expected_appid.is_none() || expected_appid == Some(id)) {
                            return Some((id, pid));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Resolves human-readable game title from an AppID using local ACF manifests or shortcuts.vdf.
pub fn resolve_game_name(appid: u32) -> String {
    if let Ok(games) = crate::library::get_installed_games(false) {
        for g in games {
            if g.appid == appid {
                return g.name;
            }
        }
    }
    let config = crate::config::VapourConfig::load();
    for ns in crate::non_steam::load_non_steam_games(&config) {
        if ns.appid == appid {
            return ns.name;
        }
    }
    format!("Steam Game {appid}")
}

// ============================================================================
// Autopilot Lifecycle Supervisor Core Loop
// ============================================================================

/// Runs the Autopilot Lifecycle Supervisor loop in the current thread.
pub fn run_supervisor(config: DaemonConfig, foreground: bool) -> VapourResult<()> {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    let _ = ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    });

    // Configure kernel auto-reaping for child processes to prevent zombie (<defunct>) accumulation
    // SAFETY: sa is zero-initialized (a valid sigaction), sa_mask is initialized by sigemptyset, and SIG_IGN with SA_NOCLDWAIT is a valid SIGCHLD disposition.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = libc::SIG_IGN;
        sa.sa_flags = libc::SA_NOCLDWAIT | libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGCHLD, &sa, std::ptr::null_mut());
    }

    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let app_slice = PathBuf::from(format!(
        "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice"
    ));

    if foreground {
        println!("\x1b[1;36m[VAPOUR DAEMON]\x1b[0m Autopilot Supervisor active.");
        println!("       Monitoring cgroups v2 at: {}", app_slice.display());
        println!(
            "       Auto-tray: {} | Notifications: {} | PSI Adaptive: {}",
            config.auto_tray_gui, config.notifications, config.psi_adaptive
        );
        println!("       Sleeping in event loop (0.0% CPU)...");
    }

    // Startup sweep: clean any stale shared memory from previous crashes if Steam is not running
    let startup_report = crate::prune::autoclean_session_leftovers();
    if foreground && startup_report.shm_files_removed > 0 {
        println!(
            "       Startup sweep: Purged {} stale Steam IPC shared memory buffer(s) ({}).",
            startup_report.shm_files_removed,
            crate::library::format_bytes(startup_report.shm_bytes_freed)
        );
    }

    if config.notifications {
        send_notification("Vapour", "Supervisor active", None);
    }

    // Set up inotify on app.slice for zero-polling instant scope creation events
    // SAFETY: inotify_init1 takes only flag constants; the result is checked for < 0 below.
    let inotify_fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if inotify_fd >= 0 && app_slice.is_dir() {
        if let Ok(path_c) = CString::new(app_slice.to_string_lossy().as_bytes()) {
            // SAFETY: inotify_fd is a valid descriptor (checked >= 0) and path_c is a live NUL-terminated CString.
            unsafe {
                libc::inotify_add_watch(inotify_fd, path_c.as_ptr(), libc::IN_CREATE);
            }
        }
    }

    while running.load(Ordering::SeqCst) {
        // Sweep any finished children to guarantee zero zombies
        // SAFETY: waitpid(-1, NULL, WNOHANG) never blocks and a null status pointer is allowed.
        unsafe { while libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) > 0 {} }

        // Wait on inotify or sleep 1.5 seconds
        if inotify_fd >= 0 {
            let mut pfd = libc::pollfd {
                fd: inotify_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: pfd is a valid, exclusively borrowed pollfd and nfds is 1.
            unsafe {
                libc::poll(&mut pfd, 1, 1500);
            }
            // Drain inotify buffer
            let mut buf = [0u8; 1024];
            // SAFETY: buf is a live, writable 1024-byte buffer and the length passed is its length.
            unsafe { while libc::read(inotify_fd, buf.as_mut_ptr().cast(), buf.len()) > 0 {} }
        } else {
            thread::sleep(Duration::from_millis(1500));
        }

        // Check for active game
        let detected = detect_active_game_scope(&app_slice)
            .map(|(id, _)| (id, None))
            .or_else(|| detect_game_pid_from_proc(None).map(|(id, pid)| (id, Some(pid))));

        let Some((appid, game_pid_opt)) = detected else {
            continue;
        };

        // If the game was launched via the CLI, the CLI instance is its primary lifecycle controller.
        // The background daemon yields completely: zero window interference, zero double-reclaim,
        // and zero competing shutdown races.
        if is_cli_supervised(appid) {
            if foreground {
                println!(
                    "\x1b[36m[SUPERVISOR]\x1b[0m AppID {} is actively managed by vapour CLI. Yielding daemon controller.",
                    appid
                );
            }
            while is_cli_supervised(appid) && running.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(1500));
            }
            continue;
        }

        // Found active game (launched externally via Steam GUI / desktop launcher)!
        let game_name = resolve_game_name(appid);
        let steam_active = is_steam_running();

        if foreground {
            println!(
                "\x1b[1;32m[INTERCEPT]\x1b[0m Detected game launch: {} (AppID {})",
                game_name, appid
            );
        }

        // Step 1: Tuck an adopted (user-started) Steam GUI to the tray, filtering out launch/update
        // dialogs. A vapour-owned Steam is headless and has no GUI to close.
        let origin = steam_origin();
        let mut had_gui = false;
        if config.auto_tray_gui && steam_active && origin == SteamOrigin::Adopted {
            if let Ok(closed) = close_steam_gui_to_tray(Some(&game_name), Some(appid)) {
                if closed > 0 {
                    had_gui = true;
                    if foreground {
                        println!(
                            "\x1b[36m[TRAY]\x1b[0m Sent a close request to {} Steam window(s); Steam hides them to the system tray.",
                            closed
                        );
                    }
                    thread::sleep(Duration::from_millis(200));
                }
            }
        }

        // Step 2: Supervise through the shared lifecycle engine.
        let on_exit = decide_on_exit(
            origin,
            had_gui,
            false,
            ExitCfg {
                shutdown_if_headless: config.shutdown_if_headless && steam_active,
                restore_gui_on_exit: config.restore_gui_on_exit,
            },
        );
        let policy = SessionPolicy {
            reclaim_at: DAEMON_RECLAIM_AT,
            psi_cap: if config.psi_adaptive { None } else { Some(0) },
            psi_cooldown: Duration::from_secs(10),
            tick: Duration::from_secs(2),
            on_exit,
            verbose: foreground,
        };

        let notify_first = std::cell::Cell::new(config.notifications);
        let notify_name = game_name.clone();
        let mut session = GameSession::attach(appid, &game_name, game_pid_opt).with_reclaim_hook(
            Box::new(move |res| {
                if notify_first.replace(false) {
                    let freed = crate::library::format_bytes(res.mem_freed);
                    send_notification(&notify_name, &format!("{freed} RAM reclaimed"), None);
                }
            }),
        );
        // Invariant 14: yield on every tick if a CLI launch takes over this AppID, or on shutdown.
        let keep_going = || running.load(Ordering::SeqCst) && !is_cli_supervised(appid);
        let end = session.run(&policy, &keep_going);

        if end == SessionEnd::Yielded {
            if !running.load(Ordering::SeqCst) {
                if foreground {
                    println!("\x1b[33m[DAEMON]\x1b[0m Termination requested during gameplay. Exiting cleanly without teardown.");
                }
                break;
            }
            if foreground {
                println!(
                    "\x1b[36m[SUPERVISOR]\x1b[0m AppID {} is now managed by vapour CLI. Yielding daemon controller.",
                    appid
                );
            }
            continue;
        }

        // Step 3: Exit already handled by the engine (cloud wait + on_exit); report it.
        match on_exit {
            OnExit::RestoreGui => {
                if foreground {
                    println!("\x1b[32m[RESTORE]\x1b[0m Restored Steam GUI window to screen.");
                }
                if config.notifications {
                    send_notification(&game_name, "Cloud synced. Steam restored.", None);
                }
            }
            OnExit::ShutdownSteam => {
                if foreground {
                    println!("\x1b[32m[SHUTDOWN]\x1b[0m Cleanly shut down headless Steam daemon (0 MB remaining).");
                }
                if config.notifications {
                    send_notification(&game_name, "Cloud synced. Steam closed.", None);
                }
            }
            OnExit::KeepRunning => {}
        }

        // Cool-down period before returning to idle
        thread::sleep(Duration::from_secs(2));
    }

    if inotify_fd >= 0 {
        // SAFETY: inotify_fd is >= 0 here, owned by this function, and not used after this close.
        unsafe {
            libc::close(inotify_fd);
        }
    }

    if foreground {
        println!("\x1b[36m[VAPOUR DAEMON]\x1b[0m Supervisor cleanly stopped.");
    }

    Ok(())
}

// ============================================================================
// Systemd User Service Installer / Uninstaller
// ============================================================================

/// Installs and activates the Vapour Autopilot Supervisor as a systemd user service.
pub fn install_systemd_service() -> VapourResult<()> {
    let current_exe = std::env::current_exe()
        .map_err(|e| VapourError::Daemon(format!("Failed to resolve current binary path: {e}")))?;

    let home = std::env::var("HOME")
        .map_err(|_| VapourError::Daemon("Cannot determine HOME directory".into()))?;
    let systemd_dir = PathBuf::from(&home).join(".config/systemd/user");
    fs::create_dir_all(&systemd_dir)
        .map_err(|e| VapourError::Daemon(format!("Failed to create systemd user dir: {e}")))?;

    let service_file = systemd_dir.join("vapour.service");

    let unit_content = format!(
        r#"[Unit]
Description=Vapour Game Lifecycle Supervisor
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart={} daemon --foreground
Restart=on-failure
RestartSec=3
Slice=app.slice

[Install]
WantedBy=graphical-session.target
"#,
        current_exe.display()
    );

    crate::non_steam::write_atomic(&service_file, unit_content.as_bytes())
        .map_err(|e| VapourError::Daemon(format!("Failed to write service unit file: {e}")))?;

    println!(
        "\x1b[32m[OK]\x1b[0m Created systemd user unit: {}",
        service_file.display()
    );

    // Configure systemd user session environment so even native Steam GUI launches isolate games into scopes
    let env_dir = PathBuf::from(&home).join(".config/environment.d");
    if let Ok(()) = fs::create_dir_all(&env_dir) {
        let env_file = env_dir.join("vapour.conf");
        let env_content = "# Auto-configured by Vapour Game Lifecycle Supervisor\n\
                           PRESSURE_VESSEL_SYSTEMD_SCOPE=1\n\
                           STEAM_LAUNCH_WRAPPER_SCOPE=1\n";
        if crate::non_steam::write_atomic(&env_file, env_content.as_bytes()).is_ok() {
            println!(
                "\x1b[32m[OK]\x1b[0m Configured session environment: {}",
                env_file.display()
            );
            let _ = Command::new("systemctl")
                .args([
                    "--user",
                    "set-environment",
                    "PRESSURE_VESSEL_SYSTEMD_SCOPE=1",
                    "STEAM_LAUNCH_WRAPPER_SCOPE=1",
                ])
                .status();
        }
    }

    // Reload systemd and enable service
    let reload = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    if let Ok(s) = reload {
        if !s.success() {
            eprintln!("\x1b[33m[WARN]\x1b[0m systemctl --user daemon-reload exited with error");
        }
    }

    let enable = Command::new("systemctl")
        .args(["--user", "enable", "--now", "vapour.service"])
        .status();

    match enable {
        Ok(s) if s.success() => {
            println!("\x1b[32m[SUCCESS]\x1b[0m vapour.service successfully enabled and started!");
            println!("          Autopilot is running in the background at 0.0% CPU.");
            println!("          Run 'vapour daemon --status' to inspect.");
        }
        _ => {
            eprintln!("\x1b[33m[WARN]\x1b[0m Failed to enable vapour.service automatically.");
            eprintln!("       Run manually: systemctl --user enable --now vapour.service");
        }
    }

    Ok(())
}

/// Disables and removes the Vapour systemd user service.
pub fn uninstall_systemd_service() -> VapourResult<()> {
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", "vapour.service"])
        .status();

    let home = std::env::var("HOME").unwrap_or_default();
    let service_file = PathBuf::from(&home).join(".config/systemd/user/vapour.service");
    if service_file.exists() {
        let _ = fs::remove_file(&service_file);
        println!(
            "\x1b[32m[OK]\x1b[0m Removed unit file: {}",
            service_file.display()
        );
    }

    let env_file = PathBuf::from(&home).join(".config/environment.d/vapour.conf");
    if env_file.exists() {
        let _ = fs::remove_file(&env_file);
        println!(
            "\x1b[32m[OK]\x1b[0m Removed session environment: {}",
            env_file.display()
        );
        let _ = Command::new("systemctl")
            .args([
                "--user",
                "unset-environment",
                "PRESSURE_VESSEL_SYSTEMD_SCOPE",
                "STEAM_LAUNCH_WRAPPER_SCOPE",
            ])
            .status();
    }

    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    println!("\x1b[32m[SUCCESS]\x1b[0m vapour.service has been uninstalled.");
    Ok(())
}

/// Prints current systemd user service status.
pub fn print_service_status() {
    let status = Command::new("systemctl")
        .args(["--user", "status", "vapour.service"])
        .status();

    if let Err(e) = status {
        eprintln!("\x1b[31m[ERROR]\x1b[0m Failed to query systemctl: {e}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cgroup_is_vapour_scope_should_match_only_vapour_scopes() {
        assert!(super::cgroup_is_vapour_scope(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/vapour-steam-123.scope\n"
        ));
        assert!(!super::cgroup_is_vapour_scope(
            "0::/user.slice/user-1000.slice/session-2.scope\n"
        ));
    }

    #[test]
    pub fn should_declare_exit_should_wait_when_replacement_found() {
        assert_eq!(super::should_declare_exit(false, true, 2), (false, 0));
    }

    #[test]
    pub fn should_declare_exit_should_exit_after_three_empty_ticks() {
        assert_eq!(super::should_declare_exit(false, false, 0), (false, 1));
        assert_eq!(super::should_declare_exit(false, false, 2), (true, 3));
    }
    use super::*;

    #[test]
    fn test_escape_markup() {
        assert_eq!(escape_markup("Counter-Strike 2"), "Counter-Strike 2");
        assert_eq!(
            escape_markup("Steam <Beta> & \"Special\""),
            "Steam &lt;Beta&gt; &amp; \"Special\""
        );
        assert_eq!(escape_markup("A > B"), "A &gt; B");
    }

    #[test]
    fn test_is_steam_headless_does_not_panic() {
        // Runs against live /proc without panicking
        let _ = is_steam_headless();
    }

    #[test]
    fn test_resolve_game_name_synthetic() {
        let name = resolve_game_name(730);
        assert!(!name.is_empty());
    }

    #[test]
    fn test_detect_game_pid_from_proc_synthetic() {
        // AppID 99999999 should not exist
        assert!(detect_game_pid_from_proc(Some(99999999)).is_none());
    }

    #[test]
    fn test_cli_session_guard_lifecycle() {
        let test_appid = 999998;
        assert!(!is_cli_supervised(test_appid));
        {
            let _guard = CliSessionGuard::acquire(test_appid);
            assert!(is_cli_supervised(test_appid));
        }
        assert!(!is_cli_supervised(test_appid));
    }

    #[test]
    fn test_is_launch_or_game_window_protection() {
        // Game windows must be protected
        assert!(is_launch_or_game_window("steam_app_730", None, None, None));
        assert!(is_launch_or_game_window("gamescope", None, None, None));
        assert!(is_launch_or_game_window("gameoverlayui", None, None, None));

        // Modal dialog titles must be protected
        assert!(is_launch_or_game_window(
            "steam",
            Some("Updating Counter-Strike 2"),
            None,
            None
        ));
        assert!(is_launch_or_game_window(
            "steam",
            Some("Preparing to launch Counter-Strike 2..."),
            None,
            None
        ));
        assert!(is_launch_or_game_window(
            "steam",
            Some("Verifying installation"),
            None,
            None
        ));
        assert!(is_launch_or_game_window(
            "steam",
            Some("Counter-Strike 2"),
            Some("Counter-Strike 2"),
            Some(730)
        ));

        // Ordinary Steam client window should NOT be blocked
        assert!(!is_launch_or_game_window(
            "steamwebhelper",
            Some("Steam"),
            Some("Counter-Strike 2"),
            Some(730)
        ));
        assert!(!is_launch_or_game_window(
            "steam",
            Some("Friends List"),
            Some("Counter-Strike 2"),
            Some(730)
        ));
    }

    #[test]
    fn test_ensure_vapour_icon_creates_file_and_returns_path() {
        let icon_path = ensure_vapour_icon();
        assert!(icon_path.is_some());
        let path = icon_path.unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().contains("vapour.png"));
    }
}
