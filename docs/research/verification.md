# Verification of Vapour's technical claims

Date: 2026-11-16 · Test machine: Linux 7.3.4-1-cachyos, systemd 262.2, 16 GB zram0, `vm.swappiness=133`, Hyprland (uwsm) with Xwayland, Steam client with `steamrt64` installed. Steam was **not running** during the checks.

Method: claims were taken from AGENTS.md §3–4, ARCHITECTURE.md and `src/`. Five topic subagents checked them against primary sources, and the lead agent then re-checked every ❌ and ⚠️. Re-checking overturned one subagent verdict: the subagent said `AutoCloud complete` never appears in `cloud_log.txt`, but it appears 60 times (see C15).

Steam is mostly undocumented by Valve. For Steam facts, the evidence is the local client binaries (`strings -a` on `ubuntu12_32/steam`, `steamui.so`, `steamclient.so`, `steam-launch-wrapper`, `pressure-vessel-wrap`) and the local logs and VDFs, all dated 2026-11-16. Those claims are labelled "local".

## Summary

**Trust score: 66 %**, computed as (✅ + ½·⚠️) / (all claims − ❓) = (34 + 12) / 70.

| Verdict | Count |
|---|---|
| ✅ true | 34 |
| ⚠️ partly true / version-dependent | 24 |
| ❌ false | 12 |
| ❓ unverifiable without hardware or a running Steam | 9 |
| **Total** | **79** |

The kernel mechanisms are mostly sound: memory.reclaim, PSI, pidfd, delegation and zRAM. The Steam-facing heuristics are the weak part: cloud-sync detection, launch flags and scope names. The docs (ARCHITECTURE.md, AGENTS.md) also overstate several things.

The findings with the most impact:

1. **Cloud sync is declared complete before the upload finishes** (C15). On exit Steam logs `AutoCloud complete` and `already synced` *before* `Need to upload file …` and `Upload complete, result OK`. `parse_cloud_sync_complete` matches the first marker it sees.
2. **`systemctl --user stop vapour` skips the clean-exit path** (B6). `ctrlc` handles SIGTERM only when its `termination` feature is enabled, and it is not enabled here.
3. **The kernel floor is wrong** (A3). The doctor passes kernels from 5.14 up, but `memory.reclaim` and `memory.peak` need 5.19, and `swappiness=` needs 6.11.
4. **Non-interactive mounts can hang on polkit** (E13). `udisksctl` is called without `--no-user-interaction`, which breaks invariant 9.
5. **Five Steam flags don't exist in the current client** (C5, C6). Four are passed at launch (`-nominidumps`, `-nointro`, `-nochatui`, `-cef-delaypageload`). The fifth, `-no-browser`, is only used for headless detection.

## How `vapour doctor` and `vapour status` compare

Both commands ran on this machine and exited 0. `doctor --json` also exited 0.

| Doctor line | Output | Verdict after research |
|---|---|---|
| Linux Kernel | "Linux 7.3.4 (cgroups v2 memory.reclaim fully supported)" | Correct for 7.3, but the check is wrong: it would print the same for 5.14–5.18, which have no `memory.reclaim` (A3). It also does not say whether `swappiness=` (6.11) or `swappiness=max` (6.16) is available. |
| Cgroups v2 Reclaim | writable `…/user@1000.service/app.slice/memory.reclaim` | Correct. That file is mode `--w-------` and owned by the user, because systemd chowns `memory.reclaim` in delegated subtrees (A2, A15). |
| Memory / Swap | 16005 MB, zRAM active, swappiness 133 | Correct. 133 is exactly the example vm.rst gives for fast swap devices (A14). |
| Storage / Polkit | udisksctl available | Correct, but the doctor doesn't check that non-interactive mounts are safe (E13). |
| Steam IPC Daemon | stopped | Consistent with the system: `~/.steam/steam.pipe` is a FIFO and `steam.pid` is stale (C1, C19). |
| Container Isolation | "systemd transient scoping and 64-bit Steam Runtime (steamrt3c) verified … STEAM_LAUNCH_WRAPPER_SCOPE supported" | ⚠️ "supported" is inferred only from `systemd-run --version` succeeding, which says nothing about the wrapper (D13). "steamrt3c" is hard-coded text. A legacy `ubuntu12_64/` directory alone also counts as 64-bit (D12). |
| `vapour status` | PSI some avg10 0.00 %, zRAM 15.6 GB | Matches `/proc/pressure/memory` and `/proc/swaps`. |

Journal evidence from 2026-11-12/13 confirms the scope names in practice: `vapour-steam-17824.scope`, `app-steam-unknown-*.scope` and `app-steam-app730-19818.scope`, all directly under `app.slice`. The journal also records the reclaim effect: `app-steam-unknown-17973.scope` reached 906 MB swap peak and `vapour-steam-17824.scope` reached 239 MB.

## Claims table

### a) cgroup v2 memory

