# AGENTS.md — AI Agent Architecture & Maintenance Protocol

> **DOCUMENTATION RULE FOR ALL AI AGENTS:**
> Update `README.md`, `CONTRIBUTING.md`, and this file (`AGENTS.md`) only when your change alters what they document: CLI flags, configuration parameters, module responsibilities, invariants, or architectural flows. Internal refactors and bug fixes that keep behavior unchanged need no doc edits.
> Read `ARCHITECTURE.md` only when working on the kernel/cgroup/Steam internals it covers.

---

## 1. System Philosophy & High-Level Architecture

**Vapour** (`vapour`) is a specialized Linux orchestration daemon and terminal controller designed to run Steam games while moving the Steam desktop client's idle memory out of RAM (into zRAM/swap).

Steam on Linux is bundled with Chromium Embedded Framework (CEF) webviews and background updater processes that keep over 1 GB resident (1213 MB in one measured run). Vapour replaces Steam's GUI with a high-performance terminal UI and headless daemon management layer.

Vapour implements a **Dual-Mode CLI Architecture**:
- **Interactive Terminal (Human-Centric)**: Runs interactive fuzzy game pickers (`inquire`), dynamic acronym scoring (`cs2` $\to$ Counter-Strike 2), and interactive configuration menus when connected to a TTY (`std::io::stdin().is_terminal()`).
- **Non-Interactive / Scriptable (Machine-Centric)**: Executes deterministically with zero prompts, emits clean machine-readable data (`--json`, `--dmenu`, `--plain`), and returns standardized exit codes for shell scripts, cron jobs, and tiling window manager keybindings (Hyprland, Sway, Rofi).

### System Flow
```
                           +---------------------------+
                           |         main.rs           |
                           | (CLI Dispatch & Selector) |
                           +-------------+-------------+
                                         |
             +---------------------------+---------------------------+
             |                           |                           |
             v                           v                           v
+-------------------------+ +-------------------------+ +-------------------------+
|      library.rs         | |      non_steam.rs       | |       doctor.rs         |
| ACF Manifest Discovery  | | Binary shortcuts.vdf    | | cgroups v2 & zRAM Check |
| localconfig.vdf Parser  | | CRC32 AppID Generator   | | Kernel / Polkit Health  |
+------------+------------+ +------------+------------+ +-------------------------+
             |                           |
             +-------------+-------------+
                           |
                           v
             +---------------------------+
             |         runner.rs         |
             | GameRunner Trait Factory  |
             | SteamDaemon / Shortcut    |
             | ProtonDirect / Native     |
             | Wrapper %command% Engine  |
             +-------------+-------------+
                           |
                           v
             +---------------------------+
             |        launcher.rs        |
             | Headless Steam Lifecycle  |
             | Non-blocking FIFO & Shm   |
             +-------------+-------------+
                           |
                           v
             +---------------------------+
             |        watcher.rs         |
             | cgroup v2 Memory Reclaim  |
             | Handoff Bridging & Cloud  |
             +---------------------------+
```

---

## 2. Codebase Map & Module Responsibilities

The codebase consists of 15 core modules located in [`src/`](src/):

