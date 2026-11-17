# TODO

Open work after the 0.0.1-alpha.1 release, ordered by value per effort. Each item has the file and the reason.

## Next

- [ ] Finish the live test with Steam running. Done: a CLI launch with a vapour-started Steam (CS2, with and without gamescope), and quitting the game. Still open: a CLI launch with your own Steam open, `--keep-steam`, a daemon session with the Steam window open, `systemctl --user stop vapour` mid-game, and a save edit with `Upload complete` checked in `cloud_log.txt` before Steam shuts down.
- [ ] Strip ANSI colours when stdout is not a terminal (`src/session.rs`, `src/daemon.rs`, about 46 `println!` calls). Under systemd they end up in the journal as escape codes.
- [ ] Test `adopt_session_env()` (`src/launcher.rs`) over a real SSH session. `XAUTHORITY` is not handled.
- [ ] `game_launch_chain_cgroups()` also protects `vapour-steam-*.scope` without a wrapper (measured: 47 MB, 0 swap), because Steam's launcher chain runs there. Accepted; narrow it only if that scope grows.

## Then

- [ ] Add a test that runs `GameSession::run` against a temporary scope directory whose `cgroup.events` flips to `populated 0`. Nothing exercises `run()` today; the PSI-latch bug got past every test.
- [ ] Move the 48 `unsafe` blocks into `src/x11.rs` (the dlopen'd libX11 calls) and `src/sys.rs` (pidfd, inotify, poll, PSI).
- [ ] Add a CI job that attaches a release binary on tag push, and maybe an AUR `PKGBUILD`.

## Later

- [ ] Measure the real minimum Rust version (e.g. `cargo msrv` in CI) and set `rust-version` in `Cargo.toml`. It is at least 1.82 because of `is_none_or`.
- [ ] Split the binary VDF parser out of `src/non_steam.rs` (1,914 lines).
- [ ] If testers' Steam scopes land in nested slices, watch those too. The daemon only watches direct children of `app.slice`, and the `/proc` fallback still finds the game, just more slowly.
