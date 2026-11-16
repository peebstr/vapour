<p align="center">
  <img src="assets/vapour-logo.svg" alt="Vapour Logo" width="160" height="160">
</p>

# Vapour

> **Lightweight native Steam launcher, non-Steam runner, and lifecycle controller for Linux.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Linux%20(systemd%20%2B%20cgroup%20v2)-lightgrey.svg)](https://www.kernel.org/)

**Vapour** (`vapour`) is an ultra-fast, headless orchestration layer and terminal controller for Steam on Linux. It moves most of the resident memory of Steam's Chromium Embedded Framework (CEF) desktop client (`steamwebhelper`) into zRAM/swap (1213 MB → 44 MB RSS in one measured run), manages Proton prefixes and non-Steam shortcuts with native binary VDF mutation, automates non-privileged storage mounting via Polkit (`udisksctl`), and actively offloads Steam daemon heap to zRAM/swap via cgroups v2 while your games are running.

Vapour features a **Dual-Mode CLI Architecture**: a rich interactive terminal UI with dynamic initialism scoring (`cs2`, `tw3`, `er`) when run interactively, and a deterministic, machine-readable UNIX automation interface (`--json`, `--dmenu`, `--plain`) with standardized exit codes for scripts, desktop keybindings, and tiling window managers (Hyprland, Sway).

---

## Table of Contents

- [The Problem with Steam on Linux](#the-problem-with-steam-on-linux)
- [Core Architecture](#core-architecture)
  - [Headless Daemon Execution](#1-headless-daemon-execution)
  - [Kernel Memory Reclamation via cgroup v2](#2-kernel-memory-reclamation-via-cgroup-v2)
  - [Process Birth Scoping & Container Isolation](#3-process-birth-scoping--container-isolation)
  - [Cloud Sync Verification & Safe Lifecycle Shutdown](#4-cloud-sync-verification--safe-lifecycle-shutdown)
  - [Non-Steam Native Binary VDF Engine](#5-non-steam-native-binary-vdf-engine)
  - [Non-Blocking Storage & Polkit Auto-Mounting](#6-non-blocking-storage--polkit-auto-mounting)
  - [Modular Runner Trait & Wrapper Pipeline](#7-modular-runner-trait--wrapper-pipeline)
- [Installation & Requirements](#installation--requirements)
- [Dual-Mode CLI & Command Reference](#dual-mode-cli--command-reference)
  - [1. Interactive TUI Mode](#1-interactive-tui-mode)
  - [2. Direct Launches & Trailing Game Arguments](#2-direct-launches--trailing-game-arguments)
  - [3. Scriptable Management (`add`, `remove`, `compat`)](#3-scriptable-management-add-remove-compat)
  - [4. Machine-Readable Queries (`--json`, `--dmenu`)](#4-machine-readable-queries---json---dmenu)
  - [5. System Health Diagnostic Doctor (`vapour doctor`)](#5-system-health-diagnostic-doctor-vapour-doctor)
  - [6. Standardized Exit Codes](#6-standardized-exit-codes)
  - [Command & Flag Taxonomy](#command--flag-taxonomy)
- [Configuration Reference (`settings.toml`)](#configuration-reference-settingstoml)
  - [4-Tier Configuration Precedence](#4-tier-configuration-precedence)
  - [Global Settings](#global-settings)
  - [Per-Game Overrides](#per-game-overrides)
  - [Wrapper Composition & Steam %command%](#wrapper-composition--steam-command)
- [Memory Benchmarks & Telemetry](#memory-benchmarks--telemetry)
- [VAC & Anti-Cheat Compliance](#vac--anti-cheat-compliance)
- [Testing & CI/CD Quality Gates](#testing--cicd-quality-gates)
- [Codebase Structure](#codebase-structure)
- [License](#license)

---

## The Problem with Steam on Linux

Under standard desktop conditions, running Steam introduces substantial memory and process overhead:
1. **Web Subsystem Bloat**: Steam boots 12–14 Chromium Embedded Framework (CEF) helper processes, consuming 1.2 GB to 2.2+ GB of resident memory (RSS) just to render store, chat, and library webviews.
2. **Background I/O Storms**: Default Steam configurations trigger automatic workshop synchronization, shader pre-caching, and patch downloads while games are launching.
3. **Cgroup Contamination**: When launched from standard desktop application menus, Steam and all spawned game sub-processes share the same desktop cgroup hierarchy (`app.slice` or session scope), making it impossible for the Linux kernel to distinguish active game textures from idle Steam UI heap.
4. **Binary VDF Corruption**: Steam stores custom games in a proprietary binary KeyValues format (`shortcuts.vdf`). Generic text VDF converters strip binary tags or corrupt byte offsets, causing Steam to silently wipe custom shortcuts on launch.

**Vapour solves every single one of these bottlenecks.**

---

## Core Architecture

```
                                  +---------------------------+
                                  |     vapour CLI / TUI      |
                                  |  (main.rs / launcher.rs)  |
                                  +-------------+-------------+
                                                |
                   +----------------------------+----------------------------+
                   |                                                         |
                   v                                                         v
   +-------------------------------+                         +-------------------------------+
   |        Official Steam         |                         |        Non-Steam Game         |
   | (appmanifest / IPC FIFO pipe) |                         | (shortcuts.vdf / Proton pfx)  |
   +---------------+---------------+                         +---------------+---------------+
                   |                                                         |
                   v                                                         v
   +-------------------------------+                         +-------------------------------+
   |      Steam Client Daemon      |                         |       Native Linux Exec       |
   |   -silent -noverifyfiles     |                         |     OR Proton Runner Wine     |
   | cgroup: app-steam.slice       |                         | cgroup: isolated game scope   |
   +---------------+---------------+                         +-------------------------------+
                   |
                   | Launch Event
                   v
   +-----------------------------------------------------------------------------------------+
    |                       session.rs (shared lifecycle engine)                              |
   |                                                                                         |
    |  1. Exit detection: cgroup.events of the game scope, pidfd//proc ticks as fallback      |
    |  2. Scope isolation: STEAM_LAUNCH_WRAPPER_SCOPE=1 puts each game in its own scope       |
    |  3. Reclaim: CLI T+12s/T+40s, daemon T+0/T+25s, then PSI-triggered passes               |
    |     - memory.reclaim "1500M swappiness=200" (plain 1500M before Linux 6.11)             |
    |     - the game scope is never written to                                                |
    |  4. Cloud wait: finishes on "Upload complete" in cloud_log.txt, then exit policy        |
   +-----------------------------------------------------------------------------------------+
```

### 1. Headless Daemon Execution
When [`src/launcher.rs`](src/launcher.rs) initiates Steam, it injects surgical CLI parameters that suppress all web UI subsystems, friend network renders, crash reporters, and background overlay loaders:
```bash
steam -silent -nocrashdialog -noverifyfiles -vrdisable -nofriendsui \
      -cef-force-occlusion -cef-disable-site-isolation -noshaders \
      -cef-disable-hang-timeouts -cef-disable-breakpad \
      -disable-overlay-gpu-pri
```
To verify that Steam's daemon is listening without blocking, Vapour uses POSIX non-blocking FIFO probing:
- Calling `libc::open(pipe_path, libc::O_WRONLY | libc::O_NONBLOCK)` completes in `< 1 µs`.
- If Steam is still initializing and no process has opened the FIFO for reading, the kernel immediately returns `ENXIO` (No such device or address).
- Vapour couples this with a zero-allocation POSIX shared memory probe (`/dev/shm/u<UID>-ValveIPCSharedObj-Steam`) to achieve deterministic launch synchronization without arbitrary sleep timers.

### 2. Kernel Memory Reclamation via cgroup v2
Rather than attaching debuggers or calling virtual memory syscalls that trigger anti-cheat protections, [`src/watcher.rs`](src/watcher.rs) communicates directly with the Linux kernel cgroups v2 interface via `/sys/fs/cgroup/.../memory.reclaim`:
- **Multi-Cgroup Discovery**: Identifies both Vapour's primary daemon scope (`vapour-steam-*.scope`) and Steam Runtime's containerized CEF webhelper scopes (`app-steam-unknown-*.scope`), while strictly isolating and protecting active game containers (`app-steam-app<appid>-*.scope`).
- **Scheduled + Adaptive Eviction** (the CLI and the daemon share one engine, [`src/session.rs`](src/session.rs), with different schedules; values are empirical and tunable, see ARCHITECTURE.md §10):
  - **Scheduled passes**: CLI at T+12 s and T+40 s, daemon at T+0 and T+25 s. Vapour writes `"1500M swappiness=200\n"` to all Steam cgroups (`swappiness=` needs Linux 6.11; older kernels get plain `1500M`). Reclaim scans both anonymous and file pages, so it is not limited to inactive heap.
  - **PSI-triggered passes**: after the schedule, Vapour registers a kernel PSI trigger on `/proc/pressure/memory` (`some 300000 2000000`) and reclaims when it fires; if triggers are refused it polls `some avg10 >= 5.0`. The CLI allows at most 4 such passes (15 s cooldown); the daemon has no cap (10 s cooldown).
- **Result**: one recorded run went from 1213 MB RSS to 44 MB with 946 MB in zRAM (`benchmarks/BENCHMARK_RESULTS.md`, 2026-11-02); results vary by machine.

### 3. Process Birth Scoping & Container Isolation
When games are launched, third-party launchers often suffer from "cgroup contamination" where the game process remains inside Steam's cgroup, preventing memory eviction. Vapour injects container environment variables at birth:
```bash
STEAM_LAUNCH_WRAPPER_SCOPE=1
PRESSURE_VESSEL_SYSTEMD_SCOPE=1
```
`STEAM_LAUNCH_WRAPPER_SCOPE` makes Steam's `steam-launch-wrapper` place each game in its own `app-steam-app<appid>-*.scope`; pressure-vessel creates its own scopes through D-Bus `StartTransientUnit` (whether `PRESSURE_VESSEL_SYSTEMD_SCOPE` is on by default is unknown, so Vapour sets it). In addition, Vapour natively validates the 64-bit Steam Runtime client (`steamrt64`) and enables pure WoW64 execution for Proton 9+ and 10+ runners.

### 4. Cloud Sync Verification & Conflict Guard
When a game terminates, abruptly killing Steam risks corrupting Steam Cloud save files. [`src/watcher.rs`](src/watcher.rs) implements a 3-stage validation pipeline:
1. **Delta Offset Tracking**: Captures the exact EOF byte length (`cloud_offset`) of `~/.local/share/Steam/logs/cloud_log.txt` at the exact millisecond of game exit, completely ignoring historical upload events.
2. **Deterministic Completion & Conflict Match**: Continuously inspects newly appended log lines for completion (`[AppID <id>] Upload complete, result OK` or `Upload complete in build list`; `AutoCloud complete` and `already synced` appear *before* the upload and do not count) and detects save conflicts (marker text is a guess, not yet seen in a real log) (`Conflict detected`, `mismatch`, `files differ`).
3. **Conflict Preservation Guard**: If a cloud conflict is detected, Vapour halts daemon shutdown and keeps Steam alive, preventing save loss and allowing the user to resolve divergence in the Steam UI.
4. **Graceful Teardown Guard**: Requests shutdown, then sends `SIGTERM` through a pidfd (`kill(2)` fallback). If active writes occurred within the last 2 seconds, shutdown waits for an adaptive flush window before releasing resources.
5. **Real-Time Download & Update Inspection**: Prior to launch, when Steam triggers game or shader updates, Vapour tracks `content_log.txt` alongside active staging chunks in `steamapps/downloading/<appid>/`. This guarantees live download byte tracking without stalling progress indicators at 0.0% before manifest flush.

### 5. Non-Steam Native Binary VDF Engine
[`src/non_steam.rs`](src/non_steam.rs) is a standalone binary KeyValues engine for managing custom games, emulators, and runners:
- **In-Place Byte Splicing**: Modifies Steam's `shortcuts.vdf` directly without translating to text or losing binary type tags (`0x00` subkey, `0x01` string, `0x02` int32, `0x07` uint64, `0x08` stop tag).
- **Descending Offset Sorting**: Calculates byte offsets for modified string fields and splices replacements in reverse descending order to preserve byte-offset validity across multi-field updates.
- **CRC32 Shortcut AppID**: Calculates deterministic Steam shortcut AppIDs:
  $$\text{AppID} = (\text{CRC32}(\text{Target} + \text{AppName}) \text{ as u32}) \mid \text{0x80000000}$$
- **Active User Targeting**: Parses `loginusers.vdf` to extract the `MostRecent 1` SteamID64, masks it to a 32-bit `AccountID`, and only writes to the active user's `userdata/<account_id>/config/shortcuts.vdf`.
- **Proton Prefix Management**: Automatically registers compatibility mappings in `config.vdf`, generates clean isolated wineprefixes in `steamapps/compatdata/<appid>/pfx`, and supports running games under Proton either through Steam or directly via Wine (`--direct`).

### 6. Non-Blocking Storage & Polkit Auto-Mounting
[`src/mount.rs`](src/mount.rs) and [`src/library.rs`](src/library.rs) handle storage and game library discovery:
- **Zero-Block Startup**: Game discovery utilizes an mtime-validated cache (`~/.cache/vapour/games.txt`). The CLI interface loads instantly without spinning up mechanical disks or waiting for unmounted drives.
- **Linux Octal Decoding**: Accurately matches paths in `/proc/mounts` containing encoded octal sequences (e.g., `\040` for spaces in `/run/media/user/Samsung\040T7`).
- **Prompt-on-Launch Polkit Mounting**: If a game resides on an unmounted external NVMe or SSD, Vapour prompts the user only upon game launch (or auto-mounts if `--yes` is specified) and invokes `udisksctl mount -b /dev/...`, providing seamless non-root authentication matching native desktop file managers.
- **On-Demand Update Throttling**: Automatically patches official game manifests to set `AutoUpdateBehavior 1` with atomic temporary file renaming, preventing background Steam download storms from saturating bandwidth during gameplay.

### 7. Modular Runner Trait & Wrapper Pipeline
[`src/runner.rs`](src/runner.rs) provides an extensible execution abstraction that decouples game target resolution from execution strategy:
- **`GameRunner` Trait**: Defines `name()`, `build_command(&self, target, config)`, and `launch(&self, target, config, dry_run)`.
- **Concrete Runner Implementations**:
  - `SteamDaemonRunner`: Dispatches official Steam games through the headless Steam daemon IPC (`-applaunch <id>`).
  - `SteamShortcutRunner`: Launches custom games registered in Steam's shortcut database (`-gameidlaunch <long_appid>`).
  - `ProtonDirectRunner`: Directly executes Windows executables via Proton / Wine standalone (`--direct`), bypassing the Steam client entirely.
  - `NativeDirectRunner`: Directly executes native Linux ELF game binaries.
- **Wrapper Chaining & `%command%` Substitution**: Supports first-class toggles (`gamemode`, `mangohud`, `gamescope`) and custom wrapper expressions with full Steam `%command%` interpolation.


---

## Installation & Requirements

### System Requirements
- **OS**: Linux (kernel 5.19+ for cgroups v2 `memory.reclaim`, 6.11+ for the `swappiness=` bias)
- **Init**: systemd (with unified cgroup v2 hierarchy)
- **Swap**: zRAM, zswap, or physical swap enabled
- **Dependencies**: `libc`, `udisks2` (optional, for unprivileged drive mounts)

### Build from Source
Ensure Rust and Cargo are installed (`rustup` recommended):
```bash
git clone https://github.com/peebstr/vapour.git
cd vapour
cargo build --release
```

Install the compiled binary to your local user binary directory:
```bash
install -Dm755 target/release/vapour ~/.local/bin/vapour
```

Verify the installation and system readiness:
```bash
vapour doctor
```

---

## Dual-Mode CLI & Command Reference

Vapour is built for both human terminal interaction and deterministic UNIX scripting.

### 1. Interactive TUI Mode
Execute `vapour` without arguments to launch the interactive game selector:
```bash
vapour
```
- **Initialism & Acronym Search**: Type `cs2` to match *Counter-Strike 2*, `tw3` for *The Witcher 3*, or `er` for *Elden Ring*.
- **Ambiguous queries**: An exact name always wins (`portal` picks *Portal* over *Portal 2*). Otherwise, outside a terminal, a query matching several games fails and lists the matches instead of picking one; use the AppID.
- **Fuzzy Search & AppID**: Type partial names or exact Steam AppIDs.
- **Unmounted Drive Indicator**: Games on unmounted partitions appear marked with `[UNMOUNTED: label]`. Selecting one automatically prompts for Polkit mounting.

### 2. Direct Launches & Trailing Game Arguments
Launch a game directly by name, acronym, or AppID:
```bash
# Launch Counter-Strike 2 directly
vapour cs2

# Launch by AppID
vapour 730

# Pass arbitrary native game parameters directly through trailing arguments
vapour cs2 -novid -nojoy +exec autoexec.cfg

# Preview commands and cgroup spawn without launching
vapour cs2 --dry-run
```

### 3. Scriptable Management (`add`, `remove`, `compat`)
Every management action supports both interactive wizards (when invoked without arguments on a TTY) and non-interactive scripted execution:

```bash
# Register a non-Steam game programmatically
vapour add "Cyberpunk 2077" "/mnt/games/Cyberpunk2077/bin/x64/Cyberpunk2077.exe" \
  --compat "Proton Experimental" \
  --launch-options "-novid" \
  --json

# Assign a Proton compatibility tool directly in Steam's config.vdf
vapour compat 730 "GE-Proton9-23"

# Remove a non-Steam game and its Proton prefix with auto-confirmation
vapour rm "Cyberpunk 2077" --yes --json
```

### 4. Machine-Readable Queries (`--json`, `--dmenu`)

#### Pipeable Wayland Launcher Integration (Hyprland / Sway / Rofi / Fuzzel)
`vapour list --dmenu` outputs single-line pipeable entries: `<Name> | <AppID> | <Kind> | <Status>`.
```bash
# In ~/.config/hypr/hyprland.conf:
bind = $mainMod, G, exec, vapour list --dmenu | rofi -dmenu -i -p "Steam" | cut -d'|' -f2 | xargs vapour
```

#### JSON Output for Automation & External Tools
```bash
# Query installed games in structured JSON format
vapour list --json

# Query system diagnostics in structured JSON format
vapour doctor --json

# Query running daemon status in structured JSON format
vapour status --json
```

### 5. System Health Diagnostic Doctor (`vapour doctor`)
Runs a comprehensive self-test verifying kernel compatibility, cgroups v2 write permissions, zRAM/swap configuration, Polkit elevation, Valve IPC sockets, and container scoping:
```bash
vapour doctor
```

### 6. Live Telemetry HUD & Sparklines (`vapour status --watch` / `vapour monitor`)
Launches a real-time terminal dashboard with 8-level Unicode sparklines (` ▂▃▄▅▆▇█`) monitoring Core Steam RSS, CEF WebHelper RSS, zRAM offloading, Linux PSI memory stall pressure, and game container cgroup isolation:

```bash
# Launch live HUD at 1-second refresh
vapour monitor

# Custom refresh interval
vapour status --watch --interval 0.5
```

```text
┌── VAPOUR LIVE TELEMETRY HUD ────────────────────────────────────────────────────────┐
│ Daemon: RUNNING (PID: 12345)             Interval: 1.0s  Time: 20:30:15 UTC         │
│ Core RSS: 18.2 MB    CEF RSS: 142.1 MB   Total RSS: 160.3 MB                        │
│ zRAM Swap: 842.0 MB (CR: 3.42x) Peak/Saved: 1.84 GB/-1.68 GB PSI avg10: 0.00%      │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ RSS Trend:   [ ▂▃▄▅▆▇██▇▆▅▄▃▂ ]     160.3 MB                                       │
│ Swap Trend:  [████████████████]     842.0 MB                                       │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ Cgroup Scopes:                                                                      │
│   • vapour-steam-12345     RSS:   4.2 MB | Swap: 120.0 MB | Peak: 380.0 MB          │
│   • app-steam-unknown      RSS: 142.0 MB | Swap: 722.0 MB | Peak: 1.46 GB           │
│   ★ GAME: AppID 730    Scope: app-steam-app730   RSS: 2.41 GB | Swap: 0 MB (ISOLATED)│
├─────────────────────────────────────────────────────────────────────────────────────┤
│ Hotkeys: [r] Reclaim Memory  [k] Shutdown  [+/-] Interval  [q] Quit                 │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

#### Interactive Hotkeys in HUD
- `r` / `R`: Triggers an immediate kernel cgroup memory reclamation pass (`memory.reclaim`).
- `k` / `K`: Sends a graceful shutdown signal to the Steam daemon via IPC.
- `+` / `-`: Dynamically adjusts the HUD refresh interval (from 0.2s to 10.0s).
- `q` / `Esc`: Cleanly restores terminal raw mode and exits back to shell.

### 7. Storage, Process & Shader Pruner (`vapour prune` / `vapour clean` / `--prune`)
Scans all Steam library folders, `/dev/shm` tmpfs, and `/proc` for orphaned Proton prefixes (`compatdata/<appid>`), Vulkan/DXVK pipeline caches (`shadercache/<appid>`), unreferenced Steam POSIX shared memory buffers (`/dev/shm/u<UID>-Shm_*`), and orphaned Steam runtime helpers (`steamwebhelper`, `srt-bwrap`, `pv-adverb`) holding memory and swap after crashes or uninstalls.

Safety rules:
- Wine processes (`wineserver`, `winedevice.exe`) are never killed, because Lutris, Heroic and umu run them too.
- A prefix or shader cache is never pruned while an `appmanifest_<appid>.acf` for it exists in any library.
- If the installed-games list can't be read, directory pruning is skipped entirely.
- If any library is unmounted, directory pruning is also skipped.

```bash
# Preview reclaimable storage, shared memory, and orphaned processes without modifying anything
vapour prune --dry-run

# Purge all orphaned prefixes, shader caches, dead IPC shared memory buffers, and lingering helper processes
vapour prune --yes

# Selective pruning
vapour prune --compat-only --yes
vapour prune --shader-only --yes
vapour prune --shm-only --yes
vapour prune --proc-only --yes
```

### 8. XDG Desktop Entry Generator (`vapour desktop-entries`)
Generates native `.desktop` application files in `~/.local/share/applications/` for all installed Steam and non-Steam games, setting `Exec=vapour launch <appid>` and auto-resolving game cover art:

```bash
# Generate desktop entries and refresh application database
vapour desktop-entries

# Clean up obsolete .desktop files for uninstalled games
vapour desktop-entries --clean

# Preview in JSON format
vapour desktop-entries --dry-run --json
```
Games immediately appear in application launchers (GNOME, KDE Application Launcher, Hyprland/Rofi, Fuzzel, KRunner) and launch natively through Vapour.

### 9. Autopilot Lifecycle Supervisor Daemon (`vapour daemon`)
A passive, event-driven supervisor that runs in the background at **0.0% CPU and < 5 MB RAM** using kernel `inotify` on cgroup scopes:
- **Set-and-Forget Automation:** You do not need to launch games through terminal commands or custom shortcuts. Launch games normally through the official Steam client, desktop menus, or Steam Deck Game Mode.
- **CLI Session Guard & Mutual Exclusion:** When games are launched directly through the terminal (`vapour <game>`), the CLI acquires an RAII lock in `/dev/shm/u<uid>-vapour-cli-<appid>.pid`. The background supervisor immediately yields supervision, eliminating dual-watcher race conditions, double memory-reclaim calls, or competing shutdown signals.
- **Safe Modal Window Auto-Tray:** When a game starts, the supervisor checks `$DISPLAY` and sends `WM_DELETE_WINDOW` close requests to the main windows of a Steam you started yourself (Steam hides them to the tray; Vapour does not unmap anything). A Steam that Vapour started has no GUI to close. Crucially, it inspects `_NET_WM_WINDOW_TYPE` and ignores modal dialogs (`_NET_WM_WINDOW_TYPE_DIALOG`), splashes, and in-flight launch/update prompts (*"Preparing to launch..."*, *"Updating..."*), preventing launch cancellations.
- **Eviction to zRAM:** Issues `memory.reclaim` to Steam's cgroups on detection and again at T+25 s (see Core Architecture §2 for measured numbers).
- **Desktop Notifications:** Emits native desktop notifications via `notify-send` when memory is reclaimed and when games exit.
- **Automatic GUI Restoration:** When the game terminates and Steam Cloud save sync is verified, the supervisor restores your Steam GUI window (`steam steam://open/games`, untested without a running Steam) if it was open before, and only a Steam that Vapour itself started is shut down. The daemon also yields mid-game if a CLI launch takes over the same AppID.

```bash
# Install and enable as a systemd user service (recommended)
vapour daemon --install

# Check service and supervisor status
vapour daemon --status

# Run in foreground for debugging / live logging
vapour daemon --foreground

# Disable and remove systemd service
vapour daemon --uninstall
```

### 10. Standardized Exit Codes
- `0`: Success / System healthy.
- `1`: General runtime or system failure (e.g. game process spawn failed).
- `2`: CLI argument error or non-interactive missing argument.
- `3`: Storage offline (game resides on an unmounted drive without `--yes`).
- `4`: Steam daemon communication failure.

---

### Command & Flag Taxonomy

Vapour supports both modern subcommands and legacy flags for 100% backwards compatibility:

| Subcommand | Flag Equivalent | Non-Interactive Usage | Interactive (TTY) Usage |
| :--- | :--- | :--- | :--- |
| `vapour launch <QUERY>` | `vapour <QUERY>` | Direct launch with trailing game args | Direct launch with trailing game args |
| `vapour add` | `--add-game` | `vapour add <NAME> <EXE> [--compat <TOOL>] [--icon <PATH>] [--tags <TAGS>]` | Interactive Title $\to$ Path $\to$ Proton wizard |
| `vapour remove` / `rm` | `--del-game <GAME>` | `vapour rm <GAME> --yes` | Interactive select list + confirm prompt |
| `vapour compat` | `--compat <GAME> [TOOL]`| `vapour compat <GAME> <TOOL>` | Interactive Proton version picker |
| `vapour list` | `-l, --list` | `vapour list [--json] [--dmenu] [--plain] [--sort <recent\|size\|name>]` | Formatted colorized library table with size/recency metadata |
| `vapour libraries` | `--libraries` | `vapour libraries [--add <P>] [--del <P>]` | Formatted library list with mount statuses |
| `vapour config` | `-c, --settings` | `vapour config [KEY=VAL] [--get <KEY>]` | Interactive TUI configuration editor |
| `vapour doctor` | `--doctor` | `vapour doctor [--json] [--plain] [--deep]` | Formatted system health check with one line per kernel capability; `--deep` also scans the installed Steam client for flags Vapour passes that no longer exist |
| `vapour completions <SHELL>` | `--completions <SHELL>`| `vapour completions <bash\|zsh\|fish\|elvish\|powershell>` | Emits native shell tab-completion script to stdout |
| `vapour shutdown` / `kill` | `-k, --shutdown` | `vapour shutdown [--json] [-q]` | Graceful Steam daemon shutdown |
| `vapour status` | `-s, --status` | `vapour status [--watch] [-i <SEC>] [--json]` | Displays running/stopped daemon state, RSS, zRAM, and cgroups |
| `vapour monitor` | `-m, --monitor` | `vapour monitor [-i <SEC>]` | Live telemetry terminal HUD with hotkeys ([r], [k], [+/-], [q]) |
| `vapour reclaim` | `--reclaim` | `vapour reclaim` | Triggers immediate on-demand cgroup memory reclamation |
| `vapour daemon` | — | `vapour daemon [--foreground] [--install] [--uninstall] [--status]` | Runs the background supervisor or manages its systemd service |
| `vapour prune` / `clean`| `--prune` | `vapour prune [--dry-run] [--yes] [--compat-only] [--shader-only] [--shm-only] [--proc-only]` | Purges orphaned Proton prefixes, shader caches, `/dev/shm` IPC buffers, and dead Steam processes holding swap/RAM |
| `vapour desktop-entries`| — | `vapour desktop-entries [--dry-run] [--clean]` | Generates XDG .desktop files for system menu launches |
| `vapour info` | `-i, --info` | `vapour info <GAME> [--json]` | Inspects runtime deps and Wine prefix |

---

## Configuration Reference (`settings.toml`)

Vapour stores configuration in `~/.config/vapour/settings.toml`. You can edit this file manually, manage keys via `vapour config <KEY>=<VAL>` or `vapour config --get <KEY>`, or open the interactive editor with `vapour config`.

### 4-Tier Configuration Precedence

Vapour resolves launch parameters using a strict 4-tier configuration precedence hierarchy:

```
[ Tier 1: CLI Flags & Arguments ]
       │ (e.g. --overlay / --no-overlay, --keep-steam, trailing game args)
       ▼
[ Tier 2: Per-Game Settings ]
       │ ([games.<appid_or_name>] in ~/.config/vapour/settings.toml)
       ▼
[ Tier 3: Global Defaults ]
       │ ([defaults] in ~/.config/vapour/settings.toml)
       ▼
[ Tier 4: Native Steam localconfig.vdf ]
         (userdata/<account_id>/config/localconfig.vdf LaunchOptions)
```

```toml
[defaults]
overlay = true
zero_ram = true
nojoy = false
gamemode = true
mangohud = false
gamescope = false
gamescope_args = "-W 1920 -H 1080 -f -r 144"
wrapper = ""

[games.730]
# Per-game override for Counter-Strike 2
overlay = true
gamemode = true
mangohud = true
gamescope = true
gamescope_args = "-W 1920 -H 1080 -w 1280 -h 960 -r 240 --immediate-flips -f -S stretch --force-grab-cursor"
extra_args = ["-novid", "-freq", "240"]
env = [["MESA_VK_WSI_PRESENT_MODE", "mailbox"]]

[games.2165744007]
# Per-game override for non-Steam game
compat_tool = "Proton Experimental"
exe_path = "/run/media/user/win/Games/Game.exe"
start_dir = "/run/media/user/win/Games"
wrapper = "gamemoderun gamescope -W 1920 -H 1080 -- %command%"
extra_args = ["-fullscreen"]
```

### Global Settings

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `zero_ram` | boolean | `true` | Enables headless Steam flags and two-tier cgroup v2 memory reclamation |
| `overlay` | boolean | `true` | Controls Vulkan overlay layers; when `false`, overlay is fully suppressed |
| `nojoy` | boolean | `false` | When `true`, injects `-nojoy` to suppress joystick/gamepad input polling |
| `gamemode` | boolean | `false` | Prepends `gamemoderun` wrapper to process launch |
| `mangohud` | boolean | `false` | Prepends `mangohud` telemetry HUD to process launch |
| `gamescope` | boolean | `false` | Wraps execution with Valve's `gamescope` micro-compositor |
| `gamescope_args` | string | `None` | Custom arguments passed to `gamescope` before `--` |
| `wrapper` | string | `None` | Custom wrapper command; supports Steam `%command%` interpolation |

### Daemon Settings (`[daemon]`)

Configure the behavior of the background supervisor daemon (`vapour daemon`):

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `auto_tray_gui` | boolean | `true` | Sends `WM_DELETE_WINDOW` to the main Steam windows of a user-started Steam on game start (Steam hides them to the tray) |
| `restore_gui_on_exit` | boolean | `true` | Automatically reopens Steam GUI (`steam steam://open/games`) once game exits and cloud sync completes |
| `reclaim_bytes_mb` | integer | `1500` | Target megabytes requested per `memory.reclaim` write (the kernel reclaims anonymous and file pages) |
| `swappiness` | integer | `200` | Aggressive swap preference parameter passed to kernel `memory.reclaim` |
| `psi_adaptive` | boolean | `true` | Monitors PSI (`/proc/pressure/memory`) to dynamically trigger re-reclamation if inactive memory spikes |
| `notifications` | boolean | `true` | Emits native desktop notifications via `notify-send` on memory reclamation and game state transitions |
| `shutdown_if_headless` | boolean | `true` | If Steam was started headlessly by the supervisor, shuts it down gracefully when gameplay ends |

### Per-Game Overrides

Every field in `[defaults]` can be overridden per game under `[games.<id>]` (using Steam AppID or non-Steam game name). In addition, per-game sections support:
- `compat_tool`: Proton compatibility tool name (e.g. `"GE-Proton9-23"`).
- `exe_path`: Custom executable path (used for non-Steam games).
- `start_dir`: Working directory for game execution.
- `extra_args`: Array of strings passed directly to the game binary.
- `env`: Array of `[key, value]` pairs injected into the game's environment.
- `launch_options`: Steam-style launch options string (set via `vapour set-launch-options`). Setting it via `vapour set-launch-options` replaces the game's `extra_args` (Vapour prints the replaced arguments); if both are present in the file, `extra_args` wins. With `%command%`, leading `KEY=VAL` tokens become environment variables, the remaining tokens before it are prepended to the wrapper, and tokens after it become game arguments. For example, `WINEDLLOVERRIDES="winhttp=n,b" gamemoderun %command% -novid` sets the variable, adds `gamemoderun`, and passes `-novid`. Without `%command%`, every token is a game argument. Note that if Steam is already running, a Steam-dispatched launch cannot apply env or wrapper settings (Vapour warns); set those in Steam's own launch options instead.

`settings.toml` must be valid TOML. If it can't be read or parsed, Vapour stops with a configuration error instead of resetting it to defaults. A missing file is created with defaults.

### Wrapper Composition & Steam %command%

When multiple wrappers are active, Vapour composes the command sequence:
1. If `wrapper` contains `%command%`, the target executable replaces `%command%` directly (e.g. `gamemoderun gamescope -- %command%` becomes `gamemoderun gamescope -- /path/to/game`).
2. Otherwise, first-class toggles are chained in order without duplication:
   `[custom wrapper] -> [gamemoderun] -> [mangohud] -> [gamescope <args> --] -> <executable> <args>`.

---

## Memory Benchmarks & Telemetry

One live run, recorded in [`benchmarks/BENCHMARK_RESULTS.md`](benchmarks/BENCHMARK_RESULTS.md) (CachyOS, kernel 7.3.1, 2026-11-02, `scripts/benchmark.sh`):

| Metric | Measured |
| :--- | :--- |
| Steam RSS before reclaim | 1213 MB |
| Steam RSS after reclaim | 44 MB |
| Moved to zRAM | 946 MB (4.29x compression) |
| `vapour status --json` runtime | 4 ms (5 runs; CLI overhead, not game launch time) |

Earlier figures for a native-GUI baseline, frame-time improvement, startup time and process counts were not backed by a recorded run and have been removed. Results vary by machine and Steam version.

---

## Anti-Cheat Considerations

Vapour never touches game processes, which is the property anti-cheat systems care about. It has not been certified or tested against any anti-cheat vendor, so no compliance is claimed:
- **Zero Process Injection**: Vapour never attaches `ptrace`, never executes `process_madvise(MADV_PAGEOUT)`, and never reads `/proc/<pid>/mem`.
- **Pure Kernel Boundary Interaction**: Memory reclamation operates exclusively via the standard Linux kernel cgroup v2 controller interface (`/sys/fs/cgroup/.../memory.reclaim`), which operates at the cgroup container boundary without touching game address spaces.
- **Dedicated Process Scoping**: With `STEAM_LAUNCH_WRAPPER_SCOPE=1`, Steam places each game in its own scope (`app-steam-app<appid>-*.scope`), which Vapour never writes to.
- **No Mid-Game Daemon Killing**: Steam's background daemon stays alive for the whole match; Vapour only evicts its idle memory.

---

## Testing & CI/CD Quality Gates

Vapour employs rigorous automated verification and property-based fuzzing:
- **Typed Error Architecture**: Every failure path returns strongly typed `VapourError` (via `thiserror`) that maps deterministically to standardized POSIX exit codes (`0`, `1`, `2`, `3`, `4`).
- **Atomic File Protocol**: All state updates (`shortcuts.vdf`, `config.vdf`, `appmanifest_*.acf`, `settings.toml`, and caches) write to PID-tagged temporary files, `fsync` them, then perform an atomic POSIX rename, so a killed process or power loss cannot leave a partial file. (The `/dev/shm` CLI lock is exempt.)
- **Property-Based Fuzzing (`proptest`)**: VDF parsers and binary updaters undergo property testing against thousands of randomized byte sequences and malformed buffers, guaranteeing zero panics.
- **In-Memory Mock Cgroup Harness**: Full integration tests in `tests/mock_cgroup_lifecycle.rs` simulate Linux cgroup hierarchies, POSIX non-blocking FIFOs, and kernel PSI spikes using `tempfile`, enabling 100% automated CI verification without requiring root or active Steam.
- **PIDFD Race-Free Supervision**: Linux 5.3+ PID file descriptors (`libc::SYS_pidfd_open`) narrow the PID-recycling window for liveness checks and signals (falling back to `/proc` if `pidfd_open` fails).
- **CI/CD Pipeline**: GitHub Actions workflow (`.github/workflows/ci.yml`) enforces `cargo check --all-targets`, `cargo fmt --check`, `cargo clippy -- -D warnings`, unit/fuzz tests, and release builds on every commit.

---

## Codebase Structure

The Vapour codebase is built with strict modularity and zero unnecessary external dependencies:

```
src/
├── lib.rs        # Core crate library target (enables integration tests and benchmarks)
├── main.rs       # Ultra-lean CLI entry point (dispatches to vapour::cli::run)
├── cli/          # Modular command line and terminal user interface subsystem
│   ├── mod.rs    # Clap command definitions, normalized parsing, global routing
│   ├── launch.rs # Game resolution, execution planner (--dry-run), runner dispatch
│   ├── list.rs   # Library & game listing, JSON serialization, dmenu streaming
│   ├── manage.rs # Game registration, shortcut removal, Proton compat configuration
│   ├── system.rs # System health doctor, storage prune, desktop entries, status
│   └── tui.rs    # Interactive fuzzy selector, dynamic initialism scoring (cs2, tw3)
├── error.rs      # Strongly typed domain errors (thiserror), POSIX exit code mappings
├── doctor.rs     # System diagnostic engine (cgroup v2, swap/zram, kernel, polkit, IPC)
├── launcher.rs   # Process lifecycle, headless flags, non-blocking POSIX FIFO probing
├── runner.rs     # Modular GameRunner trait, dispatch factory, wrapper composition
├── session.rs    # Shared lifecycle engine: scope/pidfd exit detection, reclaim, PSI triggers, cloud wait
├── watcher.rs    # CLI driver: update/launch progress, hands over to session.rs
├── telemetry.rs  # Real-time telemetry snapshot, zero-alloc stack buffers, sparkline HUD
├── prune.rs      # Automated storage pruner, orphaned Proton prefix & shadercache janitor
├── desktop.rs    # XDG .desktop entry generator, cover art resolution, stale cleanup
├── library.rs    # ACF/VDF parsing, localconfig LaunchOptions, on-demand update control
├── mount.rs      # Kernel octal unescaping, Polkit udisksctl mounting, storage state
├── non_steam.rs  # Binary shortcuts.vdf splicing, CRC32 AppIDs, Proton environment runner
├── daemon.rs     # Background autopilot supervisor, inotify cgroup engine, daemon driver, X11 close requests
└── config.rs     # 4-tier configuration precedence, TOML deserialization, wrapper toggles

tests/
└── mock_cgroup_lifecycle.rs # Mock cgroup isolation, PSI spikes, and non-blocking FIFO integration tests

scripts/
└── benchmark.sh  # Automated empirical benchmark harness (measuring RSS, latency, page faults)
```

For comprehensive technical deep-dives into kernel mechanics, cgroups delegation, container isolation, and benchmarks:
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — Technical whitepaper on Linux kernel cgroups v2, pressure-vessel, and memory dynamics
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — Code style, quality gates, and patch guidelines
- [`AGENTS.md`](AGENTS.md) — Architectural invariants and module responsibilities

---

## License

This project is licensed under the [MIT License](LICENSE).