| Module | Exact Responsibilities | Key Functions / Structs |
| :--- | :--- | :--- |
| [`src/main.rs`](src/main.rs) / [`src/cli/`](src/cli/) | Dual-Mode CLI parsing, subcommands (`cli/mod.rs`), modular launchers (`launch.rs`), library tables (`list.rs`), non-Steam wizard (`manage.rs`), system diagnostics/pruning (`system.rs`), and dynamic acronym scoring (`tui.rs`) | [`Cli`](src/cli/mod.rs), [`Commands`](src/cli/mod.rs), [`UnifiedGame`](src/cli/list.rs), [`extract_acronyms`](src/cli/tui.rs), [`is_interactive`](src/cli/mod.rs) |
| [`src/error.rs`](src/error.rs) | Strongly typed domain error enum (`thiserror`), POSIX exit code mappings, `VapourResult<T>` | [`VapourError`](src/error.rs), [`VapourResult`](src/error.rs), `exit_code` |
| [`src/doctor.rs`](src/doctor.rs) | Environment validation, cgroups v2 reclaim writable test, zRAM/swap inspection, 64-bit Steam runtime container validation, Polkit | [`DoctorReport`](src/doctor.rs), [`run_doctor_checks`](src/doctor.rs), [`CheckItem`](src/doctor.rs) |
| [`src/runner.rs`](src/runner.rs) | Pluggable `GameRunner` abstraction, wrapper pipeline, Steam `%command%` interpolation, target factory | [`GameRunner`](src/runner.rs), [`LaunchTarget`](src/runner.rs), [`SteamDaemonRunner`](src/runner.rs), [`ProtonDirectRunner`](src/runner.rs), [`compose_command_tokens`](src/runner.rs) |
| [`src/launcher.rs`](src/launcher.rs) | Steam client lifecycle, headless flag injection (Breakpad/minidump suppression), non-blocking POSIX FIFO probing, birth scoping | [`launch_game`](src/launcher.rs), [`is_steam_pipe_open`](src/launcher.rs), [`is_valve_ipc_ready`](src/launcher.rs), [`ensure_steam_daemon`](src/launcher.rs) |
| [`src/session.rs`](src/session.rs) | Shared lifecycle engine used by the CLI and the daemon: scope/pidfd exit detection, cgroup v2 reclaim, PSI triggers, Steam Cloud exit-sync state machine, pidfd signalling, `Owned`/`Adopted` exit policy | [`GameSession`](src/session.rs), [`SessionPolicy`](src/session.rs), [`decide_on_exit`](src/session.rs), [`reclaim_steam_memory`](src/session.rs), [`cloud_step`](src/session.rs), [`PidFd`](src/session.rs), [`Caps`](src/session.rs) |
| [`src/watcher.rs`](src/watcher.rs) | CLI driver: update/launch progress from `content_log.txt`, then hands the running game to `session.rs`; re-exports `session::*` | [`watch_and_cleanup`](src/watcher.rs), [`is_relevant_update`](src/watcher.rs) |
| [`src/telemetry.rs`](src/telemetry.rs) | Telemetry snapshot aggregation (uses `steam_cgroup_dirs_unfiltered` and subtracts estimated game-wrapper RSS from the Steam totals), `memory.peak` tracking, Unicode sparklines, live terminal HUD, interactive hotkeys | [`collect_telemetry_snapshot`](src/telemetry.rs), [`render_sparkline`](src/telemetry.rs), [`run_telemetry_hud`](src/telemetry.rs), [`TelemetrySnapshot`](src/telemetry.rs) |
| [`src/prune.rs`](src/prune.rs) | Storage and process pruning, orphaned Proton prefix & shadercache detection, /dev/shm IPC cleaner, dead Steam process/swap purger | [`scan_orphan_storage`](src/prune.rs), [`scan_orphan_processes`](src/prune.rs), [`execute_prune`](src/prune.rs), [`OrphanCandidate`](src/prune.rs), [`collect_active_appids`](src/prune.rs) |
| [`src/desktop.rs`](src/desktop.rs) | XDG .desktop application entry generator, cover art resolution, obsolete entry purger | [`generate_desktop_entries`](src/desktop.rs), [`resolve_game_icon`](src/desktop.rs), [`format_desktop_entry`](src/desktop.rs) |
| [`src/library.rs`](src/library.rs) | Parsing Valve ACF manifests, localconfig.vdf launch options (read, and write via `write_steam_localconfig_launch_options`, which refuses while the game or Steam runs), utility blacklisting, single Valve runtime/Proton app ID table, atomic updates | [`Game`](src/library.rs), [`VALVE_TOOLS`](src/library.rs), [`get_installed_games`](src/library.rs), [`parse_appmanifest`](src/library.rs), [`parse_localconfig_launch_options`](src/library.rs) |
| [`src/mount.rs`](src/mount.rs) | Linux kernel octal decoding, Polkit `udisksctl` mounting, external library persistence | [`MountInfo`](src/mount.rs), [`unescape_octal`](src/mount.rs), [`mount_device_with_udisksctl`](src/mount.rs), [`check_library_mount`](src/mount.rs) |
| [`src/non_steam.rs`](src/non_steam.rs) | Binary `shortcuts.vdf` in-place byte splicing, CRC32 AppIDs, active user resolution, Proton WoW64 runner | [`NonSteamGame`](src/non_steam.rs), [`calculate_steam_shortcut_appid`](src/non_steam.rs), [`write_atomic`](src/non_steam.rs), [`add_non_steam_game`](src/non_steam.rs) |
| [`src/daemon.rs`](src/daemon.rs) | Autopilot lifecycle supervisor, event-driven cgroup inotify monitoring, safe X11 close requests, CLI session guard mutual exclusion, minimal Freedesktop notifications | [`run_supervisor`](src/daemon.rs), [`close_steam_gui_to_tray`](src/daemon.rs), [`CliSessionGuard`](src/daemon.rs), [`is_cli_supervised`](src/daemon.rs), [`is_launch_or_game_window`](src/daemon.rs), [`ensure_vapour_icon`](src/daemon.rs), [`send_notification`](src/daemon.rs) |
| [`src/config.rs`](src/config.rs) | 4-tier configuration precedence resolution, TOML deserialization (errors, never silent defaults), Steam-style `launch_options` parsing | [`VapourConfig`](src/config.rs), [`parse_launch_options`](src/config.rs), [`GameConfig`](src/config.rs), [`ResolvedLaunchConfig`](src/config.rs), [`resolve_launch_config`](src/config.rs) |
| [`assets/`](assets/) | Visual branding assets: circular vector logo (`vapour-logo.svg`) and high-res raster export (`vapour-logo.png`) | Centered Flat Vapour Mark, Base Torus, terminal core |

