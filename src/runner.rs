use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::ResolvedLaunchConfig;
use crate::error::{VapourError, VapourResult};
use crate::launcher::{ensure_steam_daemon, is_steam_running, resolve_steam_bin};
use crate::library::{find_steam_root, home_dir};
use crate::non_steam::{find_available_protons, get_compat_data_path_for_appid, NonSteamGame};

/// Target for game execution across official Steam games, shortcuts, and direct binaries.
#[derive(Debug, Clone)]
pub enum LaunchTarget {
    SteamApp {
        appid: u32,
        name: String,
    },
    SteamShortcut {
        appid: u32,
        name: String,
        long_appid: u64,
    },
    DirectNonSteam {
        game: NonSteamGame,
    },
}

impl LaunchTarget {
    pub fn appid(&self) -> u32 {
        match self {
            Self::SteamApp { appid, .. } => *appid,
            Self::SteamShortcut { appid, .. } => *appid,
            Self::DirectNonSteam { game } => game.appid,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::SteamApp { name, .. } => name,
            Self::SteamShortcut { name, .. } => name,
            Self::DirectNonSteam { game } => &game.name,
        }
    }
}

/// Result returned from executing or dry-running a game launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchResult {
    pub appid: u32,
    pub started_steam: bool,
    pub pid: Option<u32>,
}

/// Composes wrapper tokens around an executable and its argument list.
/// Supports Steam's %command% expansion or standard wrapper chaining:
/// [wrapper tokens from launch options] -> <executable> <args>
pub fn compose_command_tokens(
    base_bin: &Path,
    game_args: &[String],
    config: &ResolvedLaunchConfig,
) -> (String, Vec<String>) {
    let bin_str = base_bin.to_string_lossy().to_string();

    // 1. Check if config.wrapper contains %command%
    if let Some(ref w) = config.wrapper {
        if w.contains("%command%") {
            let mut parts: Vec<String> = w.split_whitespace().map(String::from).collect();
            if let Some(pos) = parts.iter().position(|p| p == "%command%") {
                parts[pos] = bin_str;
                parts.extend_from_slice(game_args);
                let exe = parts[0].clone();
                let args = parts[1..].to_vec();
                return (exe, args);
            }
        }
    }

    // 2. Standard wrapper prefix composition
    let mut prefix: Vec<String> = Vec::new();

    if let Some(ref w) = config.wrapper {
        for token in w.split_whitespace() {
            prefix.push(token.to_string());
        }
    }

    prefix.push(bin_str);
    prefix.extend_from_slice(game_args);

    let exe = prefix[0].clone();
    let args = prefix[1..].to_vec();
    (exe, args)
}

/// Dry-run lines for the memory plan, built from the real config instead of fixed text.
fn print_memory_plan(appid: u32) {
    let d = crate::config::VapourConfig::load_checked()
        .map(|c| c.daemon)
        .unwrap_or_default();
    println!("          Reclaim Targets:   vapour-steam-*.scope, app-steam-unknown-*.scope");
    println!("                             (cgroups holding game wrappers such as gamescope are skipped)");
    println!("          Protected Scope:   app-steam-app{appid}-*.scope (game, never written)");
    println!(
        "          Memory Policy:     memory.reclaim {}M swappiness={} @ T+12s, T+40s, PSI-triggered",
        d.reclaim_bytes_mb, d.swappiness
    );
}

/// Core abstraction for game runners.
pub trait GameRunner {
    fn name(&self) -> &'static str;

    fn build_command(
        &self,
        target: &LaunchTarget,
        config: &ResolvedLaunchConfig,
    ) -> VapourResult<Command>;

    fn launch(
        &self,
        target: &LaunchTarget,
        config: &ResolvedLaunchConfig,
        dry_run: bool,
    ) -> VapourResult<LaunchResult>;
}

