<p align="center">
  <img src="assets/vapour-logo.svg" alt="Vapour logo" width="160" height="160">
</p>

# Vapour

A Steam launcher and lifecycle manager for Linux. It runs Steam without its desktop UI, moves Steam's idle memory into zRAM/swap while you play, and manages non-Steam games and Proton prefixes from the terminal.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Linux%20(systemd%20%2B%20cgroup%20v2)-lightgrey.svg)](https://www.kernel.org/)

> **Alpha.** Vapour has only been run on one machine (CachyOS, kernel 7.3, Hyprland). It starts and stops Steam, kills leftover Steam helper processes (`vapour prune`), and edits `shortcuts.vdf`, `config.vdf`, `localconfig.vdf` (launch options) and `appmanifest_*.acf`. Back up `~/.local/share/Steam/userdata` before trying it. See [Known limitations](#known-limitations).

## Why

Steam's desktop client keeps several Chromium (CEF) processes resident while a game runs. In one measured run they held 1213 MB of RAM. Vapour starts Steam headless, then asks the kernel to move that memory out to swap. In that run, Steam dropped to 44 MB of resident memory, with 946 MB moved to zRAM at 4.29x compression ([benchmark](benchmarks/BENCHMARK_RESULTS.md)). Your numbers will differ.

## How it works

- **Headless Steam.** If Steam isn't running, Vapour starts it with `-silent` and a few flags that disable UI subsystems, inside its own systemd scope (`systemd-run --user --scope -p Delegate=yes --unit=vapour-steam-<pid>`). Games launch through Steam's normal path (`-applaunch` / `-gameidlaunch`).
- **Memory reclaim.** Vapour writes `1500M swappiness=200` (the `[daemon]` defaults `reclaim_bytes_mb` and `swappiness`) to `memory.reclaim` in Steam's cgroups. On kernels before 6.11 it writes plain `1500M`. It skips the cgroups of the game and of wrappers such as gamescope that sit between Steam and the game. The CLI does this at T+12 s and T+40 s, the daemon at T+0 and T+25 s. Further passes run when a kernel PSI trigger reports memory pressure. These timings are empirical defaults.
- **The game is never touched.** `STEAM_LAUNCH_WRAPPER_SCOPE=1` makes Steam put each game in its own `app-steam-app<appid>-*.scope`, and Vapour never writes to that scope.
- **Exit and cloud sync.** Game exit is detected from the scope's `cgroup.events`, with pidfd and `/proc` polling as fallbacks. Before shutting Steam down, Vapour waits for that game's `Upload complete` line in `cloud_log.txt`.
- **Ownership.** Vapour only ever shuts down a Steam it started itself. A Steam you started is left running; if the daemon closed its window, it reopens it.

Details, sources and the tuning constants are in [ARCHITECTURE.md](ARCHITECTURE.md). Every kernel and Steam claim was checked against primary sources in [docs/research/verification.md](docs/research/verification.md).

## Requirements

- Linux 5.19+ (`memory.reclaim`); 6.11+ for the `swappiness=` bias
- systemd with the unified cgroup v2 hierarchy
- Swap: zRAM, zswap or a swap device. Without it, there is nowhere to move the memory.
- Steam (native package; the Flatpak is untested)
- Optional: `udisks2` for mounting library drives, `notify-send` for notifications, `gamemoderun`, `mangohud` and `gamescope` as wrappers

## Install

```bash
git clone https://github.com/peebstr/vapour.git
cd vapour
cargo build --release
install -Dm755 target/release/vapour ~/.local/bin/vapour
vapour doctor
```

`vapour doctor` checks the kernel features, cgroup permissions, swap, Steam install and wrappers. `vapour doctor --deep` also checks that every Steam flag Vapour passes still exists in your installed client.

## Usage

```bash
vapour                         # interactive picker; type "cs2", "tw3", a name or an AppID
vapour cs2                     # launch by name, initialism or AppID
vapour cs2 -novid +exec a.cfg  # extra arguments go to the game
vapour cs2 --dry-run           # show what would run
vapour cs2 --keep-steam        # leave Steam running after the game exits
```

When the input isn't a terminal, Vapour never prompts. A query that matches several games fails and lists them; an exact name always wins.

**Launcher keybinding** (Hyprland example; `--dmenu` prints `Name | AppID | Kind | Status`):

```bash
bind = $mainMod, G, exec, vapour list --dmenu | rofi -dmenu -i -p Steam | cut -d'|' -f2 | xargs vapour
```

**Non-Steam games:**

```bash
vapour add "Game" /path/to/Game.exe --compat "Proton Experimental" --launch-options "-novid"
vapour compat 730 GE-Proton9-23      # set the Proton version for a game
vapour --set-launch-options "Game" "gamemoderun %command% -novid"   # also works for Steam games
vapour rm "Game" --yes               # removes the shortcut and its Proton prefix
vapour "Game" --direct               # run through Proton without starting Steam
```

`--direct` bypasses the Steam client, so there's no overlay, Steam Input or Cloud sync for that launch.

**Monitoring and cleanup:**

```bash
vapour status [--json]          # Steam state, PSI, zRAM
vapour monitor                  # live HUD: [r] reclaim, [k] shut down Steam, [+/-] interval, [q] quit
vapour reclaim                  # one reclaim pass now
vapour prune --dry-run          # orphaned prefixes, shader caches, /dev/shm buffers, stray helpers
vapour desktop-entries [--clean]  # .desktop files in ~/.local/share/applications
```

`prune` never kills Wine processes, because Lutris, Heroic and umu use them too. It skips prefixes and caches whose `appmanifest` still exists. It skips directory pruning entirely if any library is unmounted or the game list can't be read.

**Background daemon** (for games launched from the Steam UI or a desktop menu):

```bash
vapour daemon --install     # systemd user service
vapour daemon --status
vapour daemon --foreground  # run in the terminal with logs
vapour daemon --uninstall
```

The daemon watches `app.slice` with inotify for new game scopes, then reclaims memory using the same engine as the CLI. If you started Steam yourself, it sends `WM_DELETE_WINDOW` to Steam's main window (Steam hides to the tray). It skips dialogs, splash screens and launch/update prompts, and needs X11 or Xwayland. It steps aside whenever a CLI launch holds the same AppID, including mid-game.

### Commands

| Command | Legacy flag | Purpose |
| :--- | :--- | :--- |
| `vapour [launch] <QUERY> [ARGS]` | | Launch a game |
| `vapour add <NAME> <EXE> [--compat T] [--launch-options O] [--icon P] [--tags T]` | `--add-game` | Add a non-Steam game |
| `vapour remove` / `rm <GAME>` | `--del-game` | Remove a non-Steam game and its prefix |
| `vapour compat <GAME> [TOOL]` | `--compat` | Show or set the Proton version |
| `vapour list [--json\|--dmenu\|--plain] [--sort recent\|size\|name]` | `-l` | List games |
| `vapour libraries [--add P] [--del P]` | `--libraries` | Manage library folders |
| `vapour config [KEY=VAL] [--get KEY]` | `-c`, `--set KEY=VAL` | Edit settings (interactive without arguments); keys: `overlay`, `zero_ram`, `nojoy` |
| `vapour info <GAME> [--json]` | `-i` | Show a game's runtime and configuration |
| `vapour doctor [--json] [--deep]` | `--doctor` | Check the system |
| `vapour status [--watch] [--interval S] [--json]` | `-s` | Steam state |
| `vapour monitor` | `-m` | Live HUD |
| `vapour reclaim` | `--reclaim` | Reclaim once |
| `vapour shutdown` / `kill` | `-k` | Shut Steam down gracefully |
| `vapour prune` / `clean` [`--dry-run`] [`--compat-only`\|`--shader-only`\|`--shm-only`\|`--proc-only`] | `--prune` | Clean up leftovers |
| `vapour desktop-entries` | | Generate `.desktop` files |
| `vapour daemon` | | Background supervisor |
| `vapour completions <SHELL>` | `--completions` | Shell completions |

Exit codes: `0` success, `1` runtime failure, `2` usage error or missing argument in non-interactive mode, `3` library drive unmounted (use `--yes` to mount), `4` Steam communication failure.

## Configuration

`~/.config/vapour/settings.toml`, edited by hand or with `vapour config`. An invalid file is an error; Vapour never resets it to defaults.

Launch settings resolve in this order: CLI flags, then `[games.<id>]`, then `[defaults]`, then the game's launch options in Steam's `localconfig.vdf`.

```toml
[defaults]
overlay = true         # Steam overlay; false suppresses it
zero_ram = true        # headless Steam flags and memory reclaim
nojoy = false          # pass -nojoy

[games.730]
extra_args = "-novid"                        # space separated
env = "MESA_VK_WSI_PRESENT_MODE=mailbox"     # KEY=VAL pairs separated by ";"
wrapper = "gamemoderun %command%"            # supports %command%

[games."Some Game"]
compat_tool = "Proton Experimental"
exe_path = "/home/user/Games/Game.exe"
start_dir = "/home/user/Games"
launch_options = "WINEDLLOVERRIDES=\"winhttp=n,b\" gamemoderun %command% -fullscreen"

[daemon]
auto_tray_gui = true          # close a user-started Steam's window to the tray on game start
restore_gui_on_exit = true    # reopen it after the game exits
reclaim_bytes_mb = 1500       # size of each memory.reclaim request
swappiness = 200              # swappiness= bias for it (kernel 6.11+)
psi_adaptive = true           # extra reclaim passes under memory pressure
notifications = true
shutdown_if_headless = true   # shut down a Steam the daemon's session owns
```

`launch_options` follows Steam's rules. Leading `KEY=VAL` tokens become environment variables, the other tokens before `%command%` become wrappers, and tokens after it become game arguments. Without `%command%`, everything is a game argument. If Steam is already running, a Steam-dispatched launch can't apply env or wrapper settings; Vapour warns, and you set those in Steam instead.

There are no `gamemode`, `mangohud` or `gamescope` switches. Put wrappers in the launch options (`gamemoderun gamescope -f -- %command%`). Old `gamemode`, `mangohud`, `gamescope` and `gamescope_args` keys are ignored and dropped the next time Vapour saves the file.

For a Steam game, `vapour --set-launch-options` and the `vapour config` menu show the current launch options from Steam's `localconfig.vdf` and replace them with what you type. Vapour refuses the change while the game is running. It also refuses while Steam is running, because Steam rewrites that file when it exits and would undo the edit; run `vapour shutdown` first. The first write saves a copy as `localconfig.vdf.vapour.bak`. A game Steam has never launched has no entry in the file, so launch it once from Steam first.

## Anti-cheat

Vapour never attaches to, reads from or signals a game process, and never writes to a game's cgroup. It only acts on Steam's own processes. It has not been tested against any anti-cheat system, so no compatibility is claimed.

## Known limitations

- Only tested on one machine. Kernels 5.19 to 6.10, other distros, GNOME/KDE and Steam Deck are untested.
- Steam behaviour is undocumented by Valve. Launch flags, log formats and scope names were checked against the client installed on 2026-11-16, and can change with any Steam update. `vapour doctor --deep` catches removed flags.
- The cloud-conflict check matches marker words that have never been seen in a real log, so it may not fire. When it fires, or an exit sync starts and never completes, the raw lines for that game are saved to `~/.cache/vapour/cloud_unknown.log`; attach them to a bug report.
- Run live only for CLI launches of one game (CS2, with and without gamescope): reclaim passes, exit detection and the Steam shutdown. Not yet exercised with Steam running: the daemon, the X11 window handling, PSI-trigger passes and the Cloud exit-sync wait.
- The reclaim timings and the 5 % PSI threshold are guesses that haven't been tuned across games.

## Development

```bash
cargo test && cargo clippy -- -D warnings && cargo fmt --check
```

CI runs the same checks plus a release build. The binary VDF code is fuzzed with `proptest`. `tests/mock_cgroup_lifecycle.rs` exercises cgroup discovery, PSI parsing and the FIFO probe without root or Steam. See [CONTRIBUTING.md](CONTRIBUTING.md) for conventions and [AGENTS.md](AGENTS.md) for the module map and invariants.

## License

[MIT](LICENSE)