---

## 3. Deep-Dive Module Breakdown

### A. Dual-Mode Dispatch & Terminal Detection (`main.rs`)
- **What it does**: Handles command routing via hybrid Clap parser supporting both modern subcommands (`add`, `rm`, `compat`, `list`, `doctor`) and legacy flags (`--add-game`, `--del-game`, `--compat`).
- **How it works**:
  - Checks `std::io::stdin().is_terminal() && std::io::stdout().is_terminal()`.
  - In non-interactive mode: ensures all required parameters are provided; exits with code `2` on validation errors. An ambiguous game query (several matches, none exact by name) fails and lists the matches instead of picking one.
  - `cli::run()` returns `VapourResult<()>`; handlers return errors instead of calling `exit()`, and `main.rs` prints the error and exits with `err.exit_code()`.
  - Formats output according to `--json`, `--dmenu`, `--plain`, or interactive colored tables.
- **Exit Codes**:
  - `0`: Success / System healthy.
  - `1`: General runtime / process failure.
  - `2`: CLI argument error or non-interactive missing argument.
  - `3`: Storage offline (unmounted drive without `--yes`).
  - `4`: Steam daemon communication error.

### B. Environment Diagnostic Doctor (`doctor.rs`)
- **What it does**: Inspects machine capabilities for minimal footprint execution: kernel floors by capability (`memory.reclaim` 5.19, `swappiness=` 6.11), unified cgroups v2 write permissions on user slice, zRAM/swap existence and swappiness, `udisksctl` availability, Steam IPC pipe state, container isolation, one line per probed kernel capability (reclaim, pidfd, PSI, cgroup.events, swappiness), and active Steam userdata AccountID. `vapour doctor --deep` also scans the installed Steam client for flags Vapour passes that no longer exist (`STEAM_DAEMON_FLAGS` in `launcher.rs`).
- **How it works**: Evaluates checks into `DoctorReport` and renders either colorized human tables or structured JSON (`--json`).

### C. Process Lifecycle & Headless Daemon (`launcher.rs`)
- **What it does**: Dispatches game launches through the native Steam daemon or starts Steam in headless mode if it is not running.
- **How it works**:
  - Checks if Steam is active using POSIX non-blocking `libc::open(pipe, O_WRONLY | O_NONBLOCK)`.
  - Injects birth scoping variables: `STEAM_LAUNCH_WRAPPER_SCOPE=1` and `PRESSURE_VESSEL_SYSTEMD_SCOPE=1`.
  - Spawns Steam under `systemd-run --user --scope -p Delegate=yes --unit=vapour-steam-<pid>`, so the `memory.reclaim` writes target a delegated cgroup.