/// A running Steam spawns the game itself, so Vapour's env/wrapper cannot reach it.
fn warn_if_env_wrapper_ignored(config: &ResolvedLaunchConfig, steam_running: bool) {
    if steam_running && (!config.env.is_empty() || config.wrapper.is_some()) {
        eprintln!("[WARN] Steam is already running: env/wrapper from launch options are ignored; set them in Steam's own launch options.");
    }
}

/// Runner for official Steam games dispatched through Steam's IPC daemon (-applaunch).
pub struct SteamDaemonRunner;

impl GameRunner for SteamDaemonRunner {
    fn name(&self) -> &'static str {
        "SteamDaemonRunner"
    }

    fn build_command(
        &self,
        target: &LaunchTarget,
        config: &ResolvedLaunchConfig,
    ) -> VapourResult<Command> {
        let appid = match target {
            LaunchTarget::SteamApp { appid, .. } => *appid,
            _ => {
                return Err(VapourError::Launch(
                    "SteamDaemonRunner only supports LaunchTarget::SteamApp".into(),
                ))
            }
        };

        let steam_bin = resolve_steam_bin();
        let mut cmd = Command::new(&steam_bin);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if !config.overlay {
            cmd.env("PRESSURE_VESSEL_REMOVE_GAME_OVERLAY", "1")
                .env("SteamNoOverlayUIDrawing", "1")
                .env("DISABLE_VK_LAYER_VALVE_steam_overlay_1", "1");
        }

        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        cmd.arg("-silent").arg("-applaunch").arg(appid.to_string());

        for a in &config.extra_args {
            cmd.arg(a);
        }

        Ok(cmd)
    }

    fn launch(
        &self,
        target: &LaunchTarget,
        config: &ResolvedLaunchConfig,
        dry_run: bool,
    ) -> VapourResult<LaunchResult> {
        let appid = match target {
            LaunchTarget::SteamApp { appid, .. } => *appid,
            _ => {
                return Err(VapourError::Launch(
                    "SteamDaemonRunner only supports LaunchTarget::SteamApp".into(),
                ))
            }
        };

        let was_running = is_steam_running();
        let steam_bin = resolve_steam_bin();
        warn_if_env_wrapper_ignored(config, was_running);

        if dry_run {
            println!("\x1b[33m[DRY-RUN]\x1b[0m Execution Plan:");
            println!("          Runner:            {}", self.name());
            println!(
                "          Target Game:       {} (AppID: {})",
                target.name(),
                target.appid()
            );
            println!(
                "          Dispatch Command:  {} -silent -applaunch {} {}",
                steam_bin.display(),
                appid,
                config.extra_args.join(" ")
            );
            println!("          Steam Pre-existing: {}", was_running);
            print_memory_plan(appid);
            if config.overlay {
                println!("          Overlay:           ENABLED (Available in-game)");
            } else {
                println!("          Overlay:           SUPPRESSED");
            }
            if let Some(ref w) = config.wrapper {
                println!("          Wrapper:           {}", w);
            }
            if let Some(ref lo) = config.localconfig_options {
                println!("          Localconfig Opts:  \"{}\"", lo);
            }
            if !config.env.is_empty() {
                let env_str = config
                    .env
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("          Custom Env:        {}", env_str);
            }
            let _ = ensure_steam_daemon(config.overlay, config.nojoy, &config.env, true)?;
            return Ok(LaunchResult {
                appid,
                started_steam: !was_running,
                pid: None,
            });
        }

        if config.overlay {
            println!("\x1b[36m[OVERLAY]\x1b[0m Steam In-Game Overlay: ENABLED (Available in-game)");
        } else {
            println!("\x1b[33m[OVERLAY]\x1b[0m Steam In-Game Overlay: SUPPRESSED");
        }

        let started_steam = ensure_steam_daemon(config.overlay, config.nojoy, &config.env, false)?;

        println!(
            "\x1b[1;36m[LAUNCH]\x1b[0m Dispatching '{}' (AppID: {}) via {}...",
            target.name(),
            target.appid(),
            self.name()
        );

        let mut launch_cmd = self.build_command(target, config)?;
        launch_cmd.spawn().map_err(|e| {
            VapourError::Launch(format!("Failed to dispatch game launch to Steam: {}", e))
        })?;

        Ok(LaunchResult {
            appid,
            started_steam,
            pid: None,
        })
    }
}

