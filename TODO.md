# TODO

Open work after the 0.0.1-alpha.1 release, ordered by value per effort. Each item has the file and the reason.

## Next

- [ ] Live test with Steam running. Run five sessions: CLI launch with a vapour-started Steam, CLI launch with your own Steam open, `--keep-steam`, a daemon session with the Steam window open, and `systemctl --user stop vapour` mid-game. Also edit a save and confirm `Upload complete` appears in `cloud_log.txt` before Steam shuts down. The session engine has never run against a real game.
- [ ] Make `[daemon] reclaim_bytes_mb` and `swappiness` work (`src/config.rs:186-187`). Reclaim hardcodes `1500M swappiness=200`; build the string from the config instead.
- [ ] Add a GitHub issue template asking for `vapour doctor --json --deep`, the distro, the kernel, and the relevant `cloud_log.txt` lines.
- [ ] Log unknown cloud-log lines. When a per-AppID line contains "conflict", or the drift warning fires, append the raw lines to `~/.cache/vapour/cloud_unknown.log`. The conflict markers are still a guess.
- [ ] Strip ANSI colours when stdout is not a terminal (`src/session.rs`, `src/daemon.rs`, about 46 `println!` calls). Under systemd they end up in the journal as escape codes.

## Then

- [ ] Add a test that runs `GameSession::run` against a temporary scope directory whose `cgroup.events` flips to `populated 0`. Nothing exercises `run()` today; the PSI-latch bug got past every test.
- [ ] Move the 47 `unsafe` blocks into `src/x11.rs` (the dlopen'd libX11 calls) and `src/sys.rs` (pidfd, inotify, poll, PSI).
- [ ] Report the net saving after reclaim: RSS freed minus the growth in zRAM `mem_used_total` (verification.md A10).
- [ ] Add a CI job that attaches a release binary on tag push, and maybe an AUR `PKGBUILD`.

## Later

- [ ] Measure the real minimum Rust version (e.g. `cargo msrv` in CI) and set `rust-version` in `Cargo.toml`. It is at least 1.82 because of `is_none_or`.
- [ ] Split the binary VDF parser out of `src/non_steam.rs` (1,914 lines).
- [ ] If testers' Steam scopes land in nested slices, watch those too. The daemon only watches direct children of `app.slice`, and the `/proc` fallback still finds the game, just more slowly.