### D. Shared Lifecycle Engine (`session.rs`) & CLI Watcher (`watcher.rs`)
- **What it does**: `session.rs` holds the mechanism both drivers share: finding the game (its `app-steam-app<id>-*.scope` first, `/proc` environ as fallback), detecting exit, reclaiming Steam memory through cgroups v2, PSI-triggered reclaim, the Steam Cloud exit-sync wait, pidfd signalling, and the exit policy. `watcher.rs` is the CLI driver: it follows update/launch progress, then calls `GameSession::run`.
- **How it works**:
  - `find_steam_cgroup_dirs()`: Resolves all active Steam cgroups, including Vapour's `vapour-steam-*.scope` unit and pressure-vessel scopes without an app id (`app-steam-unknown-*.scope`), while strictly excluding game scopes (leaf name `app-steam-app*`) and any cgroup holding a wrapper between `steam` and the game (e.g. gamescope).
  - Reclaim writes `"<reclaim_bytes_mb>M swappiness=<swappiness>\n"` (`[daemon]` in settings.toml, defaults 1500 and 200); after the first `EINVAL` (kernel < 6.11) it writes the plain `<reclaim_bytes_mb>M\n`. `find_steam_cgroup_dirs()` skips wrapper cgroups; `steam_cgroup_dirs_unfiltered()` is for telemetry only. Schedules differ by driver: CLI at T+12 s and T+40 s with at most 4 PSI passes (15 s cooldown); daemon at T+0 and T+25 s with no PSI cap (10 s cooldown). PSI passes use a kernel trigger (`some 300000 2000000`) and fall back to polling `some avg10 >= 5.0`.
  - Exit detection: inotify `IN_MODIFY` on the game scope's `cgroup.events` (`populated 0`) first; without a scope, pidfd/`/proc` ticks with a successor rescan (3 empty 2 s ticks); for the CLI, Steam's "app stopped running" log line as a last hint.
  - Cloud Sync: `cloud_step` is a per-AppID state machine (`Waiting -> ExitSyncStarted -> Done | Conflict`). Only `Upload complete, result OK` / `Upload complete in build list` count as completion; `AutoCloud complete` and `already synced` are logged before the upload and must not. Conflict markers are a guess. If an exit sync started but never completed within 60 s, a drift warning is printed. The per-AppID lines seen are then saved to `~/.cache/vapour/cloud_unknown.log` (last 64 KiB), as are the lines of a matched conflict marker.
  - Exit policy (`decide_on_exit`): only a Steam with a populated `vapour-steam-*.scope` (`Owned`) is shut down, and `--keep-steam` wins. A user-started Steam (`Adopted`) is never shut down; its GUI is restored if the daemon had closed it.

### E. Steam Manifests & Storage (`library.rs`, `mount.rs`)
- **What it does**: Locates Steam libraries, parses `appmanifest_*.acf`, decodes kernel octal sequences (`\040`), and auto-mounts unmounted drives via `udisksctl`.
- **How it works**:
  - Uses an mtime game cache in `~/.cache/vapour/games.txt`.
  - Patches manifests to `AutoUpdateBehavior 1` with atomic tempfile renaming.

### F. Modular Game Runners & Wrapper Engine (`runner.rs`)
- **What it does**: Provides polymorphic game execution through the `GameRunner` trait, isolating target definition (`LaunchTarget`) from command composition and runner dispatch (`SteamDaemonRunner`, `SteamShortcutRunner`, `ProtonDirectRunner`, `NativeDirectRunner`).
- **How it works**:
  - `compose_command_tokens`: Prefixes the wrapper tokens parsed from launch options and performs `%command%` interpolation. There are no gamemode/mangohud/gamescope toggles.
  - `get_runner_for_target`: Resolves the appropriate runner dynamically based on target type and whether `--direct` execution was requested.