/// Runner for non-Steam shortcuts registered in Steam client (-gameidlaunch).
pub struct SteamShortcutRunner;

impl GameRunner for SteamShortcutRunner {
    fn name(&self) -> &'static str {
        "SteamShortcutRunner"
    }

    fn build_command(
        &self,
        target: &LaunchTarget,
        config: &ResolvedLaunchConfig,
    ) -> VapourResult<Command> {
        let (appid_32, long_appid) = match target {
            LaunchTarget::SteamShortcut {
                appid, long_appid, ..
            } => (*appid, *long_appid),
            _ => {
                return Err(VapourError::Launch(
                    "SteamShortcutRunner only supports LaunchTarget::SteamShortcut".into(),
                ))
            }
        };

        let steam_bin = resolve_steam_bin();
        let mut cmd = Command::new(&steam_bin);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if !config.overlay {
            cmd.env("PRESSURE_VESSEL_REMOVE_GAME_OVERLAY", "1")
                .env("SteamNoOverlayUIDrawing", "1")
                .env("DISABLE_VK_LAYER_VALVE_steam_overlay_1", "1");
        }

        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        cmd.arg("-silent")
            .arg("-gameidlaunch")
            .arg(long_appid.to_string());

        for a in &config.cli_args {
            cmd.arg(a);
        }

        let _ = appid_32; // Used for reference
        Ok(cmd)
    }

    fn launch(
        &self,
        target: &LaunchTarget,
        config: &ResolvedLaunchConfig,
        dry_run: bool,
    ) -> VapourResult<LaunchResult> {
        let (appid_32, long_appid) = match target {
            LaunchTarget::SteamShortcut {
                appid, long_appid, ..
            } => (*appid, *long_appid),
            _ => {
                return Err(VapourError::Launch(
                    "SteamShortcutRunner only supports LaunchTarget::SteamShortcut".into(),
                ))
            }
        };

        let was_running = is_steam_running();
        let steam_bin = resolve_steam_bin();
        warn_if_env_wrapper_ignored(config, was_running);

        if dry_run {
            println!("\x1b[33m[DRY-RUN]\x1b[0m Execution Plan:");
            println!("          Runner:            {}", self.name());
            println!(
                "          Target Game:       {} (AppID: {})",
                target.name(),
                target.appid()
            );
            println!(
                "          Dispatch Command:  {} -silent -gameidlaunch {} (Shortcut 32-bit AppID: {}) {}",
                steam_bin.display(),
                long_appid,
                appid_32,
                config.cli_args.join(" ")
            );
            println!("          Steam Pre-existing: {}", was_running);
            print_memory_plan(appid_32);
            if config.overlay {
                println!("          Overlay:           ENABLED (Available in shortcut)");
            } else {
                println!("          Overlay:           SUPPRESSED");
            }
            if let Some(ref w) = config.wrapper {
                println!("          Wrapper:           {}", w);
            }
            if !config.env.is_empty() {
                let env_str = config
                    .env
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("          Custom Env:        {}", env_str);
            }
            let _ = ensure_steam_daemon(config.overlay, config.nojoy, &config.env, true)?;
            return Ok(LaunchResult {
                appid: appid_32,
                started_steam: !was_running,
                pid: None,
            });
        }

        if config.overlay {
            println!("\x1b[36m[OVERLAY]\x1b[0m Steam In-Game Overlay: ENABLED (Available in-game)");
        } else {
            println!("\x1b[33m[OVERLAY]\x1b[0m Steam In-Game Overlay: SUPPRESSED");
        }

        let started_steam = ensure_steam_daemon(config.overlay, config.nojoy, &config.env, false)?;

        println!(
            "\x1b[1;36m[LAUNCH]\x1b[0m Dispatching '{}' (AppID: {}) via {}...",
            target.name(),
            target.appid(),
            self.name()
        );

        let mut launch_cmd = self.build_command(target, config)?;
        launch_cmd.spawn().map_err(|e| {
            VapourError::Launch(format!(
                "Failed to dispatch shortcut launch to Steam: {}",
                e
            ))
        })?;

        Ok(LaunchResult {
            appid: appid_32,
            started_steam,
            pid: None,
        })
    }
}