| # | Claim | Verdict | Min version | Source | Code location |
|---|---|---|---|---|---|
| A1 | `memory.reclaim` was added in 5.19 | ✅ | Linux 5.19 (commit 94968384dde1) | [cgroup-v2](https://docs.kernel.org/admin-guide/cgroup-v2.html), [KernelNewbies 5.19](https://kernelnewbies.org/Linux_5.19) | src/watcher.rs:482 |
| A2 | `memory.reclaim` is writable without root in the delegated user subtree | ✅ | 5.19; systemd must chown it (262 does) | `memory_reclaim()` in [mm/memcontrol.c v6.12](https://raw.githubusercontent.com/torvalds/linux/v6.12/mm/memcontrol.c) has no capability check; systemd `cg_set_access` in [cgroup-setup.c](https://raw.githubusercontent.com/systemd/systemd/main/src/shared/cgroup-setup.c) | src/doctor.rs:211-245 |
| A3 | Kernel ≥ 5.14 means "memory.reclaim fully supported", and < 5.14 means unavailable | ❌ | The real floor is 5.19, and 6.11 for `swappiness=` | A1, A4, A9 | src/doctor.rs:163-192 |
| A4 | `memory.reclaim` accepts `swappiness=<0-200>` | ⚠️ only from 6.11 | Linux 6.11 (commit 68cd9050d871) | [cgroup-v2](https://docs.kernel.org/admin-guide/cgroup-v2.html), [patch](https://lkml.iu.edu/hypermail/linux/kernel/2312.0/06922.html) | src/watcher.rs:516 |
| A5 | An unknown key returns EINVAL, so falling back to plain `"1500M"` is safe | ⚠️ inferred for 5.19–6.10; it also hides real EINVAL causes | 5.19 | v6.12 `memory_reclaim()` returns -EINVAL on unknown tokens | src/watcher.rs:516-521 |
| A6 | The `1500M` size suffix is accepted | ✅ | 5.19 | parsed by `memparse()` (memcontrol.c) | src/watcher.rs:516 |
| A7 | EAGAIN means a partial reclaim (pages are not rolled back), so treating it as success is correct | ✅ | 5.19 | [cgroup-v2](https://docs.kernel.org/admin-guide/cgroup-v2.html) "-EAGAIN is returned"; memcontrol.c retry loop | src/watcher.rs:524-532; invariant 8 |
| A8 | "Evicts inactive anon and file pages, prioritising zRAM" | ⚠️ Reclaim scans both LRUs, not only inactive pages. Only `swappiness=max` (6.16) is documented as anon-only. Before 6.11 the bias is silently dropped. | 6.11 / 6.16 | [cgroup-v2](https://docs.kernel.org/admin-guide/cgroup-v2.html) | ARCHITECTURE.md §2; src/watcher.rs:512-513 |
| A9 | `memory.peak` exists per cgroup | ✅ | 5.19 (commit 8e20d4b3332); per-fd reset in 6.12 | [cgroup-v2](https://docs.kernel.org/admin-guide/cgroup-v2.html), [KernelNewbies 6.12](https://kernelnewbies.org/Linux_6.12) | src/telemetry.rs:256 |
| A10 | Δ`memory.current` = "RAM freed" and Δ`memory.swap.current` = "moved to zRAM" | ⚠️ Δcurrent includes dropped page cache. The net saving is freed minus growth in zRAM `mem_used_total`. | — | [cgroup-v2](https://docs.kernel.org/admin-guide/cgroup-v2.html), [zram](https://docs.kernel.org/admin-guide/blockdev/zram.html) | src/watcher.rs:507-540; src/telemetry.rs:196-198 |
| A11 | PSI `/proc/pressure/memory` format, and `some avg10` = share of the last 10 s with at least one task stalled | ✅ | 4.20; on by default unless `CONFIG_PSI_DEFAULT_DISABLED` | [psi](https://docs.kernel.org/accounting/psi.html) | src/watcher.rs:330-382 |
| A12 | `some avg10 ≥ 5.0` is the right moment to reclaim | ❓ An empirical threshold; avg10 lags real stalls by up to about 10 s | — | no source | src/watcher.rs:923-935; src/daemon.rs:891-905 |
| A13 | zRAM `mm_stat` field order (orig, compr, mem_used_total, …) | ✅ | long-standing | [zram](https://docs.kernel.org/admin-guide/blockdev/zram.html) | src/telemetry.rs:142-170 |
| A14 | `vm.swappiness` ranges 0–200, and values > 100 suit zRAM | ✅ | 5.8 | [vm.rst](https://docs.kernel.org/admin-guide/sysctl/vm.html) | src/doctor.rs:283-288 |
| A15 | user@UID.service delegates `+memory`, and its files are user-writable | ⚠️ True on systemd 262 (`Delegate=pids memory cpu`). The systemd release that started chowning `memory.reclaim` is unknown. | systemd ≥ 236 for controller lists | [CGROUP_DELEGATION](https://systemd.io/CGROUP_DELEGATION/), [user@.service.in](https://raw.githubusercontent.com/systemd/systemd/main/units/user%40.service.in) | ARCHITECTURE.md §2; src/doctor.rs:211-245 |
| A16 | Reclaim runs at T+12 s and T+40 s | ❓ The values are empirical. ⚠️ This holds only for the CLI watcher; the daemon reclaims at T+0 and T+25 s. | — | no source | src/watcher.rs:909-918 vs src/daemon.rs:797-813, 871-873 |
| A17 | PSI reclaim is "rate-limited to max 4 passes/session" | ⚠️ True for the CLI only (4 passes, 15 s cooldown). The daemon has no cap and a 10 s cooldown. | — | code | src/watcher.rs:926-927 vs src/daemon.rs:891 |

### b) Process lifecycle

| # | Claim | Verdict | Min version | Source | Code location |
|---|---|---|---|---|---|
| B1 | `pidfd_open` exists, called through a raw syscall | ✅ | 5.3 | [pidfd_open(2)](https://man7.org/linux/man-pages/man2/pidfd_open.2.html) | src/watcher.rs:14-35 |
| B2 | A pidfd polls readable (POLLIN) on exit, including for non-child processes | ✅ | 5.3 | same | src/watcher.rs:37-58 |
| B3 | pidfd "eliminates" PID recycling | ⚠️ It narrows the race. The window between the `/proc` scan and `pidfd_open` remains. | 5.3 | same | ARCHITECTURE.md §7; src/watcher.rs |
| B4 | The fallback to `/proc` happens on ENOSYS | ⚠️ The code falls back on any error. That is harmless, but the doc is wrong. | — | code | src/watcher.rs:24-34 |
| B5 | SIGCHLD set to `SIG_IGN` with `SA_NOCLDWAIT` prevents zombies | ✅ Side effect: `waitpid` then blocks and fails with ECHILD, so Rust's `wait`/`status`/`output` return `Err` inside the daemon. Today's callers ignore the result. | any | [sigaction(2)](https://man7.org/linux/man-pages/man2/sigaction.2.html), [wait(2)](https://man7.org/linux/man-pages/man2/wait.2.html) | src/daemon.rs:671-679, 71-72, 533-534, 725-726 |
| B6 | The daemon exits cleanly on SIGTERM ("Safe SIGTERM check") | ❌ `ctrlc` 3.5.2 handles SIGTERM only with its `termination` feature, which Cargo.toml does not enable | — | ctrlc 3.5.2 `src/lib.rs:45` and `src/platform/unix/mod.rs:98` (local registry copy) | Cargo.toml:15; src/daemon.rs:663-668, 914 |
| B7 | An inotify `IN_CREATE` watch on `app.slice` sees new game scopes | ✅ The scopes land directly in `app.slice`, confirmed with a probe and the journal. Not recursive. | — | [inotify(7)](https://man7.org/linux/man-pages/man7/inotify.7.html) | src/daemon.rs:711-745 |
| B8 | Games can be found through `SteamAppId`/`SteamGameId` in `/proc/<pid>/environ` | ✅ This is the initial environment only, and needs ptrace-read access. | — | [proc_pid_environ(5)](https://man7.org/linux/man-pages/man5/proc_pid_environ.5.html) | src/daemon.rs:575-605 |
| B9 | Matching `comm == "steam"` is safe (15-character limit) | ✅ | — | [proc_pid_comm(5)](https://man7.org/linux/man-pages/man5/proc_pid_comm.5.html) | src/launcher.rs:50-57 |
| B10 | A 5.6 s grace window (7 × 800 ms) covers launcher handoff | ❓ empirical | — | no source | src/watcher.rs:697-745 |
| B11 | The daemon declares exit after 3 empty 2 s ticks (~6 s) | ❓ empirical | — | no source | src/daemon.rs:560-571 |
| B12 | `libc::kill` cuts latency "from ~12 ms to sub-microsecond" | ⚠️ The direction is right, but the numbers are illustrative: fork+exec takes about 1 ms and the syscall about 1 µs | — | no source | ARCHITECTURE.md §5 |
| B13 | A `cloud_log.txt` written within the last 2 s means a sync is in flight | ❓ An empirical backstop | — | no source | src/watcher.rs:1080-1102; invariant 7 |
| B14 | SIGTERM, then SIGKILL 50 ms later, for orphan helpers | ⚠️ No source, and aggressive for Chromium helpers | — | — | src/prune.rs:485-500 |

### c) Steam client

| # | Claim | Verdict | Min version | Source | Code location |
|---|---|---|---|---|---|
| C1 | `~/.steam/steam.pipe` is a FIFO; opening it with `O_WRONLY\|O_NONBLOCK` fails with ENXIO when nothing is reading | ✅ | current client (local `stat`: fifo) | [open(2)](https://man7.org/linux/man-pages/man2/open.2.html) ENXIO, [fifo(7)](https://man7.org/linux/man-pages/man7/fifo.7.html) | src/launcher.rs:16-35; invariant 5 |
| C2 | `/dev/shm/u<UID>-ValveIPCSharedObj-Steam` means the client is ready | ❓ Steam was stopped, so this could not be observed | — | — | src/launcher.rs:37-43 |
| C3 | `-silent`, `-applaunch`, `-gameidlaunch` | ✅ | current client (local) | strings in `steamui.so` | src/runner.rs:177, 330-332 |
| C4 | `-nocrashdialog -vrdisable -nofriendsui -nojoy -noshaders -cef-force-occlusion -cef-disable-site-isolation -cef-disable-hang-timeouts -cef-disable-breakpad -cef-disable-gpu -disable-overlay-gpu-pri -noverifyfiles` | ✅ | current client (local) | strings in the client binaries | src/launcher.rs:148-171 |
| C5 | `-nominidumps -nointro -nochatui -cef-delaypageload` | ❌ Found in none of the binaries checked (0 hits each) | — | local `strings` | src/launcher.rs:163-170 |
| C6 | `-no-browser` marks a headless Steam | ❌ 0 hits in the client. Headless detection really depends only on `-silent`. | — | local `strings` | src/daemon.rs:610-635 |
| C7 | `steam -shutdown` stops the client | ✅ | current client (local, 18 hits) | local `strings` | src/launcher.rs:359-371 |
| C8 | Overlay suppression variables | ⚠️ `DISABLE_VK_LAYER_VALVE_steam_overlay_1=1` matches the layer JSON's `disable_environment`. `ENABLE_…=0` does nothing (the layer expects `1`). `PRESSURE_VESSEL_REMOVE_GAME_OVERLAY` and `SteamNoOverlayUIDrawing` are present in the binaries, but their behaviour is undocumented. | — | `~/.local/share/vulkan/implicit_layer.d/steamoverlay_x86_64.json`; [Vulkan loader layer env](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderLayerInterface.md) | src/launcher.rs:226-232; src/runner.rs:167-170, 320-323 |
| C9 | Long shortcut AppID = `(id32 << 32) \| 0x02000000` | ✅ Secondary sources only (open-source reimplementations) | — | e.g. Steam-ROM-Manager, BoilR | src/launcher.rs:45-48 |
| C10 | Shortcut AppID = `CRC32(exe+name) \| 0x80000000` | ⚠️ This is the legacy formula. The `appid` values Steam stores locally don't match it. Vapour reads the stored value (non_steam.rs:358), so only shortcuts it creates use the formula. | — | local `shortcuts.vdf` | src/non_steam.rs:91-110; invariant 3 |
| C11 | Binary VDF types 0x00/01/02/07/08 | ✅ A correct subset (0x03–0x06, 0x0A and 0x0B also exist) | — | [ValvePython/vdf](https://github.com/ValvePython/vdf/blob/master/vdf/__init__.py) (secondary) | ARCHITECTURE.md §6 |
| C12 | `AutoUpdateBehavior "1"` = update only on launch | ⚠️ Community consensus only; Valve doesn't document it | — | forums only | src/library.rs:617-660 |
| C13 | `LaunchOptions` sits under the per-app key in `localconfig.vdf` | ✅ | current client (local) | local file | src/library.rs:670-730 |
| C14 | `CompatToolMapping` is in `config/config.vdf` | ✅ | current client (local) | local file | src/non_steam.rs |
| C15 | `Upload complete, result OK`, `AutoCloud complete` and `already synced` each mean the exit sync is done | ❌ On exit Steam logs `Starting sync (up,AC Exit,)` → `AutoCloud complete` → `Currently already synced …` → `Need to upload file …` → `Upload OK …` → `Upload complete, result OK` (or `Upload complete in build list` when nothing changed). Only the last line means done. | current client (local, 2026-11-16 00:26) | `~/.local/share/Steam/logs/cloud_log.txt` lines ~4700-4732 | src/watcher.rs:298-305, 1019-1031 |
| C16 | `conflict` / `mismatch` / `files differ` mark a cloud conflict | ❓ 0 hits in 4,931 log lines. The real conflict format has not been seen. | — | local log | src/watcher.rs:307-320 |
| C17 | Only bytes appended after game exit are inspected (`cloud_offset = len`) | ✅ The code does this | — | code | src/watcher.rs:994-1000; invariant 6 |
| C18 | `steam steam://open/games` restores the library window | ❓ | — | — | src/daemon.rs:531-537 |
| C19 | `~/.steam/steam.pid` holds the client PID | ✅ It goes stale after exit, which the code handles through the `comm` check | — | local file | src/launcher.rs:54-75 |

### d) Steam Linux Runtime and systemd

| # | Claim | Verdict | Min version | Source | Code location |
|---|---|---|---|---|---|
| D1 | `STEAM_LAUNCH_WRAPPER_SCOPE=1` puts each game in `app-steam-app<id>-<pid>.scope` | ✅ The variable is the opt-in equivalent of `--scope`. Without it, the wrapper stays in the caller's cgroup. | steam-launch-wrapper 0.20240918 (local) | `strings ubuntu12_32/steam-launch-wrapper`: `STEAM_LAUNCH_WRAPPER_SCOPE`, `app-steam-app%s-%d.scope`, `StartTransientUnit`; a probe confirmed it | src/launcher.rs:220-224; src/daemon.rs:1017-1035 |
| D2 | `PRESSURE_VESSEL_SYSTEMD_SCOPE=1` is "the default" and makes pressure-vessel run `systemd-run --user --scope` | ❌ pressure-vessel calls D-Bus `StartTransientUnit` itself, and has no `systemd-run` string. Whether it is on by default is unknown. | SLR 3 (local) | `strings …/pressure-vessel/bin/pressure-vessel-wrap` | ARCHITECTURE.md §3; README.md |
| D3 | `app-steam-unknown-*.scope` is the "CEF WebHelper pool" | ⚠️ The name is real (pressure-vessel fills `app-%s-%s-%s` with `unknown` when there is no app id). What runs inside it is unproven; most of these scopes live 1–5 s. | — | local strings; journal (90 such scopes) | src/watcher.rs:405-440 |
| D4 | `app-steamwebhelper-*.scope` exists | ❌ Not in any wrapper binary and never in the journal | — | local strings and journal | src/watcher.rs:407, 429, 1259 |
| D5 | Excluding `app-steam-app*` protects the game | ⚠️ The `starts_with` check on the scope name is fine. The `/proc` path check at watcher.rs:456 uses `contains`, which also matches ancestor directories. | — | [DESKTOP_ENVIRONMENTS](https://systemd.io/DESKTOP_ENVIRONMENTS/) | src/watcher.rs:424, 456 |
| D6 | The scopes land in `app.slice` | ✅ Even under a uwsm session. The main `steam` process itself may sit in `session-N.scope`, which the `/proc` fallback covers. | systemd user manager default | probe: `systemd-run --user --scope` lands in `app.slice` | src/daemon.rs:690-745; src/watcher.rs:410-466 |
| D7 | `systemd-run --user --scope --unit=vapour-steam-<pid>` forwards the environment and lives until its last process exits | ✅ | any modern systemd | [systemd-run(1)](https://www.freedesktop.org/software/systemd/man/latest/systemd-run.html); probe; journal shows a 39-minute `vapour-steam-*` scope | src/launcher.rs:197-215 |
| D8 | That scope's `memory.reclaim` is user-writable | ✅ Mode 0200, owned by the user. ⚠️ systemd.io says not to write attributes of cgroups that weren't delegated to you, and this scope is not delegated. | — | [CGROUP_DELEGATION](https://systemd.io/CGROUP_DELEGATION/) | src/watcher.rs:502-521 |
| D9 | `~/.config/environment.d/vapour.conf` reaches Steam | ⚠️ It reaches what the user manager spawns: services, uwsm apps, `systemd-run`. It does not reach a Steam started from a login shell (seen here in `session-29.scope`). It takes effect only after re-login or `set-environment`. | systemd ≥ 233 (not re-fetched) | [environment.d(5)](https://www.freedesktop.org/software/systemd/man/latest/environment.d.html) | src/daemon.rs:1017-1035 |
| D10 | The direct runners keep "100 % compatibility" with VAC, Cloud, Steam Input, Workshop and SLR | ❌ This holds only for `steam -applaunch`. `ProtonDirectRunner` and `NativeDirectRunner` bypass the client, so there is no overlay, Cloud or Steam Input. | — | [Proton README](https://github.com/ValveSoftware/Proton) | ARCHITECTURE.md §1; src/runner.rs:449-600 |
| D11 | A `vapour-steam-client` scope exists | ❌ The code only creates `vapour-steam-<pid>` | — | `grep` returns nothing | ARCHITECTURE.md §3 table |
| D12 | The doctor detects the 64-bit client (steamrt64) | ⚠️ steamrt64 is real, but a legacy `ubuntu12_64/` directory alone also counts as 64-bit | — | local layout | src/doctor.rs:379-387 |
| D13 | "STEAM_LAUNCH_WRAPPER_SCOPE supported" | ⚠️ This is inferred from `systemd-run --version`, which says nothing about the wrapper | — | code | src/doctor.rs:373-432 |

### e) Desktop integration

| # | Claim | Verdict | Min version | Source | Code location |
|---|---|---|---|---|---|
| E1 | Windows are found through `_NET_CLIENT_LIST`, falling back to `XQueryTree` | ✅ | EWMH 1.x | [EWMH](https://specifications.freedesktop.org/wm-spec/latest/) | src/daemon.rs:300-350 |
| E2 | `WM_DELETE_WINDOW` ClientMessage | ⚠️ The format is correct (WM_PROTOCOLS, format 32, `l[0]` = atom, NoEventMask). `l[1]` is CurrentTime, and `WM_PROTOCOLS` is never checked for `WM_DELETE_WINDOW` support. | ICCCM 2.0 §4.2.8.1 | [ICCCM](https://x.org/releases/X11R7.7/doc/xorg-docs/icccm/icccm.html) | src/daemon.rs:455-500 |
| E3 | Windows are "unmapped to tray" and "Chromium tears down its GPU context" | ⚠️ The code sends a close request and never calls `XUnmapWindow`. Hiding to tray is Steam's choice, and the GPU teardown is unverified. | — | ICCCM | AGENTS.md §3J; ARCHITECTURE.md §8 |
| E4 | The DIALOG, SPLASH and NOTIFICATION window types are protected, and every entry in the type list is checked | ✅ | EWMH 1.4 (NOTIFICATION) | [EWMH](https://specifications.freedesktop.org/wm-spec/latest/) | src/daemon.rs:316-320, 418-448 |
| E5 | The CEF carousel refetches swapped memory every 20 s while the window is mapped | ❓ empirical | — | no source | ARCHITECTURE.md §8 |
| E6 | `notify-send -a -i --` | ✅ | libnotify | [libnotify](https://gnome.pages.gitlab.gnome.org/libnotify/) | src/daemon.rs:55-75 |
| E7 | Escaping markup in notifications | ⚠️ Correct only when the server advertises `body-markup`; otherwise `&amp;` shows literally | Notification spec 1.2 | [markup](https://specifications.freedesktop.org/notification/latest/markup.html) | src/daemon.rs:48-60 |
| E8 | The `.desktop` keys conform to the spec | ✅ | Desktop Entry 1.5 | [desktop-entry](https://specifications.freedesktop.org/desktop-entry/latest/) | src/desktop.rs:65-80 |
| E9 | `.desktop` values are safe for any game name | ⚠️ `Name`/`Comment` are not escaped (`\`, newline) | 1.5 | [value types](https://specifications.freedesktop.org/desktop-entry/latest/value-types.html) | src/desktop.rs:65-80 |
| E10 | `StartupNotify=true` and `Exec=vapour …` | ⚠️ vapour never maps a window, so launchers show a busy cursor until their timeout. A bare `vapour` depends on the launcher's PATH. | 1.5 | [startup-notification](https://specifications.freedesktop.org/startup-notification/latest/) | src/desktop.rs:72-76 |
| E11 | `udisksctl mount -b` works unprivileged through polkit | ✅ | udisks2 | [udisksctl(1)](https://storaged.org/doc/udisks2-api/latest/udisksctl.1.html) | src/mount.rs:159-190; invariant 4 |
| E12 | Mount paths decode octal escapes (`\040`, …) | ✅ | — | [proc_pid_mountinfo(5)](https://man7.org/linux/man-pages/man5/proc_pid_mountinfo.5.html) | src/mount.rs:58-88 |
| E13 | Mounting never hangs in non-interactive mode (invariant 9) | ❌ `--no-user-interaction` exists (confirmed locally with `udisksctl mount --help`) but is never passed | udisks2 | udisksctl(1) | src/mount.rs:166-169; src/cli/launch.rs:160-182 |

### f) Persistence

| # | Claim | Verdict | Min version | Source | Code location |
|---|---|---|---|---|---|
| F1 | `write_atomic` calls `flush()` and `sync_all()` for physical persistence | ❌ It does `fs::write` then `rename` with no fsync. That is atomic against a killed process, but not durable across power loss. | — | [rename(2)](https://man7.org/linux/man-pages/man2/rename.2.html), [fsync(2)](https://man7.org/linux/man-pages/man2/fsync.2.html) | src/non_steam.rs:119-130; ARCHITECTURE.md §6 |
| F2 | All persistent files go through `write_atomic` (invariant 12) | ❌ Bypassed by `.desktop` files, the systemd unit, environment.d, the icon and the CLI guard pid | — | code | src/desktop.rs:109, 142; src/daemon.rs:28, 95, 1008, 1023 |
| F3 | The temp file is named `.<filename>.tmp.<pid>` | ⚠️ The code uses `<stem>.tmp.<pid>`, which matches invariant 12 but not ARCHITECTURE.md | — | code | src/non_steam.rs:123 |

## Improvements, ranked by impact ÷ effort

| Rank | Change | File | Source | Gain | Effort |
|---|---|---|---|---|---|
| 1 | Treat only `Upload complete, result OK` or `Upload complete in build list` for the AppID as completion, and only after `Starting sync (up,AC Exit` has been seen. Drop `AutoCloud complete` and `already synced`. | src/watcher.rs:298-305 | C15, local cloud_log | Robustness: the "verified complete" claim becomes true, and shutdown no longer races the upload | S |
| 2 | `ctrlc = { version = "3.4", features = ["termination"] }` | Cargo.toml:15 | B6, ctrlc source | Robustness: `systemctl --user stop` takes the clean path | S |
| 3 | Pass `--no-user-interaction` to `udisksctl` when `!is_interactive()`, and map failure to exit code 3 | src/mount.rs:166 | E13, udisksctl(1) | Robustness: restores invariant 9 | S |
| 4 | Replace the kernel-version gate with feature probes: does `memory.reclaim` exist and is it writable, and is `swappiness=` accepted (probe once, cache the result). The floor becomes 5.19. | src/doctor.rs:163-192; src/watcher.rs:516-521 | A1, A3, A4 | Compatibility: correct doctor output, no double write on 5.19–6.10 | S |
| 5 | Remove the dead flags (`-nominidumps -nointro -nochatui -cef-delaypageload`), the no-op `ENABLE_…=0`, and `-no-browser`. Detect "vapour-started Steam" by its `vapour-steam-*.scope` cgroup instead of `-silent`. | src/launcher.rs:163-170, 230; src/daemon.rs:610-635 | C5, C6, C8 | Robustness: a user's tray-autostarted `steam -silent` is no longer treated as headless and shut down (see open question 6) | S |
| 6 | Route `.desktop`, unit and environment.d writes through `write_atomic`, and add `File::sync_all()` before the rename | src/non_steam.rs:119-130; src/desktop.rs:109, 142; src/daemon.rs:1008, 1023 | F1, F2 | Robustness: invariant 12 actually holds | S |
| 7 | Clean up the scope filter: drop `app-steamwebhelper-`, and replace `contains("app-steam-app")` on the full path with a check on the leaf name | src/watcher.rs:407, 429, 456 | D4, D5 | Correctness and less dead code | S |
| 8 | Install a no-op `XSetErrorHandler` (through dlsym), and check `WM_PROTOCOLS` before sending `WM_DELETE_WINDOW` | src/daemon.rs:195-500 | E2; Xlib's default handler calls `exit()` on BadWindow | Robustness: a window that disappears mid-scan can't kill the daemon | M |
| 9 | Spawn the Steam scope with `-p Delegate=yes`, so vapour writes only to a delegated cgroup | src/launcher.rs:205-209 | D8, [CGROUP_DELEGATION](https://systemd.io/CGROUP_DELEGATION/) | Compliance with the systemd contract | S, but needs a test that systemd still accounts for the scope |
| 10 | Detect game exit with an inotify `IN_MODIFY` watch on `<game scope>/cgroup.events` (`populated 0`), keeping the PID scan as a fallback | src/daemon.rs:833-870; src/watcher.rs:697-745 | [cgroup-v2 cgroup.events](https://docs.kernel.org/admin-guide/cgroup-v2.html) | Latency and robustness: removes the 5.6 s and 6 s grace guesses (B10, B11), since a launcher handoff keeps the scope populated | M |
| 11 | Replace avg10 polling with PSI triggers: write `some 300000 2000000` and `poll(POLLPRI)`. Unprivileged since 6.4 when the window is a multiple of 2 s; fall back to polling below that. | src/watcher.rs:923-935; src/daemon.rs:891-905 | [psi](https://docs.kernel.org/accounting/psi.html) | Latency and idle CPU | M |
| 12 | Try `swappiness=max` on 6.16+ before 200, and report net savings as freed minus Δ zRAM `mem_used_total` | src/watcher.rs:507-540; src/telemetry.rs | A8, A10 | Memory: anon-only reclaim keeps Steam's file cache. Also honest numbers. | S–M |
| 13 | Open the pidfd at discovery time and signal through `pidfd_send_signal` | src/watcher.rs:1095-1101; src/prune.rs:493 | [pidfd_send_signal(2)](https://man7.org/linux/man-pages/man2/pidfd_send_signal.2.html) | Robustness: makes the "no PID-reuse race" claim (B3) true | M |
| 14 | Escape `.desktop` values, use an absolute `Exec`, and set `StartupNotify=false` | src/desktop.rs:65-80 | E9, E10 | Compatibility | S |

### Conflicts with the AGENTS.md §4 invariants

None of the ideas above breaks an invariant. Ideas 3, 6 and 8 restore invariants 9, 12 and 14, which the code currently violates or under-protects. Three invariants are inaccurate as written and should be revised, not the ideas:

- **Invariant 7** (the 2 s `cloud_log.txt` mtime window): an empirical guess (B13). Once idea 1 is in, the real signal is the per-AppID `Upload complete` line. Keep the 2 s mtime check as a backstop and say so in the invariant.
- **Invariant 12** (`write_atomic` for all persistent files): atomicity without fsync is not "prevents corruption from abrupt termination" on power loss. Idea 6 should be applied and the invariant should say "atomic and fsync'd".
- **Invariant 3** (bit 31): correct, but the CRC32 formula is only Steam's legacy ID (C10). The invariant should say vapour always prefers the `appid` stored in `shortcuts.vdf` and uses the formula only for new shortcuts.

Idea 10 (cgroup.events) would retire the "launcher handoff grace" text in AGENTS.md §3J and ARCHITECTURE.md §7. That text describes the design but is not an invariant, so the docs should change.

### Doc corrections that need no code change

- AGENTS.md §3D and ARCHITECTURE.md §4: the daemon reclaims at T+0 and T+25 s with an uncapped PSI tier. Only the CLI does T+12 s / T+40 s with at most 4 PSI passes (A16, A17).
- ARCHITECTURE.md §3: pressure-vessel uses D-Bus `StartTransientUnit`, not `systemd-run`. Game scopes come from `steam-launch-wrapper` and `STEAM_LAUNCH_WRAPPER_SCOPE`. Remove `vapour-steam-client` (D2, D11).
- ARCHITECTURE.md §1: limit "100 % compatibility" to the `-applaunch` path (D10).
- ARCHITECTURE.md §7: "narrows" PID recycling rather than "eliminates" it, and the fallback is on any error, not only ENOSYS (B3, B4).
- ARCHITECTURE.md §2: `swappiness=` needs 6.11 and is silently ignored before that. Reclaim is not limited to inactive pages (A4, A8).

## Open questions (need hardware or a running Steam)

1. Does Steam hide to tray or exit when it receives `WM_DELETE_WINDOW`, and does unmapping really stop the CEF render loop and the 20 s refetch (E3, E5)? Test: run a game with the daemon in the foreground, and watch `memory.swap.current` on `app-steam-unknown-*` over 10 minutes with the window mapped and then unmapped.
2. What does a real cloud conflict log line look like (C16)? This needs two machines editing the same save offline.
3. Does `steam -shutdown` itself wait for an in-flight upload? If so, C15 is cosmetic rather than a risk to save data.
4. Which processes live in `app-steam-unknown-*` scopes? Check `cgroup.procs` while Steam runs (D3).
5. Does the real Steam client pass `--scope` to the launch wrapper by default, without the variable? environment.d has been active since before the journal began, so the journal can't tell (D1).
6. Is a user's Steam autostart started with `-silent`? If so, the daemon treats it as headless and shuts it down after a game (C6, improvement 5). Steam's autostart entry was not present on this machine.
7. Does `IN_CREATE` fire reliably on cgroupfs for a scope created by systemd (B7)? This is likely, since it is a VFS mkdir, but it hasn't been observed with this daemon.
8. Are the July 2026 fixes for `swappiness=max` evicting file pages (commits e26f7a91, 430e4cdc) in 7.3? This decides idea 12 on this machine.
9. Tuning: the PSI threshold of 5 % (A12), the reclaim times (A16), the handoff grace (B10, B11) and the SIGKILL delay of 50 ms (B14) all need measurement under real games.
10. Are the flags missing from the client (C5) harmlessly ignored, or do they show up in `stderr_log`?

## Changes after the fix round

Applied 2026-11-16 (working tree, uncommitted). "Resolved" means the code or docs no longer make the false claim; nothing marked ❓ was changed.

| Claim | Status after | What changed |
|---|---|---|
| A3 kernel floor 5.14 | resolved | `classify_kernel`: Fail < 5.19, Warn 5.19-6.10, Ok 6.11+. The doctor also prints one line per probed capability (`Caps`). |
| A4/A5 `swappiness=` | resolved | First `EINVAL` is remembered; plain `1500M` is written afterwards. Docs state the 6.11 floor. |
| A8, A10 "inactive pages", "freed" | resolved (docs) | Docs say reclaim scans both LRUs and that "freed" includes page cache. `swappiness=max` (6.16) is still not used. |
| A16, A17 schedules | resolved (docs) | The CLI/daemon difference is documented and in the tuning-constants table. |
| B3, B4 pidfd | resolved (docs + code) | Docs say "narrows", falls back on any error. Signals are sent through a pidfd; prune keeps one pidfd across SIGTERM and SIGKILL. |
| B6 SIGTERM | resolved | `ctrlc` `termination` feature enabled; `vapour daemon --foreground` + `kill -TERM` printed "Supervisor cleanly stopped." |
| B10, B11 grace ticks | partly resolved | `cgroup.events` (`populated 0`) is the primary exit signal; ticks are the fallback. The 3 x 2 s constant remains ❓. |
| B12 latency figure | resolved (docs) | Removed. |
| B14 SIGKILL after 50 ms | resolved | SIGTERM, then poll up to 1 s, then SIGKILL. |
| C5, C8 dead flags / env var | resolved | `-nominidumps -nointro -nochatui -cef-delaypageload` and `ENABLE_VK_LAYER_VALVE_steam_overlay_1=0` removed. |
| C6 `-no-browser` / headless | resolved | Ownership is decided by the `vapour-steam-*.scope` cgroup, not by flags. |
| C15 cloud completion | resolved | `cloud_step` state machine tested against a sanitized real-log fixture. |
| D2, D4, D11 scope names | resolved | Docs fixed; `app-steamwebhelper-` removed from code and tests. |
| D5 scope filter | resolved | Leaf-name check. |
| D8 non-delegated scope | resolved | `systemd-run -p Delegate=yes`; probe showed `memory.reclaim` still answers (EAGAIN) and accounting is intact. |
| D10 "100 % compatibility" | resolved (docs) | Limited to the `-applaunch` path. |
| D12, D13 doctor wording | resolved | steamrt64 required for "64-bit"; the wrapper message reports only whether the variable is set. |
| E2 `WM_PROTOCOLS`, Xlib errors | resolved in code, untested live | Error handler + `WM_PROTOCOLS` check added; needs a running Steam to exercise. |
| E3 "unmaps" | resolved (docs) | Docs say close request. |
| E9, E10 `.desktop` | resolved | Values escaped, absolute `Exec`, `StartupNotify=false`. |
| E13 udisksctl prompt | resolved | `--no-user-interaction` when non-interactive. |
| F1, F2, F3 writes | resolved | `write_atomic` fsyncs; unit file, environment.d, icons, `.desktop` and the ACF patch go through it. The `/dev/shm` lock is exempt by design. |

Still ❓ and untouched: C2, C16, C18, E5, E7, C12, and the tuning constants.