### G. 4-Tier Precedence & Config Engine (`config.rs`, `library.rs`)
- **What it does**: Resolves launch configurations via a 4-tier hierarchy:
  1. CLI Flags (`--overlay`, `--no-overlay`, `--keep-steam`, extra args)
  2. Per-game settings in `settings.toml` (`[games.<id>]`)
  3. Global defaults in `settings.toml` (`[defaults]`)
  4. Native Steam client `userdata/<account_id>/config/localconfig.vdf` `LaunchOptions`
- **How it works**: `parse_localconfig_launch_options` scans the active user's localconfig VDF, returning options without overwriting explicit Vapour user configurations. `write_steam_localconfig_launch_options` is the only writer of `localconfig.vdf`: it refuses while the game or Steam runs (Steam rewrites that file on exit), saves `localconfig.vdf.vapour.bak` once, and writes with `write_atomic`. There are no gamemode/mangohud/gamescope settings; wrappers come from launch options.
  - Per-game `launch_options` go through `config::parse_launch_options`. With `%command%`, leading `KEY=VAL` tokens are merged into `env` (without overriding explicit `env`), the other tokens before `%command%` are prepended to `wrapper`, and tokens after it become `extra_args`. Never pass the raw string as game arguments.
  - `VapourConfig::parse` returns an error on invalid TOML, and `load_checked` returns an error on any read failure except `NotFound`. Never fall back to defaults and save them over an existing file.

### H. Binary VDF Mutation & Proton Integration (`non_steam.rs`)
- **What it does**: Parses and mutates Steam binary `shortcuts.vdf` in-place, computes CRC32 AppIDs, and configures `CompatToolMapping` in `config/config.vdf`.
- **How it works**: Sorts byte replacements descending before `Vec::splice` to preserve offset validity. Undergoes property-based fuzz testing via `proptest` on arbitrary byte buffers to ensure zero panic behavior.

### I. Typed Error Architecture & Exit Codes (`error.rs`)
- **What it does**: Declares `VapourError` (via `thiserror`) and `VapourResult<T>`, consolidating IPC, VDF corruption, missing media, config, and daemon errors into typed domain variants.
- **How it works**: Implements `exit_code(&self) -> i32` mapping every variant to standardized POSIX codes (Usage: 2, Storage: 3, Daemon/IPC: 4, Runtime: 1).

### J. Autopilot Lifecycle Supervisor & Mutual Exclusion (`daemon.rs`)
- **What it does**: Provides event-driven background supervisor automation using kernel inotify on cgroup scopes for external GUI launches, safely sends close requests to Steam windows without cancelling launch dialogs, and yields supervision when CLI launches are active.
- **How it works**:
  - `CliSessionGuard`: Acquires an RAII mutual-exclusion PID lock in `/dev/shm/u<uid>-vapour-cli-<appid>.pid` during CLI launches. The background daemon checks `is_cli_supervised(appid)` and completely yields supervision, eliminating dual-watcher race conditions or redundant memory reclaim passes.
  - The daemon is a thin driver over `session.rs`. It builds a `GameSession` with the daemon policy and a `keep_going` closure that returns false when `is_cli_supervised(appid)` becomes true, so it yields on every wake-up, not only at detection. It never closes the GUI of a vapour-owned Steam (that Steam is headless).
  - Autoclean (`prune::autoclean_session_leftovers`) only kills the Steam helpers listed in `ORPHAN_PROCESS_COMMS`. Never add `wine*` processes; other launchers own them too.
  - Safe X11 close requests (`close_steam_gui_to_tray` & `is_launch_or_game_window`): Checks `_NET_WM_WINDOW_TYPE` and ignores `_NET_WM_WINDOW_TYPE_DIALOG`, notification, and splash windows. Checks `WM_NAME` and `_NET_WM_NAME` to protect in-flight launch/update modals ("updating", "preparing to launch", "verifying", matching game title or AppID). `WM_DELETE_WINDOW` is sent only to windows that list it in `WM_PROTOCOLS`; Steam itself decides to hide to the tray (Vapour never unmaps). An error handler that ignores X errors is installed for the scan so a vanishing window cannot make Xlib call `exit()`.

---

## 4. Critical Invariants (Must NEVER Be Broken)