/// Runner for direct standalone execution of Windows games via Proton without Steam daemon.
pub struct ProtonDirectRunner;

impl GameRunner for ProtonDirectRunner {
    fn name(&self) -> &'static str {
        "ProtonDirectRunner"
    }

    fn build_command(
        &self,
        target: &LaunchTarget,
        config: &ResolvedLaunchConfig,
    ) -> VapourResult<Command> {
        let game = match target {
            LaunchTarget::DirectNonSteam { game } => game,
            _ => {
                return Err(VapourError::Launch(
                    "ProtonDirectRunner only supports LaunchTarget::DirectNonSteam".into(),
                ))
            }
        };

        if !game.exe.exists() {
            return Err(VapourError::Launch(format!(
                "Game executable not found at '{}'. If the game was moved, update its path.",
                game.exe.display()
            )));
        }

        let tools = find_available_protons();
        if tools.is_empty() {
            return Err(VapourError::Launch("No Proton installation found in Steam libraries or compatibilitytools.d. Please install Proton or GE-Proton.".into()));
        }

        let req_tool = config.compat_tool.as_ref().or(game.compat_tool.as_ref());
        let chosen = if let Some(req) = req_tool {
            if let Some(found) = tools.iter().find(|t| {
                t.name.eq_ignore_ascii_case(req)
                    || t.name.to_lowercase().contains(&req.to_lowercase())
            }) {
                found
            } else {
                eprintln!(
                    "\x1b[33m[WARN]\x1b[0m Configured Proton version '{}' not found. Falling back to '{}'.",
                    req, tools[0].name
                );
                &tools[0]
            }
        } else {
            &tools[0]
        };

        let steam_root = find_steam_root().unwrap_or_else(|| {
            home_dir()
                .map(|h| h.join(".local/share/Steam"))
                .unwrap_or_else(|| PathBuf::from("."))
        });

        let prefix_dir = get_compat_data_path_for_appid(game.appid);
        let _ = fs::create_dir_all(&prefix_dir);

        let appid_str = game.appid.to_string();

        let mut base_args = vec!["run".to_string(), game.exe.to_string_lossy().to_string()];
        base_args.extend(game.args.clone());
        base_args.extend(config.extra_args.clone());

        let (exe, args) = compose_command_tokens(&chosen.proton_bin, &base_args, config);
        let mut cmd = Command::new(exe);
        cmd.args(args);

        cmd.env("STEAM_COMPAT_CLIENT_INSTALL_PATH", &steam_root);
        cmd.env("STEAM_COMPAT_DATA_PATH", &prefix_dir);
        cmd.env("SteamAppId", &appid_str);
        cmd.env("STEAM_COMPAT_APP_ID", &appid_str);
        cmd.env("SteamGameId", &appid_str);
        if let Some(tool_parent) = chosen.proton_bin.parent() {
            cmd.env("STEAM_COMPAT_TOOL_PATHS", tool_parent);
        }
        if chosen.is_wow64() {
            cmd.env("PROTON_ENABLE_WOW64", "1");
        }

        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        if let Some(ref dir) = game.start_dir {
            if dir.exists() {
                cmd.current_dir(dir);
            } else if let Some(parent) = game.exe.parent() {
                cmd.current_dir(parent);
            }
        } else if let Some(parent) = game.exe.parent() {
            cmd.current_dir(parent);
        }

        Ok(cmd)
    }

    fn launch(
        &self,
        target: &LaunchTarget,
        config: &ResolvedLaunchConfig,
        dry_run: bool,
    ) -> VapourResult<LaunchResult> {
        let game = match target {
            LaunchTarget::DirectNonSteam { game } => game,
            _ => {
                return Err(VapourError::Launch(
                    "ProtonDirectRunner only supports LaunchTarget::DirectNonSteam".into(),
                ))
            }
        };

        let mut cmd = self.build_command(target, config)?;

        if dry_run {
            println!("\x1b[33m[DRY-RUN]\x1b[0m Runner: {}", self.name());
            println!(
                "          Game: {} (AppID: {})",
                target.name(),
                target.appid()
            );
            println!("          Executable: {:?}", cmd);
            println!("          AppID: {}", game.appid);
            let prefix_dir = get_compat_data_path_for_appid(game.appid);
            println!("          Prefix (compatdata): {}", prefix_dir.display());
            if let Some(ref w) = config.wrapper {
                println!("          Wrapper:           {}", w);
            }
            return Ok(LaunchResult {
                appid: game.appid,
                started_steam: false,
                pid: None,
            });
        }

        println!(
            "\x1b[1;36m[LAUNCH]\x1b[0m Starting non-Steam game '{}' (AppID: {}) via {}...",
            target.name(),
            target.appid(),
            self.name()
        );
        let child = cmd.spawn().map_err(|e| {
            VapourError::Launch(format!("Failed to spawn Proton child process: {}", e))
        })?;

        Ok(LaunchResult {
            appid: game.appid,
            started_steam: false,
            pid: Some(child.id()),
        })
    }
}

