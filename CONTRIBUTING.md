# Contributing to Vapour

Thank you for your interest in contributing to **Vapour**! Vapour is engineered for high performance, reliability, and security on Linux systems. To keep the footprint minimal and launches reliable, all contributions must adhere to the principles outlined below.

---

## Table of Contents

- [Core Principles & Architectural Philosophy](#core-principles--architectural-philosophy)
- [Development Setup](#development-setup)
- [Testing & Verification Protocol](#testing--verification-protocol)
- [Coding Standards & Invariants](#coding-standards--invariants)
  - [1. Dual-Mode CLI Architecture & Machine Formats](#1-dual-mode-cli-architecture--machine-formats)
  - [2. Standardized POSIX Exit Codes](#2-standardized-posix-exit-codes)
  - [3. Non-Blocking I/O & Zero IPC Deadlocks](#3-non-blocking-io--zero-ipc-deadlocks)
  - [4. VAC & Anti-Cheat Compliance Rules](#4-vac--anti-cheat-compliance-rules)
  - [5. POSIX Atomic File Writes](#5-posix-atomic-file-writes)
  - [6. Non-Destructive Cloud Sync Verification](#6-non-destructive-cloud-sync-verification)
  - [7. Binary vs. Text VDF Mutation Rules](#7-binary-vs-text-vdf-mutation-rules)
  - [8. Polkit & Non-Privileged Storage Elevation](#8-polkit--non-privileged-storage-elevation)
  - [9. Diagnostic Health Checks (`vapour doctor`)](#9-diagnostic-health-checks-vapour-doctor)
  - [10. Modular Game Runner Trait & Wrapper Pipeline](#10-modular-game-runner-trait--wrapper-pipeline)
  - [11. Typed Error Architecture & Property-Based Testing](#11-typed-error-architecture--property-based-testing)
  - [12. CLI <-> Daemon Mutual Exclusion & Safe X11 Close Requests](#12-cli---daemon-mutual-exclusion--safe-x11-close-requests)
- [Adding New Features or CLI Flags](#adding-new-features-or-cli-flags)
- [Pull Request & Code Review Checklist](#pull-request--code-review-checklist)

---

## Core Principles & Architectural Philosophy

1. **Zero Unnecessary RAM Overhead**: Every process spawned or managed by Vapour must have the smallest possible memory footprint. We suppress GUI subsystems (CEF, Qt, GTK) wherever native CLI or headless daemon modes exist.
2. **Never Block on Hardware or External Devices**: The CLI should launch and render games instantaneously. Slow network shares, unmounted external hard drives, or sleeping SATA/NVMe buses must never block initial UI startup.
3. **Pure Kernel Boundaries (No Process Tampering)**: We manage memory and processes strictly through official Linux kernel and systemd interfaces (`cgroup v2`, `systemd-run`). We never inspect or attach to game virtual memory address spaces.
4. **Atomic Persistence**: Configuration files, Steam manifests, and binary VDF files must never be left in a corrupted or half-written state if a crash or `SIGINT` occurs.

---

## Development Setup

### Prerequisites
- **Rust Toolchain**: 1.75+ (Stable). Install via [rustup](https://rustup.rs/):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **System Dependencies**:
  - `libc` and `pkg-config`
  - `udisks2` (for mounting external drives via Polkit)
  - `systemd` with unified cgroups v2 hierarchy enabled
- **Linux Kernel**: 5.19+ for `memory.reclaim`; 6.11+ for its `swappiness=` bias

### Building the Project
Clone the repository and build using Cargo:
```bash
git clone https://github.com/peebstr/vapour.git
cd vapour
cargo build
```

To build an optimized release binary:
```bash
cargo build --release
```

---

## Testing & Verification Protocol

### Running the Test Suite
All unit tests must pass without errors or warnings before submitting a pull request:
```bash
cargo test
```

### Running Linters & Format Checks
Vapour maintains zero-warning strictness. Verify that formatting and lints pass:
```bash
cargo fmt --check
cargo clippy -- -D warnings
```

### Running Dry-Run Validations
Before committing changes to [`src/launcher.rs`](src/launcher.rs) or [`src/config.rs`](src/config.rs), verify command construction with `--dry-run`:
```bash
cargo run -- launch cs2 --dry-run
cargo run -- launch 730 --dry-run -novid
cargo run -- cs2 --dry-run
```

### Running Machine-Readable Output Tests
Verify that JSON, dmenu, and plain format outputs conform to expected machine formats:
```bash
cargo run -- list --json
cargo run -- list --dmenu
cargo run -- doctor --json
cargo run -- status --json
cargo run -- prune --dry-run --json
cargo run -- desktop-entries --dry-run --json
```

### Running the Empirical Benchmark Suite
To measure cold startup latency, resident set size (RSS), page faults, and zRAM compression efficiency:
```bash
./scripts/benchmark.sh --mock
# Or against a live game container:
./scripts/benchmark.sh --live 730
```
For architectural diagrams and kernel system details, see [`ARCHITECTURE.md`](ARCHITECTURE.md).

### Adding Unit Tests
- When adding or modifying string parsers, ACF parsers, CLI converters, or VDF handlers, always add comprehensive unit tests in the same module.
- Mock external filesystem access or test against in-memory slices whenever possible (see examples in [`src/non_steam.rs`](src/non_steam.rs), [`src/mount.rs`](src/mount.rs), and [`src/main.rs`](src/main.rs)).

---

## Coding Standards & Invariants

### 1. Dual-Mode CLI Architecture & Machine Formats
- **Terminal vs. Pipe Detection**: Detect TTY interactive mode with `std::io::stdout().is_terminal()`.
- **Never emit ANSI escape sequences or interactive prompts to non-interactive stdout**: If stdout is piped or redirected, or if `--json`, `--dmenu`, `--plain`, or `--quiet` is specified, disable rich color formatting and interactive prompts (`dialoguer`, `inquire`).
- **Format Flags**:
  - `--json`: Output valid, parseable JSON conforming to typed structs (`serde_json`). Any error messages must also output as JSON (`{"error": "..."}`) when `--json` is active.
  - `--dmenu`: Output single-line records (`Name | AppID | Kind | Status`) suitable for fuzzy pickers (Rofi, Wofi, dmenu, fuzzel).
  - `--plain`: Output clean tabular plaintext without ANSI color codes or Unicode borders.
- **Fail-Safe Non-Interactive Operations**: Destructive operations like `vapour rm <name>` must fail immediately with exit code `2` if run non-interactively without `--yes` / `-y`. Never block or freeze background scripts on hidden stdin reads.

### 2. Standardized POSIX Exit Codes
All subcommands and failure paths must adhere to the standardized exit code taxonomy:
- `0`: Success / clean operation.
- `1`: General runtime failure / internal error.
- `2`: CLI usage / argument validation / non-interactive confirmation failure.
- `3`: Storage offline / drive not found or unmounted.
- `4`: Steam daemon / IPC communication failure.

Use the constants defined in `src/error.rs` (`EXIT_SUCCESS`, `EXIT_RUNTIME_ERR`, `EXIT_USAGE_ERR`, `EXIT_STORAGE_ERR`, `EXIT_IPC_ERR`) or call `err.exit_code()` on `VapourError`.

### 3. Non-Blocking I/O & Zero IPC Deadlocks
- **Never use `pipe.exists()` for Steam IPC readiness**: On Linux, a FIFO special file (`~/.steam/steam.pipe`) persists on the filesystem even when Steam is completely offline. Opening a FIFO in blocking mode (`File::open`) will hang the thread indefinitely until a reader attaches.
- **Always use POSIX non-blocking `libc::open`**:
  ```rust
  let fd = unsafe { libc::open(pipe_cstr.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
  if fd < 0 {
      let err = std::io::Error::last_os_error();
      if err.raw_os_error() == Some(libc::ENXIO) {
          // Normal: Steam is starting or pipe has no reader yet
      }
  }
  ```
- See [`launcher::is_steam_pipe_open`](src/launcher.rs) for the reference implementation.

### 4. VAC & Anti-Cheat Compliance Rules
- **Never use `process_madvise(MADV_PAGEOUT)`**: This syscall requires `PTRACE_MODE_READ_FSCREDS` capability, fails with `EPERM` without `CAP_SYS_ADMIN`, and trips heuristics in anti-cheat software (VAC, EAC).
- **Never terminate the Steam daemon during an active game session**: Steamworks games periodically re-verify DRM tickets and matchmaking tokens over IPC. Killing Steam drops matchmaking connections and can trigger VAC authentication errors.
- **Use cgroups v2 `memory.reclaim` instead**: Writes to `/sys/fs/cgroup/user.slice/.../app-steam.slice/memory.reclaim` operate completely unprivileged at the cgroup boundary without inspecting game memory spaces.

### 5. POSIX Atomic File Writes
- Never write directly to configuration or database files (`settings.toml`, `config.vdf`, `shortcuts.vdf`, `appmanifest_*.acf`).
- Always use [`non_steam::write_atomic`](src/non_steam.rs) or equivalent logic:
  1. Generate a temporary file in the same directory using the current PID: `<path>.<pid>.tmp`.
  2. Write and flush all data to the temporary file.
  3. Call `std::fs::rename(tmp_path, target_path)` to ensure an atomic filesystem swap on POSIX.

### 6. Non-Destructive Cloud Sync Verification
- When terminating Steam, never issue `SIGKILL` (`kill -9`) or prematurely `SIGTERM` (`kill -15`) before Steam Cloud has completed uploading save data.
- Always calculate `cloud_offset = m.len()` at game exit and inspect `cloud_log.txt` for `Cloud Sync: complete`.
- Guard shutdown with an adaptive flush window (see [`watcher::wait_cloud_and_shutdown`](src/watcher.rs)).

### 7. Binary vs. Text VDF Mutation Rules
- **Binary VDF (`shortcuts.vdf`)**:
  - `shortcuts.vdf` uses binary KeyValues with explicit byte tags (`0x00`, `0x01`, `0x02`, `0x07`, `0x08`).
  - **Never parse binary VDF through a text KeyValues parser.** Doing so drops tags and corrupts Steam's internal shortcut database.
  - When modifying binary entries in-place, collect all field offset replacements, **sort them in descending order by start offset**, and apply them using `splice`. This guarantees earlier byte offsets remain valid.
  - Always set bit 31 on shortcut AppIDs: `appid = (crc32 as u32) | 0x80000000`.
- **Plaintext VDF (`config.vdf`, `appmanifest_*.acf`)**:
  - Plaintext VDF blocks like `"CompatToolMapping"` must be parsed with strict brace counting ([`non_steam::find_block_bounds`](src/non_steam.rs)) to prevent malformed truncations.

### 8. Polkit & Non-Privileged Storage Elevation
- **Never run `sudo mount` or ask the user for root passwords**:
  - Always check `/proc/mounts` and unescape kernel octal escape characters (e.g., `\040` for spaces).
  - Use `udisksctl mount -b <device>` to trigger Polkit authorization matching desktop environments.

### 9. Diagnostic Health Checks (`vapour doctor`)
- All system environment inspections belong in [`src/doctor.rs`](src/doctor.rs).
- Diagnostic checks must produce structured `CheckItem` records with:
  - `name`: Short, clear identifier (e.g., `"Kernel & cgroups v2"`).
  - `status`: `CheckStatus::Ok`, `CheckStatus::Warn`, or `CheckStatus::Err`.
  - `message`: Diagnostic outcome summary.
  - `remediation`: Optional actionable command or config fix for the user.
- Checks must never panic or fail if an optional subsystem is missing; they should downgrade to `Warn` with a descriptive remediation string.

### 10. Modular Game Runner Trait & Wrapper Pipeline
- All process dispatch routines belong in [`src/runner.rs`](src/runner.rs) under the `GameRunner` trait (`name`, `build_command`, `launch`).
- Never write ad-hoc `Command::new` spawners inside CLI handling loops. Dispatch targets through `get_runner_for_target`.
- Compose wrappers exclusively through `compose_command_tokens`, preserving Steam `%command%` expansion. Wrappers come only from launch options; there are no wrapper toggles.
- Respect the 4-tier configuration precedence: CLI > `settings.toml` per-game > `settings.toml` defaults > `localconfig.vdf`.

### 11. Typed Error Architecture & Property-Based Testing
- All subsystem operations must return `VapourResult<T>` and construct typed `VapourError` variants (defined in [`src/error.rs`](src/error.rs)).
- When formatting user-facing failures, map to standardized POSIX exit codes using `err.exit_code()`.
- Parsers handling arbitrary binary input (such as `parse_shortcuts_vdf`) must be covered by `proptest` property-based fuzz tests so random and malformed byte buffers are checked not to panic.

### 12. CLI <-> Daemon Mutual Exclusion & Safe X11 Close Requests
- **CLI Session Locks (`CliSessionGuard`)**: When launching games via the CLI (`vapour <game>`), acquire `CliSessionGuard::acquire(appid)`. This maintains an RAII PID lockfile in `/dev/shm/u<uid>-vapour-cli-<appid>.pid`. Background supervisor daemons must check `is_cli_supervised(appid)` and yield supervision completely to prevent dual-reclamation and competing lifecycle shutdown signals.
- **Safe Window Filtering (`is_launch_or_game_window`)**: When sending `WM_DELETE_WINDOW` client messages asking Steam to close (hide) its main GUI, never broadcast blindly. Inspect `_NET_WM_WINDOW_TYPE` and ignore modal dialogs (`_NET_WM_WINDOW_TYPE_DIALOG`), splash screens, and notifications. Inspect `WM_NAME` and `_NET_WM_NAME` to ensure in-flight launch or update dialogs (*"Preparing to launch..."*, *"Updating..."*) are never closed, which would cause Steam to cancel the game launch.

### 13. Desktop Notifications & Brand Icon Resolution
- **Minimal, Non-Intrusive Notifications**: Desktop notifications sent via `send_notification` must remain concise and professional. Never send mid-game popups during active gameplay (e.g. secondary compaction passes), and avoid verbose "vibecoded" text or multi-stage redundant popups on game exit.
- **Brand Icon Embedding (`ensure_vapour_icon`)**: Desktop notifications use the embedded 512×512 Vapour brand logo (`assets/vapour-logo.png`). The daemon ensures the icon is written to `~/.local/share/icons/hicolor/512x512/apps/vapour.png` and passes its absolute path to `notify-send -i <path>`, so notification daemons that accept an icon path show it without an installed icon theme.

---

## Adding New Features or CLI Flags

When adding a new command-line option:
1. Update the `Cli` struct in [`src/main.rs`](src/main.rs) using `clap` derive attributes. Provide descriptive docstrings and short flag aliases where appropriate.
2. If the option introduces new trailing parameters or affects launch behavior, update `resolve_launch_config` in [`src/config.rs`](src/config.rs) and the runner pipeline in [`src/runner.rs`](src/runner.rs).
3. Update [`src/config.rs`](src/config.rs) if the option corresponds to a persistent configuration field in `settings.toml`.
4. Add unit tests verifying both flag parsing and command formatting.
5. Update `README.md`, `CONTRIBUTING.md`, and `AGENTS.md` to reflect the new feature.

---

## Pull Request & Code Review Checklist

Before opening a pull request or submitting code changes, verify each item:

- [ ] **Tests Pass**: `cargo test` runs cleanly with 0 failures and 0 warnings (including `proptest` fuzzing).
- [ ] **Formatting & Lints**: Code adheres to standard Rust formatting (`cargo fmt --check` / `cargo clippy -- -D warnings`).
- [ ] **Dual-Mode Standards**: Piped output is clean; `--json`, `--dmenu`, `--plain` are supported where appropriate; no unprompted hangs on stdin.
- [ ] **Exit Code Taxonomy**: Non-interactive failures return `2`, runtime errors return `1`, storage issues return `3`, IPC issues return `4`.
- [ ] **Atomic Writes**: All new or modified file writers use atomic temporary file renaming.
- [ ] **Non-Blocking Architecture**: No new synchronous network calls or blocking disk I/O are introduced in the startup path.
- [ ] **Memory Invariants**: cgroup v2 memory reclamation and headless flags are preserved.
- [ ] **VAC Safety**: No ptrace, `/proc/<pid>/mem`, or debugger syscalls added.
- [ ] **Extensible Runners**: Game launch strategies adhere to the `GameRunner` trait and compose wrappers via `compose_command_tokens`.
- [ ] **Typed Error Handling**: Subsystem methods return `VapourResult<T>` and handle failures through `VapourError`.
- [ ] **Supervisor Safety**: Daemon lifecycle logic respects `CliSessionGuard` mutual exclusion and avoids sending `WM_DELETE_WINDOW` to modal or launch dialogs.
- [ ] **Brand & Assets**: Any updates to branding or media assets in [`assets/`](assets/) (`vapour-logo.svg`, `vapour-logo.png`) preserve the circular emblem format, scalable vector fidelity, and minimalist design aesthetic.
- [ ] **Documentation**: `README.md`, `CONTRIBUTING.md`, and `AGENTS.md` have been updated to reflect any API, flag, or architectural changes.