When writing code in this repository, you must **NEVER** violate these 14 system invariants:

1. **Never delete official Steam games from CLI**:
   Official games are managed by Steam via ACF manifests. Vapour only supports adding/removing non-Steam shortcuts (`vapour add`, `vapour rm`).
2. **Always sort binary VDF replacements in descending offset order**:
   In [`non_steam::update_shortcut_fields_binary_vdf`](src/non_steam.rs), replacements must be sorted by start offset descending (`sort_by_key(|a| Reverse(a.0))`). Splicing from the bottom preserves earlier byte offsets.
3. **Always set bit 31 on shortcut AppIDs, and prefer the stored one**:
   `calculate_steam_shortcut_appid` must return `(crc32 as u32) | 0x80000000`; Steam distinguishes official store AppIDs ($< 2^{31}$) from shortcuts via bit 31. That CRC32 formula is only Steam's legacy ID and is used only for shortcuts Vapour creates; for existing shortcuts always use the `appid` stored in `shortcuts.vdf`.
4. **Never replace `udisksctl` with `sudo mount`**:
   Vapour must remain completely usable by unprivileged non-root users.
5. **Never use `pipe.exists()` for Steam IPC readiness**:
   Always use non-blocking `libc::open(pipe, libc::O_WRONLY | libc::O_NONBLOCK)` to avoid deadlocks.
6. **Always record `cloud_offset = m.len()` at game exit**:
   In `wait_cloud_and_shutdown`, never inspect the log file from byte 0; only inspect bytes appended after the game exited.
7. **Never send `SIGKILL` or prematurely `SIGTERM` before the exit cloud sync is done**:
   The signal is the per-AppID `Upload complete` line in `cloud_log.txt` (see `cloud_step`). As a backstop, if `cloud_log.txt` was modified within the last 2 seconds, do not send `SIGTERM` yet.
8. **Treat `EAGAIN` / `WouldBlock` on `memory.reclaim` as success**:
   Writing to `/sys/fs/cgroup/.../memory.reclaim` may return `EAGAIN` if the kernel is currently thrashing or cannot evict additional anonymous pages. This is non-fatal.
9. **Never hang on missing arguments in non-interactive mode**:
   If a command requires input and `!is_interactive()`, fail immediately with exit code `2` and descriptive stderr usage text.
10. **Dispatch all game launches through the `GameRunner` abstraction**:
    All game process launches and dry-runs must go through [`src/runner.rs`](src/runner.rs) concrete runners (`SteamDaemonRunner`, `SteamShortcutRunner`, `ProtonDirectRunner`, `NativeDirectRunner`) to maintain unified wrapper composition and logging.
11. **Preserve the 4-tier configuration precedence order**:
    Launch options and wrapper flags must strictly resolve in order: CLI > `settings.toml` per-game > `settings.toml` defaults > `localconfig.vdf`. Never bypass or invert this resolution hierarchy.
12. **Always use the atomic, fsync'd write protocol for persistent files**:
    All configuration, manifest, cache, unit, `.desktop` and binary VDF files must be written via `write_atomic` (`<stem>.tmp.<pid>` -> `sync_all` -> rename, same directory) to prevent corruption from abrupt termination or power loss. Ephemeral `/dev/shm` lock files (the CLI session guard) are exempt.
13. **Return strongly-typed `VapourError` instead of loose strings**:
    Domain operations across all modules must return `VapourResult<T>` and exit through `err.exit_code()`, preserving standardized POSIX status code semantics.
14. **Enforce CLI <-> Daemon mutual exclusion via `CliSessionGuard`**:
    When a game is launched or supervised via the CLI (`vapour <game>`), background daemon supervisors (`vapour daemon`) must yield supervision entirely (`is_cli_supervised`). Daemons must never unmap modal dialogs (`_NET_WM_WINDOW_TYPE_DIALOG`) or trigger premature window deletion on in-flight game launches.

---

## 5. Verification Protocol

Before completing any task, execute:
```bash
cargo test
cargo check
cargo clippy -- -D warnings
cargo fmt --check
```
All tests, clippy checks, and formatting checks must pass with exit code `0`.