/// Runner for direct standalone execution of native Linux ELF executables.
pub struct NativeDirectRunner;

impl GameRunner for NativeDirectRunner {
    fn name(&self) -> &'static str {
        "NativeDirectRunner"
    }

    fn build_command(
        &self,
        target: &LaunchTarget,
        config: &ResolvedLaunchConfig,
    ) -> VapourResult<Command> {
        let game = match target {
            LaunchTarget::DirectNonSteam { game } => game,
            _ => {
                return Err(VapourError::Launch(
                    "NativeDirectRunner only supports LaunchTarget::DirectNonSteam".into(),
                ))
            }
        };

        if !game.exe.exists() {
            return Err(VapourError::Launch(format!(
                "Game executable not found at '{}'. If the game was moved, update its path.",
                game.exe.display()
            )));
        }

        let mut all_args = game.args.clone();
        all_args.extend(config.extra_args.clone());

        let (exe, args) = compose_command_tokens(&game.exe, &all_args, config);
        let mut cmd = Command::new(exe);
        cmd.args(args);

        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        if let Some(ref dir) = game.start_dir {
            if dir.exists() {
                cmd.current_dir(dir);
            } else if let Some(parent) = game.exe.parent() {
                cmd.current_dir(parent);
            }
        } else if let Some(parent) = game.exe.parent() {
            cmd.current_dir(parent);
        }

        Ok(cmd)
    }

    fn launch(
        &self,
        target: &LaunchTarget,
        config: &ResolvedLaunchConfig,
        dry_run: bool,
    ) -> VapourResult<LaunchResult> {
        let game = match target {
            LaunchTarget::DirectNonSteam { game } => game,
            _ => {
                return Err(VapourError::Launch(
                    "NativeDirectRunner only supports LaunchTarget::DirectNonSteam".into(),
                ))
            }
        };

        let mut cmd = self.build_command(target, config)?;

        if dry_run {
            println!("\x1b[33m[DRY-RUN]\x1b[0m Runner: {}", self.name());
            println!(
                "          Game: {} (AppID: {})",
                target.name(),
                target.appid()
            );
            println!("          Executable: {:?}", cmd);
            println!("          AppID: {}", game.appid);
            if let Some(ref w) = config.wrapper {
                println!("          Wrapper:           {}", w);
            }
            return Ok(LaunchResult {
                appid: game.appid,
                started_steam: false,
                pid: None,
            });
        }

        println!(
            "\x1b[1;36m[LAUNCH]\x1b[0m Starting native Linux game '{}' (AppID: {}) via {}...",
            target.name(),
            target.appid(),
            self.name()
        );
        let child = cmd.spawn().map_err(|e| {
            VapourError::Launch(format!("Failed to spawn native game process: {}", e))
        })?;

        Ok(LaunchResult {
            appid: game.appid,
            started_steam: false,
            pid: Some(child.id()),
        })
    }
}

