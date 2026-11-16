use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::error::{VapourError, VapourResult};
use crate::library::home_dir;
use crate::non_steam::write_atomic;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalSettings {
    #[serde(default = "default_true")]
    pub overlay: bool,
    #[serde(default = "default_true")]
    pub zero_ram: bool,
    #[serde(default = "default_false")]
    pub nojoy: bool,
    #[serde(default = "default_false")]
    pub gamemode: bool,
    #[serde(default = "default_false")]
    pub mangohud: bool,
    #[serde(default = "default_false")]
    pub gamescope: bool,
    #[serde(default)]
    pub gamescope_args: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            overlay: true,
            zero_ram: true,
            nojoy: false,
            gamemode: false,
            mangohud: false,
            gamescope: false,
            gamescope_args: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameConfig {
    pub overlay: Option<bool>,
    pub extra_args: Vec<String>,
    pub wrapper: Option<String>,
    pub gamemode: Option<bool>,
    pub mangohud: Option<bool>,
    pub gamescope: Option<bool>,
    pub gamescope_args: Option<String>,
    pub env: Vec<(String, String)>,
    pub exe_path: Option<PathBuf>,
    pub start_dir: Option<PathBuf>,
    pub compat_tool: Option<String>,
    pub launch_options: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct GameConfigToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overlay: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_args: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrapper: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gamemode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mangohud: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gamescope: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gamescope_args: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_options: Option<String>,
}

impl From<GameConfigToml> for GameConfig {
    fn from(t: GameConfigToml) -> Self {
        let extra_args = t
            .extra_args
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();

        let mut env = Vec::new();
        if let Some(e) = t.env {
            for pair in e.split(';') {
                if let Some((k, v)) = pair.trim().split_once('=') {
                    env.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
        }

        Self {
            overlay: t.overlay,
            extra_args,
            wrapper: t.wrapper,
            gamemode: t.gamemode,
            mangohud: t.mangohud,
            gamescope: t.gamescope,
            gamescope_args: t.gamescope_args,
            env,
            exe_path: t.exe_path,
            start_dir: t.start_dir,
            compat_tool: t.compat_tool,
            launch_options: t.launch_options,
        }
    }
}

impl From<&GameConfig> for GameConfigToml {
    fn from(g: &GameConfig) -> Self {
        let extra_args = if g.extra_args.is_empty() {
            None
        } else {
            Some(g.extra_args.join(" "))
        };

        let env = if g.env.is_empty() {
            None
        } else {
            Some(
                g.env
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        };

        Self {
            overlay: g.overlay,
            extra_args,
            wrapper: g.wrapper.clone(),
            gamemode: g.gamemode,
            mangohud: g.mangohud,
            gamescope: g.gamescope,
            gamescope_args: g.gamescope_args.clone(),
            env,
            exe_path: g.exe_path.clone(),
            start_dir: g.start_dir.clone(),
            compat_tool: g.compat_tool.clone(),
            launch_options: g.launch_options.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedLaunchConfig {
    pub overlay: bool,
    pub keep_steam: bool,
    pub nojoy: bool,
    pub gamemode: bool,
    pub mangohud: bool,
    pub gamescope: bool,
    pub gamescope_args: Option<String>,
    pub wrapper: Option<String>,
    pub env: Vec<(String, String)>,
    pub extra_args: Vec<String>,
    /// Only the arguments typed on the command line (Steam applies the rest from shortcuts.vdf).
    pub cli_args: Vec<String>,
    pub compat_tool: Option<String>,
    pub localconfig_options: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonConfig {
    pub auto_tray_gui: bool,
    pub restore_gui_on_exit: bool,
    pub reclaim_bytes_mb: u64,
    pub swappiness: u32,
    pub psi_adaptive: bool,
    pub notifications: bool,
    pub shutdown_if_headless: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            auto_tray_gui: true,
            restore_gui_on_exit: true,
            reclaim_bytes_mb: 1500,
            swappiness: 200,
            psi_adaptive: true,
            notifications: true,
            shutdown_if_headless: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct VapourConfigToml {
    #[serde(default)]
    pub defaults: GlobalSettings,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub games: HashMap<String, GameConfigToml>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VapourConfig {
    pub defaults: GlobalSettings,
    pub daemon: DaemonConfig,
    pub games: HashMap<String, GameConfig>,
}

pub fn config_path() -> Option<PathBuf> {
    let home = home_dir()?;
    let dir = home.join(".config/vapour");
    let _ = fs::create_dir_all(&dir);
    Some(dir.join("settings.toml"))
}

impl VapourConfig {
    /// Lenient read-only load for runtime callers (daemon, watcher, prune): an unreadable or
    /// invalid file warns and yields defaults. Never saves, so the user's file is never overwritten.
    pub fn load() -> Self {
        Self::load_checked().unwrap_or_else(|e| {
            eprintln!("[WARN] {e}; using defaults. Fix settings.toml.");
            Self::default()
        })
    }

    /// Strict load for the CLI entry point: reports an unreadable/invalid file as an error.
    pub fn load_checked() -> VapourResult<Self> {
        let Some(path) = config_path() else {
            return Ok(Self::default());
        };

        match fs::read_to_string(&path) {
            Ok(contents) => Self::parse(&contents),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let default_cfg = Self::default();
                let _ = default_cfg.save();
                Ok(default_cfg)
            }
            Err(e) => Err(VapourError::Config(format!(
                "Cannot read {}: {e}",
                path.display()
            ))),
        }
    }

    pub fn save(&self) -> VapourResult<()> {
        let path = config_path()
            .ok_or_else(|| VapourError::Config("Cannot determine home directory".into()))?;
        let content = self.serialize();
        write_atomic(&path, content.as_bytes())
            .map_err(|e| VapourError::Config(format!("Failed to save config: {}", e)))
    }

    pub fn parse(input: &str) -> VapourResult<Self> {
        let toml_cfg: VapourConfigToml = toml::from_str(input)
            .map_err(|e| VapourError::Config(format!("Invalid settings.toml: {e}")))?;

        let mut games = HashMap::new();
        for (k, v) in toml_cfg.games {
            games.insert(k.to_lowercase(), GameConfig::from(v));
        }

        Ok(Self {
            defaults: toml_cfg.defaults,
            daemon: toml_cfg.daemon,
            games,
        })
    }

    pub fn serialize(&self) -> String {
        let mut toml_games = HashMap::new();
        for (k, v) in &self.games {
            toml_games.insert(k.clone(), GameConfigToml::from(v));
        }

        let toml_cfg = VapourConfigToml {
            defaults: self.defaults.clone(),
            daemon: self.daemon.clone(),
            games: toml_games,
        };

        let body = toml::to_string_pretty(&toml_cfg).unwrap_or_default();
        format!(
            "# Vapour User Settings & Game Configurations\n# Stored in ~/.config/vapour/settings.toml\n\n{}",
            body
        )
    }

    pub fn get_game_config(&self, id_or_name: &str) -> GameConfig {
        let key = id_or_name.trim().to_lowercase();
        if let Some(cfg) = self.games.get(&key) {
            return cfg.clone();
        }
        GameConfig::default()
    }

    /// Resolves whether overlay is enabled considering: CLI flag > Game override > Global default
    pub fn resolve_overlay(&self, id_or_name: &str, cli_flag: Option<bool>) -> bool {
        if let Some(cli) = cli_flag {
            return cli;
        }
        let g = self.get_game_config(id_or_name);
        if let Some(ov) = g.overlay {
            return ov;
        }
        self.defaults.overlay
    }

    /// Resolves whether to keep Steam running: CLI --keep-steam > Global zero_ram
    pub fn resolve_keep_steam(&self, cli_keep_steam: bool) -> bool {
        if cli_keep_steam {
            return true;
        }
        !self.defaults.zero_ram
    }

    /// Resolves the comprehensive launch configuration adhering to 4-tier precedence:
    /// Priority 1: Ad-hoc CLI arguments & flags
    /// Priority 2: Game-specific configuration in settings.toml
    /// Priority 3: Global defaults in settings.toml
    /// Priority 4: Native Steam client LaunchOptions in localconfig.vdf
    pub fn resolve_launch_config(
        &self,
        id_or_name: &str,
        appid: Option<u32>,
        cli_overlay: Option<bool>,
        cli_keep_steam: bool,
        cli_extra_args: &[String],
    ) -> ResolvedLaunchConfig {
        let game_cfg = self.get_game_config(id_or_name);
        let overlay = self.resolve_overlay(id_or_name, cli_overlay);
        let keep_steam = self.resolve_keep_steam(cli_keep_steam);

        let gamemode = game_cfg.gamemode.unwrap_or(self.defaults.gamemode);
        let mangohud = game_cfg.mangohud.unwrap_or(self.defaults.mangohud);
        let gamescope = game_cfg.gamescope.unwrap_or(self.defaults.gamescope);
        let gamescope_args = game_cfg
            .gamescope_args
            .clone()
            .or_else(|| self.defaults.gamescope_args.clone());
        let mut wrapper = game_cfg.wrapper.clone();
        let mut env = game_cfg.env.clone();

        let localconfig_options =
            appid.and_then(crate::library::get_steam_localconfig_launch_options);

        let mut extra_args = if !game_cfg.extra_args.is_empty() {
            game_cfg.extra_args.clone()
        } else if let Some(ref lo) = game_cfg.launch_options {
            let parsed = parse_launch_options(lo);
            for (k, v) in parsed.env {
                if !env.iter().any(|(ek, _)| *ek == k) {
                    env.push((k, v));
                }
            }
            if !parsed.wrapper.is_empty() {
                let w = parsed.wrapper.join(" ");
                wrapper = Some(match wrapper {
                    Some(existing) => format!("{w} {existing}"),
                    None => w,
                });
            }
            parsed.args
        } else {
            Vec::new()
        };
        extra_args.extend_from_slice(cli_extra_args);

        ResolvedLaunchConfig {
            overlay,
            keep_steam,
            nojoy: self.defaults.nojoy,
            gamemode,
            mangohud,
            gamescope,
            gamescope_args,
            wrapper,
            env,
            extra_args,
            cli_args: cli_extra_args.to_vec(),
            compat_tool: game_cfg.compat_tool,
            localconfig_options,
        }
    }

    /// Sets or updates the executable path and working directory for a game
    pub fn update_game_path(
        &mut self,
        id_or_name: &str,
        new_exe: PathBuf,
        new_start_dir: Option<PathBuf>,
    ) -> VapourResult<()> {
        let key = id_or_name.trim().to_lowercase();
        let mut cfg = self.get_game_config(&key);
        cfg.exe_path = Some(new_exe);
        if let Some(sd) = new_start_dir {
            cfg.start_dir = Some(sd);
        }
        self.games.insert(key, cfg);
        self.save()
    }

    /// Sets or updates the compatibility tool for a game
    pub fn set_game_compat_tool(
        &mut self,
        id_or_name: &str,
        tool: Option<String>,
    ) -> VapourResult<()> {
        let key = id_or_name.trim().to_lowercase();
        let mut cfg = self.get_game_config(&key);
        cfg.compat_tool = tool;
        self.games.insert(key, cfg);
        self.save()
    }

    /// Sets or updates the launch options for a game
    pub fn update_game_launch_options(
        &mut self,
        id_or_name: &str,
        raw_options: &str,
    ) -> VapourResult<String> {
        let formatted = self.apply_launch_options(id_or_name, raw_options);
        self.save()?;
        Ok(formatted)
    }

    /// In-memory part of `update_game_launch_options`. Clears `extra_args`, which would
    /// otherwise take precedence over `launch_options` at resolve time.
    pub(crate) fn apply_launch_options(&mut self, id_or_name: &str, raw_options: &str) -> String {
        let key = id_or_name.trim().to_lowercase();
        let formatted = format_launch_options(raw_options);
        let mut cfg = self.get_game_config(&key);
        cfg.extra_args.clear();
        cfg.launch_options = (!formatted.is_empty()).then(|| formatted.clone());
        self.games.insert(key, cfg);
        formatted
    }
}

/// Splits on whitespace, honoring `"..."` and `'...'` (quotes are stripped).
pub fn split_launch_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has_tok = false;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                has_tok = true;
            }
            None if c.is_whitespace() => {
                if has_tok {
                    out.push(std::mem::take(&mut cur));
                    has_tok = false;
                }
            }
            None => {
                cur.push(c);
                has_tok = true;
            }
        }
    }
    if has_tok {
        out.push(cur);
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedLaunchOptions {
    pub env: Vec<(String, String)>,
    pub wrapper: Vec<String>,
    pub args: Vec<String>,
}

fn split_env_assignment(tok: &str) -> Option<(String, String)> {
    let (k, v) = tok.split_once('=')?;
    let mut chars = k.chars();
    let first = chars.next()?;
    if (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        Some((k.to_string(), v.to_string()))
    } else {
        None
    }
}

/// Parses Steam-style launch options. With `%command%`, leading `KEY=VAL` tokens are env,
/// the rest before it a wrapper, and tokens after it game args; without it, all are args.
pub fn parse_launch_options(s: &str) -> ParsedLaunchOptions {
    let tokens = split_launch_tokens(s);
    let Some(pos) = tokens.iter().position(|t| t == "%command%") else {
        return ParsedLaunchOptions {
            args: tokens,
            ..Default::default()
        };
    };
    let mut parsed = ParsedLaunchOptions {
        args: tokens[pos + 1..].to_vec(),
        ..Default::default()
    };
    let mut leading = true;
    for tok in &tokens[..pos] {
        match leading.then(|| split_env_assignment(tok)).flatten() {
            Some(kv) => parsed.env.push(kv),
            None => {
                leading = false;
                parsed.wrapper.push(tok.clone());
            }
        }
    }
    parsed
}

/// Formats launch options smartly:
/// If the options contain an environment variable assignment (e.g. `VAR=val`)
/// and do NOT contain `%command%`, automatically append `%command%` because
/// Steam requires `%command%` to treat leading assignments as environment variables.
pub fn format_launch_options(opts: &str) -> String {
    let trimmed = opts.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains('=') && !trimmed.contains("%command%") {
        format!("{} %command%", trimmed)
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_and_serialize() {
        let input = r#"
[defaults]
overlay = false
zero_ram = true

[games.730]
overlay = false
extra_args = "-novid -freq 240"
wrapper = "gamemoderun gamescope --"
env = "MESA_VK_WSI_PRESENT_MODE=mailbox; DXVK_ASYNC=1"
"#;
        let cfg = VapourConfig::parse(input).unwrap();
        assert!(!cfg.defaults.overlay);
        assert!(cfg.defaults.zero_ram);

        let cs = cfg.get_game_config("730");
        assert_eq!(cs.overlay, Some(false));
        assert_eq!(cs.extra_args, vec!["-novid", "-freq", "240"]);
        assert_eq!(cs.wrapper, Some("gamemoderun gamescope --".to_string()));
        assert_eq!(
            cs.env,
            vec![
                (
                    "MESA_VK_WSI_PRESENT_MODE".to_string(),
                    "mailbox".to_string()
                ),
                ("DXVK_ASYNC".to_string(), "1".to_string())
            ]
        );

        let serialized = cfg.serialize();
        let reloaded = VapourConfig::parse(&serialized).unwrap();
        assert_eq!(cfg, reloaded);
    }

    #[test]
    fn test_precedence() {
        let mut cfg = VapourConfig::default();
        cfg.defaults.overlay = true;

        // CLI flag takes highest priority
        assert!(!cfg.resolve_overlay("730", Some(false)));
        assert!(cfg.resolve_overlay("730", Some(true)));

        // Game override takes second priority
        cfg.games.insert(
            "730".to_string(),
            GameConfig {
                overlay: Some(false),
                ..Default::default()
            },
        );
        assert!(!cfg.resolve_overlay("730", None));

        // Unknown game falls back to default
        assert!(cfg.resolve_overlay("550", None));
    }

    #[test]
    fn test_non_steam_fields() {
        let input = r#"
[games.2165744007]
compat_tool = "proton-cachyos-slr"
exe_path = "/home/user/Games/StandaloneGame.exe"
start_dir = "/home/user/Games"
launch_options = "WINEDLLOVERRIDES=\"winhttp=n,b\" %command%"
"#;
        let cfg = VapourConfig::parse(input).unwrap();
        let g = cfg.get_game_config("2165744007");
        assert_eq!(g.compat_tool, Some("proton-cachyos-slr".to_string()));
        assert_eq!(
            g.exe_path,
            Some(PathBuf::from("/home/user/Games/StandaloneGame.exe"))
        );
        assert_eq!(g.start_dir, Some(PathBuf::from("/home/user/Games")));
        assert_eq!(
            g.launch_options,
            Some("WINEDLLOVERRIDES=\"winhttp=n,b\" %command%".to_string())
        );
    }

    #[test]
    fn test_format_launch_options() {
        assert_eq!(
            format_launch_options("WINEDLLOVERRIDES=\"winhttp=n,b\""),
            "WINEDLLOVERRIDES=\"winhttp=n,b\" %command%"
        );
        assert_eq!(
            format_launch_options("WINEDLLOVERRIDES=\"winhttp=n,b\" %command%"),
            "WINEDLLOVERRIDES=\"winhttp=n,b\" %command%"
        );
        assert_eq!(
            format_launch_options("-novid -fullscreen"),
            "-novid -fullscreen"
        );
        assert_eq!(format_launch_options("   "), "");
    }

    #[test]
    fn test_resolve_launch_config_precedence() {
        let toml_str = r#"
[defaults]
gamemode = true
mangohud = false
gamescope = false

[games.730]
mangohud = true
extra_args = "-freq 240"
wrapper = "custom-wrapper"
"#;
        let cfg = VapourConfig::parse(toml_str).unwrap();
        let resolved = cfg.resolve_launch_config(
            "730",
            Some(730),
            Some(false), // CLI disables overlay
            true,        // CLI keep steam
            &["-novid".to_string()],
        );

        assert!(!resolved.overlay); // CLI override
        assert!(resolved.keep_steam); // CLI override
        assert!(resolved.gamemode); // From defaults
        assert!(resolved.mangohud); // From game override
        assert_eq!(resolved.wrapper, Some("custom-wrapper".to_string()));
        assert_eq!(resolved.extra_args, vec!["-freq", "240", "-novid"]);
    }

    #[test]
    fn parse_should_return_err_when_toml_invalid() {
        assert!(VapourConfig::parse("[[[").is_err());
    }

    #[test]
    fn parse_should_keep_games_when_toml_valid() {
        let cfg = VapourConfig::parse("[games.730]\noverlay = false\n").unwrap();
        let back = VapourConfig::parse(&cfg.serialize()).unwrap();
        assert_eq!(back.get_game_config("730").overlay, Some(false));
    }

    #[test]
    fn parse_launch_options_should_extract_env_before_command() {
        let p = parse_launch_options("WINEDLLOVERRIDES=\"winhttp=n,b\" %command%");
        assert_eq!(
            p.env,
            vec![("WINEDLLOVERRIDES".to_string(), "winhttp=n,b".to_string())]
        );
        assert!(p.wrapper.is_empty() && p.args.is_empty());
    }

    #[test]
    fn parse_launch_options_should_split_wrapper_and_args() {
        let p = parse_launch_options("gamemoderun %command% -novid");
        assert_eq!(p.wrapper, vec!["gamemoderun"]);
        assert_eq!(p.args, vec!["-novid"]);
    }

    #[test]
    fn parse_launch_options_should_treat_all_as_args_without_command() {
        let p = parse_launch_options("-novid -freq 240");
        assert_eq!(p.args, vec!["-novid", "-freq", "240"]);
        assert!(p.env.is_empty() && p.wrapper.is_empty());
    }

    #[test]
    fn resolve_launch_config_should_not_pass_percent_command_as_arg() {
        let cfg = VapourConfig::parse(
            "[games.730]\nlaunch_options = \"FOO=bar gamemoderun %command% -novid\"\n",
        )
        .unwrap();
        let r = cfg.resolve_launch_config("730", None, None, false, &[]);
        assert_eq!(r.extra_args, vec!["-novid"]);
        assert_eq!(r.wrapper.as_deref(), Some("gamemoderun"));
        assert_eq!(r.env, vec![("FOO".to_string(), "bar".to_string())]);
    }

    #[test]
    fn apply_launch_options_should_override_old_extra_args() {
        let mut cfg = VapourConfig::default();
        let g = GameConfig {
            extra_args: vec!["-old".into()],
            ..Default::default()
        };
        cfg.games.insert("1".into(), g);
        cfg.apply_launch_options("1", "-new");
        let r = cfg.resolve_launch_config("1", None, None, false, &[]);
        assert_eq!(r.extra_args, vec!["-new"]);
    }
}