/// Resolves the concrete runner instance for a given launch target.
pub fn get_runner_for_target(target: &LaunchTarget, direct_mode: bool) -> Box<dyn GameRunner> {
    match target {
        LaunchTarget::SteamApp { .. } => Box::new(SteamDaemonRunner),
        LaunchTarget::SteamShortcut { .. } => {
            if direct_mode {
                Box::new(ProtonDirectRunner)
            } else {
                Box::new(SteamShortcutRunner)
            }
        }
        LaunchTarget::DirectNonSteam { game } => {
            if game.use_proton {
                Box::new(ProtonDirectRunner)
            } else {
                Box::new(NativeDirectRunner)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GameConfig, VapourConfig};
    use crate::launcher::calculate_long_appid;

    #[test]
    fn test_compose_command_tokens_no_wrappers() {
        let cfg = ResolvedLaunchConfig {
            overlay: true,
            keep_steam: false,
            nojoy: false,
            wrapper: None,
            env: Vec::new(),
            extra_args: Vec::new(),
            cli_args: Vec::new(),
            compat_tool: None,
            localconfig_options: None,
        };

        let (bin, args) =
            compose_command_tokens(Path::new("/usr/bin/game"), &["-novid".to_string()], &cfg);
        assert_eq!(bin, "/usr/bin/game");
        assert_eq!(args, vec!["-novid"]);
    }

    #[test]
    fn test_compose_command_tokens_wrapper_prefix() {
        let cfg = ResolvedLaunchConfig {
            overlay: true,
            keep_steam: false,
            nojoy: false,
            wrapper: Some("gamemoderun gamescope -W 1920 -H 1080 --".to_string()),
            env: Vec::new(),
            extra_args: Vec::new(),
            cli_args: Vec::new(),
            compat_tool: None,
            localconfig_options: None,
        };

        let (bin, args) =
            compose_command_tokens(Path::new("/usr/bin/game"), &["-novid".to_string()], &cfg);
        assert_eq!(bin, "gamemoderun");
        assert_eq!(
            args,
            vec![
                "gamescope",
                "-W",
                "1920",
                "-H",
                "1080",
                "--",
                "/usr/bin/game",
                "-novid"
            ]
        );
    }

    #[test]
    fn test_compose_command_tokens_percent_command() {
        let cfg = ResolvedLaunchConfig {
            overlay: true,
            keep_steam: false,
            nojoy: false,
            wrapper: Some("gamemoderun gamescope -W 1920 -- %command% -fullscreen".to_string()),
            env: Vec::new(),
            extra_args: Vec::new(),
            cli_args: Vec::new(),
            compat_tool: None,
            localconfig_options: None,
        };

        let (bin, args) =
            compose_command_tokens(Path::new("/usr/bin/game"), &["-novid".to_string()], &cfg);
        assert_eq!(bin, "gamemoderun");
        assert_eq!(
            args,
            vec![
                "gamescope",
                "-W",
                "1920",
                "--",
                "/usr/bin/game",
                "-fullscreen",
                "-novid"
            ]
        );
    }

    #[test]
    fn test_steam_daemon_runner_command_generation() {
        let target = LaunchTarget::SteamApp {
            appid: 730,
            name: "Counter-Strike 2".to_string(),
        };
        let cfg = ResolvedLaunchConfig {
            overlay: false,
            keep_steam: false,
            nojoy: false,
            wrapper: None,
            env: vec![("CUSTOM_VAR".to_string(), "1".to_string())],
            extra_args: vec!["-novid".to_string()],
            cli_args: Vec::new(),
            compat_tool: None,
            localconfig_options: None,
        };

        let runner = SteamDaemonRunner;
        let outcome = runner.launch(&target, &cfg, true);
        assert!(outcome.is_ok());
        let res = outcome.unwrap();
        assert_eq!(res.appid, 730);
    }

    #[test]
    fn test_steam_shortcut_runner_command_generation() {
        let target = LaunchTarget::SteamShortcut {
            appid: 2165744007,
            name: "Custom Game".to_string(),
            long_appid: calculate_long_appid(2165744007),
        };
        let cfg = ResolvedLaunchConfig {
            overlay: true,
            keep_steam: false,
            nojoy: false,
            wrapper: None,
            env: Vec::new(),
            extra_args: Vec::new(),
            cli_args: Vec::new(),
            compat_tool: None,
            localconfig_options: None,
        };

        let runner = SteamShortcutRunner;
        let outcome = runner.launch(&target, &cfg, true);
        assert!(outcome.is_ok());
        let res = outcome.unwrap();
        assert_eq!(res.appid, 2165744007);
    }

    #[test]
    fn test_launch_target_accessors() {
        let app = LaunchTarget::SteamApp {
            appid: 730,
            name: "Counter-Strike 2".to_string(),
        };
        assert_eq!(app.appid(), 730);
        assert_eq!(app.name(), "Counter-Strike 2");

        let shortcut = LaunchTarget::SteamShortcut {
            appid: 2165744007,
            name: "Custom Game".to_string(),
            long_appid: 12345,
        };
        assert_eq!(shortcut.appid(), 2165744007);
        assert_eq!(shortcut.name(), "Custom Game");

        let non_steam = LaunchTarget::DirectNonSteam {
            game: NonSteamGame {
                name: "Direct Linux Game".to_string(),
                exe: PathBuf::from("/bin/sh"),
                start_dir: None,
                args: Vec::new(),
                compat_tool: None,
                use_proton: false,
                appid: 999999,
                launch_options: String::new(),
                from_steam_shortcut: false,
                is_path_missing: false,
            },
        };
        assert_eq!(non_steam.appid(), 999999);
        assert_eq!(non_steam.name(), "Direct Linux Game");
    }

    #[test]
    fn test_get_runner_for_target_dispatch() {
        let app = LaunchTarget::SteamApp {
            appid: 730,
            name: "CS2".to_string(),
        };
        assert_eq!(
            get_runner_for_target(&app, false).name(),
            "SteamDaemonRunner"
        );

        let shortcut = LaunchTarget::SteamShortcut {
            appid: 123,
            name: "SC".to_string(),
            long_appid: 456,
        };
        assert_eq!(
            get_runner_for_target(&shortcut, false).name(),
            "SteamShortcutRunner"
        );
        assert_eq!(
            get_runner_for_target(&shortcut, true).name(),
            "ProtonDirectRunner"
        );

        let proton_game = LaunchTarget::DirectNonSteam {
            game: NonSteamGame {
                name: "Proton Game".to_string(),
                exe: PathBuf::from("/bin/sh"),
                start_dir: None,
                args: Vec::new(),
                compat_tool: None,
                use_proton: true,
                appid: 101,
                launch_options: String::new(),
                from_steam_shortcut: false,
                is_path_missing: false,
            },
        };
        assert_eq!(
            get_runner_for_target(&proton_game, false).name(),
            "ProtonDirectRunner"
        );

        let native_game = LaunchTarget::DirectNonSteam {
            game: NonSteamGame {
                name: "Native Game".to_string(),
                exe: PathBuf::from("/bin/sh"),
                start_dir: None,
                args: Vec::new(),
                compat_tool: None,
                use_proton: false,
                appid: 102,
                launch_options: String::new(),
                from_steam_shortcut: false,
                is_path_missing: false,
            },
        };
        assert_eq!(
            get_runner_for_target(&native_game, false).name(),
            "NativeDirectRunner"
        );
    }

    #[test]
    fn test_native_direct_runner_execution() {
        let target = LaunchTarget::DirectNonSteam {
            game: NonSteamGame {
                name: "Native Test Game".to_string(),
                exe: PathBuf::from("/bin/sh"),
                start_dir: None,
                args: vec!["-c".to_string(), "true".to_string()],
                compat_tool: None,
                use_proton: false,
                appid: 8888,
                launch_options: String::new(),
                from_steam_shortcut: false,
                is_path_missing: false,
            },
        };
        let cfg = ResolvedLaunchConfig {
            overlay: false,
            keep_steam: false,
            nojoy: false,
            wrapper: None,
            env: Vec::new(),
            extra_args: Vec::new(),
            cli_args: Vec::new(),
            compat_tool: None,
            localconfig_options: None,
        };

        let runner = NativeDirectRunner;
        let dry_res = runner.launch(&target, &cfg, true);
        assert!(dry_res.is_ok());
        assert_eq!(dry_res.unwrap().appid, 8888);

        let live_res = runner.launch(&target, &cfg, false);
        assert!(live_res.is_ok());
        let res = live_res.unwrap();
        assert_eq!(res.appid, 8888);
        assert!(res.pid.is_some());
    }

    fn nonsteam_game(appid: u32) -> NonSteamGame {
        NonSteamGame {
            name: "E2E".into(),
            exe: PathBuf::from("/bin/true"),
            start_dir: None,
            compat_tool: None,
            use_proton: false,
            appid,
            launch_options: String::new(),
            args: Vec::new(), // the loader no longer copies settings args here
            from_steam_shortcut: true,
            is_path_missing: false,
        }
    }

    /// Settings are saved under the app ID; the launch path must resolve with that same key and
    /// the options must reach the command exactly once.
    #[test]
    fn launch_options_should_reach_direct_command_once_when_resolved_by_appid() {
        let appid = 2165744007u32;
        let mut cfg = VapourConfig::default();
        cfg.games.insert(
            appid.to_string(),
            GameConfig {
                extra_args: vec!["-old".into()],
                ..Default::default()
            },
        );
        cfg.apply_launch_options(&appid.to_string(), "FOO=bar %command% -novid");

        let resolved = cfg.resolve_launch_config(&appid.to_string(), Some(appid), None, false, &[]);
        let target = LaunchTarget::DirectNonSteam {
            game: nonsteam_game(appid),
        };
        let cmd = NativeDirectRunner
            .build_command(&target, &resolved)
            .unwrap();
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args.iter().filter(|a| *a == "-novid").count(), 1);
        assert!(!args.iter().any(|a| a == "-old" || a == "%command%"));
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "FOO" && v.is_some_and(|v| v == "bar")));
    }

    #[test]
    fn shortcut_runner_should_pass_only_cli_args_when_config_has_launch_options() {
        let appid = 2165744007u32;
        let mut cfg = VapourConfig::default();
        cfg.apply_launch_options(&appid.to_string(), "-novid");
        let resolved = cfg.resolve_launch_config(
            &appid.to_string(),
            Some(appid),
            None,
            false,
            &["-cli".to_string()],
        );
        assert_eq!(resolved.extra_args, vec!["-novid", "-cli"]);
        let target = LaunchTarget::SteamShortcut {
            appid,
            name: "E2E".into(),
            long_appid: calculate_long_appid(appid),
        };
        let cmd = SteamShortcutRunner
            .build_command(&target, &resolved)
            .unwrap();
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(args.contains(&"-cli".to_string()));
        assert!(!args.contains(&"-novid".to_string()));
    }
}
