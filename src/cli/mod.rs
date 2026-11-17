pub mod launch;
pub mod list;
pub mod manage;
pub mod system;
pub mod tui;

use std::fmt;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use serde::Serialize;

use crate::config::VapourConfig;
use crate::error::{VapourError, VapourResult};
use crate::library::Game;
use crate::mount::{add_custom_library, remove_custom_library};
use crate::non_steam::NonSteamGame;

use launch::execute_game_launch;
use list::{list_all_games, list_libraries};
use manage::{
    handle_add_game, handle_compat, handle_config_get, handle_remove_game, handle_set_command,
    handle_set_launch_options, handle_set_path, interactive_settings,
};
use system::{
    check_status, handle_daemon, handle_desktop_entries, handle_doctor, handle_info, handle_prune,
    handle_shutdown,
};
use tui::run_interactive_selector;

#[derive(Parser, Debug)]
#[command(
    name = "vapour",
    version = env!("CARGO_PKG_VERSION"),
    about = "Lightweight Native Linux Steam Launcher & Lifecycle Controller",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Game name or AppID to launch directly
    pub query: Option<String>,

    /// Sort order for game listings or interactive selection (recent, size, name)
    #[arg(long = "sort", value_enum, global = true)]
    pub sort: Option<crate::library::SortOrder>,

    /// Additional launch flags passed directly to the game (e.g. -novid)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub game_args: Vec<String>,

    // Global Execution / Format Flags:
    /// Preview commands without starting processes
    #[arg(short = 'd', long = "dry-run", global = true)]
    pub dry_run: bool,

    /// Suppress Steam In-Game Overlay
    #[arg(long = "no-overlay", global = true)]
    pub no_overlay: bool,

    /// Force Steam In-Game Overlay to be enabled
    #[arg(long = "overlay", global = true)]
    pub overlay: bool,

    /// Launch non-Steam game directly with Proton/Wine without starting Steam
    #[arg(long = "direct", global = true)]
    pub direct: bool,

    /// Keep Steam running in the background after the game exits
    #[arg(long = "keep-steam", global = true)]
    pub keep_steam: bool,

    /// Output machine-readable JSON
    #[arg(long = "json", global = true)]
    pub json: bool,

    /// Output single-line format for dmenu / rofi / fzf / wayland launchers
    #[arg(long = "dmenu", global = true)]
    pub dmenu: bool,

    /// Plain uncolored output (strips ANSI color codes)
    #[arg(long = "plain", global = true)]
    pub plain: bool,

    /// Suppress non-error output
    #[arg(short = 'q', long = "quiet", global = true)]
    pub quiet: bool,

    /// Automatically confirm prompts (e.g. deletion, drive mounting)
    #[arg(short = 'y', long = "yes", global = true)]
    pub yes: bool,

    // Legacy Flag Equivalents (100% backwards compatibility):
    /// Inspect game dependencies, runtime status, and active configuration
    #[arg(short = 'i', long = "info", num_args = 0..=1, default_missing_value = "")]
    pub info: Option<String>,

    /// List all installed Steam and Non-Steam games (alias for 'vapour list')
    #[arg(short = 'l', long = "list")]
    pub list: bool,

    /// List registered Steam libraries (alias for 'vapour libraries')
    #[arg(long = "libraries")]
    pub libraries: bool,

    /// Open interactive configuration menu (alias for 'vapour config')
    #[arg(short = 'c', long = "settings", alias = "config")]
    pub settings: bool,

    /// Quickly set configuration option (e.g. 'overlay=off')
    #[arg(long = "set")]
    pub set: Option<String>,

    /// Remap or update executable path for a Non-Steam game: --set-path <GAME> <NEW_PATH>
    #[arg(long = "set-path", num_args = 2, value_names = ["GAME", "NEW_PATH"])]
    pub set_path: Option<Vec<String>>,

    /// Replace a game's launch options (Steam games via localconfig.vdf, non-Steam via shortcuts.vdf): --set-launch-options <GAME> <OPTIONS>
    #[arg(long = "set-launch-options", alias = "set-opts", num_args = 2, value_names = ["GAME", "OPTIONS"], allow_hyphen_values = true)]
    pub set_launch_options: Option<Vec<String>>,

    /// Register a custom Steam library folder path
    #[arg(long = "add-library")]
    pub add_library: Option<PathBuf>,

    /// Remove a registered custom Steam library folder path
    #[arg(long = "del-library")]
    pub del_library: Option<PathBuf>,

    /// Add a non-Steam executable (alias for 'vapour add')
    #[arg(long = "add-game", alias = "add-non-steam")]
    pub add_game: bool,

    /// Delete a non-Steam game entry (alias for 'vapour remove <GAME>')
    #[arg(long = "del-game", alias = "remove-non-steam", value_name = "GAME")]
    pub del_game: Option<String>,

    /// Set Proton compatibility tool for a game: --compat <GAME> [TOOL]
    #[arg(long = "compat", num_args = 1..=2, value_names = ["GAME", "TOOL"])]
    pub compat_flag: Option<Vec<String>>,

    /// Check if Steam daemon is currently running (alias for 'vapour status')
    #[arg(short = 's', long = "status")]
    pub status: bool,

    /// Live telemetry terminal HUD monitoring Steam RSS, zRAM swap, and PSI stall pressure
    #[arg(short = 'm', long = "monitor")]
    pub monitor: bool,

    /// Watch status continuously with live HUD (for use with --status)
    #[arg(short = 'w', long = "watch")]
    pub watch: bool,

    /// Request running Steam instance to shut down gracefully (alias for 'vapour shutdown')
    #[arg(short = 'k', long = "shutdown", alias = "kill")]
    pub shutdown: bool,

    /// Evaluate system health and cgroups v2 readiness (alias for 'vapour doctor')
    #[arg(long = "doctor")]
    pub doctor: bool,

    /// Clean up orphaned Proton prefixes, shader caches, shared memory, and dead Steam processes (alias for 'vapour prune')
    #[arg(long = "prune", alias = "clean")]
    pub prune: bool,

    /// Trigger on-demand cgroup memory reclamation on running Steam processes (alias for 'vapour reclaim')
    #[arg(long = "reclaim")]
    pub reclaim: bool,

    /// Generate shell completions for bash, zsh, fish, elvish, or powershell
    #[arg(long = "completions", value_enum)]
    pub completions: Option<Shell>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Launch a game directly with optional parameters
    Launch(LaunchArgs),
    /// Register a non-Steam game executable or runner
    Add(AddArgs),
    /// Delete a non-Steam game entry, Steam shortcut, and Proton prefix
    #[command(alias = "rm")]
    Remove(RemoveArgs),
    /// Assign or inspect Proton compatibility tool for a game
    Compat(CompatArgs),
    /// List all installed Steam and Non-Steam games
    List(ListArgs),
    /// Manage registered Steam library folders
    Libraries(LibrariesArgs),
    /// View or update configuration options
    Config(ConfigArgs),
    /// Evaluate machine environment, cgroups v2, swap, and kernel capabilities
    Doctor(DoctorArgs),
    /// Request running Steam daemon to gracefully synchronize and exit
    #[command(alias = "kill")]
    Shutdown,
    /// Query current Steam daemon execution and IPC status
    Status(StatusArgs),
    /// Live telemetry terminal HUD monitoring Steam RSS, zRAM swap, and PSI pressure
    Monitor(MonitorArgs),
    /// Trigger on-demand cgroup memory reclamation on running Steam processes
    Reclaim,
    /// Autopilot lifecycle supervisor: watches for game launches, minimizes Steam to tray, and reclaims memory
    Daemon(DaemonArgs),
    /// Clean up orphaned Proton prefixes, shader caches, POSIX shared memory buffers (/dev/shm), and dead Steam processes holding swap/RAM
    #[command(alias = "clean")]
    Prune(PruneArgs),
    /// Generate XDG .desktop application entries for launching games with Vapour
    #[command(name = "desktop-entries", alias = "desktop")]
    DesktopEntries(DesktopEntriesArgs),
    /// Inspect game dependencies, runtime status, and active configuration
    Info(InfoArgs),
    /// Generate shell tab-completion scripts (bash, zsh, fish, elvish, powershell)
    Completions(CompletionsArgs),
}

#[derive(Args, Debug, Clone, Default)]
pub struct LaunchArgs {
    /// Game name or AppID to launch (omit for interactive picker)
    pub query: Option<String>,
    /// Sort order for game selection (recent, size, name)
    #[arg(long = "sort", value_enum, default_value = "recent")]
    pub sort: crate::library::SortOrder,
    /// Trailing game arguments
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub game_args: Vec<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct AddArgs {
    /// Title of the game
    pub name: Option<String>,
    /// Path to executable (.exe or Linux binary)
    pub exe: Option<PathBuf>,
    /// Compatibility tool (e.g. 'Proton Experimental', 'GE-Proton9-23')
    #[arg(short = 'c', long = "compat", alias = "proton")]
    pub compat: Option<String>,
    /// Launch arguments passed to the executable
    #[arg(
        short = 'o',
        long = "launch-options",
        alias = "opts",
        allow_hyphen_values = true
    )]
    pub launch_options: Option<String>,
    /// Custom icon path
    #[arg(long = "icon")]
    pub icon: Option<PathBuf>,
    /// Category tags (comma-separated)
    #[arg(long = "tags")]
    pub tags: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct RemoveArgs {
    /// Game name or AppID to delete
    pub game: Option<String>,
    /// Automatically confirm deletion without prompting
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct CompatArgs {
    /// Game name or AppID
    pub game: Option<String>,
    /// Compatibility tool name to assign
    pub tool: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ListArgs {
    /// Output machine-readable JSON array
    #[arg(long = "json")]
    pub json: bool,
    /// Single-line pipeable format for dmenu / rofi / fzf / wayland launchers
    #[arg(long = "dmenu")]
    pub dmenu: bool,
    /// Plain output (no ANSI colors)
    #[arg(long = "plain")]
    pub plain: bool,
    /// Sort order (recent, size, name)
    #[arg(long = "sort", value_enum, default_value = "recent")]
    pub sort: crate::library::SortOrder,
}

#[derive(Args, Debug, Clone)]
pub struct CompletionsArgs {
    /// Shell to generate completions for
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

#[derive(Args, Debug, Clone, Default)]
pub struct LibrariesArgs {
    /// Add custom library directory
    #[arg(long = "add")]
    pub add: Option<PathBuf>,
    /// Remove custom library directory
    #[arg(long = "del")]
    pub del: Option<PathBuf>,
    /// Output machine-readable JSON
    #[arg(long = "json")]
    pub json: bool,
    /// Plain output (no ANSI colors)
    #[arg(long = "plain")]
    pub plain: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ConfigArgs {
    /// Key-value pair to set (e.g. 'overlay=off', 'zero_ram=on')
    pub key_val: Option<String>,
    /// Query a specific configuration key
    #[arg(long = "get")]
    pub get: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct DoctorArgs {
    /// Output machine-readable JSON report
    #[arg(long = "json")]
    pub json: bool,
    /// Plain output (no ANSI colors)
    #[arg(long = "plain")]
    pub plain: bool,
    /// Also scan the installed Steam client binaries for flags vapour passes that no longer exist
    #[arg(long = "deep")]
    pub deep: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct StatusArgs {
    /// Watch status continuously with a live telemetry terminal HUD
    #[arg(short = 'w', long = "watch")]
    pub watch: bool,
    /// Refresh interval in seconds for --watch (default: 1.0)
    #[arg(short = 'i', long = "interval", default_value = "1.0")]
    pub interval: f64,
    /// Output machine-readable JSON
    #[arg(long = "json")]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct MonitorArgs {
    /// Refresh interval in seconds (default: 1.0)
    #[arg(short = 'i', long = "interval", default_value = "1.0")]
    pub interval: f64,
}

impl Default for MonitorArgs {
    fn default() -> Self {
        Self { interval: 1.0 }
    }
}

#[derive(Args, Debug, Clone, Default)]
pub struct InfoArgs {
    /// Game name or AppID
    pub query: Option<String>,
    /// Output machine-readable JSON
    #[arg(long = "json")]
    pub json: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct PruneArgs {
    /// Report prunable targets without deleting anything
    #[arg(long = "dry-run", short = 'd')]
    pub dry_run: bool,
    /// Skip confirmation prompt and delete automatically
    #[arg(long = "yes", short = 'y')]
    pub yes: bool,
    /// Only prune orphaned Proton prefixes (compatdata)
    #[arg(long = "compat-only")]
    pub compat_only: bool,
    /// Only prune orphaned Vulkan/DXVK shader caches (shadercache)
    #[arg(long = "shader-only")]
    pub shader_only: bool,
    /// Only prune orphaned Steam POSIX shared memory buffers (/dev/shm)
    #[arg(long = "shm-only")]
    pub shm_only: bool,
    /// Only prune orphaned Steam/Proton processes and swap memory (/proc)
    #[arg(long = "proc-only", alias = "swap-only")]
    pub proc_only: bool,
    /// Output machine-readable JSON
    #[arg(long = "json")]
    pub json: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct DaemonArgs {
    /// Run supervisor in foreground (logs output directly to terminal)
    #[arg(short = 'f', long = "foreground")]
    pub foreground: bool,
    /// Install and enable systemd user service (~/.config/systemd/user/vapour.service)
    #[arg(long = "install")]
    pub install: bool,
    /// Uninstall and disable systemd user service
    #[arg(long = "uninstall")]
    pub uninstall: bool,
    /// Check systemd user service and supervisor status
    #[arg(long = "status")]
    pub status: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct DesktopEntriesArgs {
    /// Preview generated desktop entries without writing files
    #[arg(long = "dry-run", short = 'd')]
    pub dry_run: bool,
    /// Remove obsolete .desktop files for uninstalled games
    #[arg(long = "clean", short = 'c')]
    pub clean: bool,
    /// Output machine-readable JSON
    #[arg(long = "json")]
    pub json: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LaunchExecutionOptions<'a> {
    pub extra_cli_args: &'a [String],
    pub dry_run: bool,
    pub no_overlay: bool,
    pub overlay: bool,
    pub direct: bool,
    pub keep_steam: bool,
    pub yes: bool,
}

#[derive(Debug, Clone)]
pub enum PlayableItem {
    Steam(Game),
    NonSteam(NonSteamGame),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnifiedGame {
    Steam {
        appid: u32,
        name: String,
        install_dir: PathBuf,
        is_mounted: bool,
        unmounted_label: Option<String>,
        size_bytes: u64,
        last_played: u64,
    },
    NonSteam {
        appid: u32,
        name: String,
        exe_path: PathBuf,
        compat_tool: Option<String>,
        is_mounted: bool,
        use_proton: bool,
    },
}

impl PlayableItem {
    pub fn name(&self) -> &str {
        match self {
            PlayableItem::Steam(g) => &g.name,
            PlayableItem::NonSteam(ng) => &ng.name,
        }
    }

    pub fn to_unified(&self) -> UnifiedGame {
        match self {
            PlayableItem::Steam(g) => {
                let install_dir = g.library_path.join("steamapps/common").join(&g.installdir);
                UnifiedGame::Steam {
                    appid: g.appid,
                    name: g.name.clone(),
                    install_dir,
                    is_mounted: !g.is_unmounted,
                    unmounted_label: g.unmounted_label.clone(),
                    size_bytes: g.size_on_disk,
                    last_played: g.last_played,
                }
            }
            PlayableItem::NonSteam(ng) => UnifiedGame::NonSteam {
                appid: ng.appid,
                name: ng.name.clone(),
                exe_path: ng.exe.clone(),
                compat_tool: ng.compat_tool.clone(),
                is_mounted: !ng.is_path_missing,
                use_proton: ng.use_proton,
            },
        }
    }
}

pub fn sort_playable_items(items: &mut [PlayableItem], order: crate::library::SortOrder) {
    match order {
        crate::library::SortOrder::Recent => {
            items.sort_by(|a, b| {
                let a_last = match a {
                    PlayableItem::Steam(g) => g.last_played,
                    PlayableItem::NonSteam(_) => 0,
                };
                let b_last = match b {
                    PlayableItem::Steam(g) => g.last_played,
                    PlayableItem::NonSteam(_) => 0,
                };
                b_last
                    .cmp(&a_last)
                    .then_with(|| a.name().to_lowercase().cmp(&b.name().to_lowercase()))
            });
        }
        crate::library::SortOrder::Size => {
            items.sort_by(|a, b| {
                let a_size = match a {
                    PlayableItem::Steam(g) => g.size_on_disk,
                    PlayableItem::NonSteam(_) => 0,
                };
                let b_size = match b {
                    PlayableItem::Steam(g) => g.size_on_disk,
                    PlayableItem::NonSteam(_) => 0,
                };
                b_size
                    .cmp(&a_size)
                    .then_with(|| a.name().to_lowercase().cmp(&b.name().to_lowercase()))
            });
        }
        crate::library::SortOrder::Name => {
            items.sort_by_key(|a| a.name().to_lowercase());
        }
    }
}

impl fmt::Display for PlayableItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayableItem::Steam(g) => {
                let mut meta = Vec::new();
                if g.size_on_disk > 0 {
                    meta.push(crate::library::format_bytes(g.size_on_disk));
                }
                if g.last_played > 0 {
                    meta.push(crate::library::format_relative_time(
                        g.last_played,
                        crate::library::current_epoch_secs(),
                    ));
                }
                let meta_tag = if meta.is_empty() {
                    String::new()
                } else {
                    format!(" \x1b[90m[{}]\x1b[0m", meta.join(" | "))
                };

                if g.is_unmounted {
                    let label = g.unmounted_label.as_deref().unwrap_or("External");
                    write!(
                        f,
                        "{:<36} \x1b[36m[Steam AppID: {:>7}]\x1b[0m{meta_tag} \x1b[33m[UNMOUNTED: {}]\x1b[0m",
                        g.name, g.appid, label
                    )
                } else {
                    write!(
                        f,
                        "{:<36} \x1b[36m[Steam AppID: {:>7}]\x1b[0m{meta_tag}",
                        g.name, g.appid
                    )
                }
            }
            PlayableItem::NonSteam(ng) => {
                let runner = if let Some(ref tool) = ng.compat_tool {
                    format!("\x1b[33m[Proton: {}]\x1b[0m", tool)
                } else if ng.use_proton {
                    "\x1b[33m[Proton]\x1b[0m".to_string()
                } else {
                    "\x1b[32m[Native]\x1b[0m".to_string()
                };
                if ng.is_path_missing {
                    write!(
                        f,
                        "{:<36} \x1b[35m[Non-Steam]\x1b[0m {} \x1b[31m[PATH NOT FOUND / MOVED]\x1b[0m",
                        ng.name, runner
                    )
                } else {
                    write!(f, "{:<36} \x1b[35m[Non-Steam]\x1b[0m {}", ng.name, runner)
                }
            }
        }
    }
}

pub fn resolve_launch_options(
    raw_args: Vec<String>,
    mut dry_run: bool,
    mut no_overlay: bool,
    mut overlay: bool,
    mut direct: bool,
    mut keep_steam: bool,
    mut yes: bool,
) -> (Vec<String>, bool, bool, bool, bool, bool, bool) {
    let mut clean_args = Vec::new();
    for arg in raw_args {
        match arg.as_str() {
            "--dry-run" | "-d" => dry_run = true,
            "--no-overlay" => no_overlay = true,
            "--overlay" => overlay = true,
            "--direct" => direct = true,
            "--keep-steam" => keep_steam = true,
            "--yes" | "-y" => yes = true,
            _ => clean_args.push(arg),
        }
    }
    (
        clean_args, dry_run, no_overlay, overlay, direct, keep_steam, yes,
    )
}

pub fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

pub fn run() -> VapourResult<()> {
    // Install panic hook to ensure terminal cursor visibility is always restored
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        print!("\x1b[?25h");
        let _ = std::io::stdout().flush();
        default_hook(info);
    }));

    crate::launcher::adopt_session_env();

    let raw_args: Vec<String> = std::env::args().collect();
    let normalized_args: Vec<String> = raw_args
        .into_iter()
        .enumerate()
        .map(|(i, arg)| {
            if i == 0 {
                return arg;
            }
            // Allow single dash for known multi-character flags (e.g. -set-path -> --set-path)
            if arg.starts_with('-') && !arg.starts_with("--") {
                let stripped = &arg[1..];
                if matches!(
                    stripped,
                    "set-path"
                        | "set-launch-options"
                        | "set-opts"
                        | "del-game"
                        | "add-game"
                        | "add-non-steam"
                        | "remove-non-steam"
                        | "del-library"
                        | "add-library"
                        | "keep-steam"
                        | "no-overlay"
                        | "overlay"
                        | "direct"
                        | "shutdown"
                        | "kill"
                        | "settings"
                        | "config"
                        | "status"
                        | "libraries"
                        | "completions"
                        | "dry-run"
                        | "info"
                        | "list"
                        | "doctor"
                        | "compat"
                        | "help"
                        | "version"
                        | "yes"
                        | "json"
                        | "dmenu"
                        | "plain"
                        | "quiet"
                ) || stripped.starts_with("set=")
                    || stripped.starts_with("info=")
                    || stripped.starts_with("completions=")
                    || stripped.starts_with("compat=")
                {
                    return format!("--{}", stripped);
                }
            }
            arg
        })
        .collect();

    let cli = Cli::parse_from(normalized_args);

    if let Some(shell) = cli.completions {
        let mut cmd = Cli::command();
        generate(shell, &mut cmd, "vapour", &mut std::io::stdout());
        return Ok(());
    }

    let mut config = VapourConfig::load_checked()?;

    // Check Subcommands first
    if let Some(cmd) = cli.command {
        match cmd {
            Commands::Launch(args) => {
                let mut raw_args = args.game_args;
                raw_args.extend(cli.game_args);
                let (game_args, dry_run, no_overlay, overlay, direct, keep_steam, yes) =
                    resolve_launch_options(
                        raw_args,
                        cli.dry_run,
                        cli.no_overlay,
                        cli.overlay,
                        cli.direct,
                        cli.keep_steam,
                        cli.yes,
                    );
                let opts = LaunchExecutionOptions {
                    extra_cli_args: &game_args,
                    dry_run,
                    no_overlay,
                    overlay,
                    direct,
                    keep_steam,
                    yes,
                };
                if let Some(ref q) = args.query {
                    execute_game_launch(&config, q, &opts)?;
                } else {
                    if !is_interactive() {
                        return Err(VapourError::Usage(
                            "Interactive selector requires a terminal. Usage: vapour launch <GAME_OR_APPID>".into(),
                        ));
                    }
                    let sort = cli.sort.unwrap_or(args.sort);
                    run_interactive_selector(&config, &opts, sort)?;
                }
                return Ok(());
            }
            Commands::Add(args) => {
                handle_add_game(&mut config, args, cli.quiet, cli.json)?;
                return Ok(());
            }
            Commands::Remove(args) => {
                let yes = args.yes || cli.yes;
                handle_remove_game(&mut config, args.game, yes, cli.quiet, cli.json)?;
                return Ok(());
            }
            Commands::Compat(args) => {
                handle_compat(&mut config, args.game, args.tool, cli.quiet, cli.json)?;
                return Ok(());
            }
            Commands::List(args) => {
                let json = args.json || cli.json;
                let dmenu = args.dmenu || cli.dmenu;
                let plain = args.plain || cli.plain;
                let sort = cli.sort.unwrap_or(args.sort);
                list_all_games(&config, json, dmenu, plain, sort);
                return Ok(());
            }
            Commands::Completions(args) => {
                let mut cmd = Cli::command();
                generate(args.shell, &mut cmd, "vapour", &mut std::io::stdout());
                return Ok(());
            }
            Commands::Libraries(args) => {
                if let Some(ref path) = args.add {
                    add_custom_library(path)?;
                    if !cli.quiet {
                        println!("==> Successfully registered library: {}", path.display());
                    }
                    return Ok(());
                }
                if let Some(ref path) = args.del {
                    remove_custom_library(path)?;
                    if !cli.quiet {
                        println!("==> Successfully removed library: {}", path.display());
                    }
                    return Ok(());
                }
                let json = args.json || cli.json;
                let plain = args.plain || cli.plain;
                list_libraries(json, plain);
                return Ok(());
            }
            Commands::Config(args) => {
                if let Some(ref key) = args.get {
                    handle_config_get(&config, key, cli.json)?;
                    return Ok(());
                }
                if let Some(ref pair) = args.key_val {
                    handle_set_command(&mut config, pair)?;
                    return Ok(());
                }
                if is_interactive() {
                    interactive_settings(&mut config);
                } else {
                    println!("{}", config.serialize());
                }
                return Ok(());
            }
            Commands::Doctor(args) => {
                let json = args.json || cli.json;
                let plain = args.plain || cli.plain;
                handle_doctor(json, plain, args.deep)?;
                return Ok(());
            }
            Commands::Shutdown => {
                handle_shutdown(cli.quiet, cli.json)?;
                return Ok(());
            }
            Commands::Status(args) => {
                let json = args.json || cli.json;
                let watch = args.watch || cli.watch;
                check_status(watch, args.interval, json);
                return Ok(());
            }
            Commands::Monitor(args) => {
                check_status(true, args.interval, false);
                return Ok(());
            }
            Commands::Reclaim => {
                crate::watcher::reclaim_steam_memory()?;
                return Ok(());
            }
            Commands::Daemon(args) => {
                handle_daemon(args, &config)?;
                return Ok(());
            }
            Commands::Prune(args) => {
                let json = args.json || cli.json;
                handle_prune(args, json)?;
                return Ok(());
            }
            Commands::DesktopEntries(args) => {
                let json = args.json || cli.json;
                handle_desktop_entries(args, json)?;
                return Ok(());
            }
            Commands::Info(args) => {
                let json = args.json || cli.json;
                handle_info(&config, args.query.as_deref(), json)?;
                return Ok(());
            }
        }
    }

    // Legacy Flag Handlers (100% Backwards Compatibility)
    if cli.doctor {
        handle_doctor(cli.json, cli.plain, false)?;
        return Ok(());
    }

    if cli.prune {
        handle_prune(
            PruneArgs {
                dry_run: cli.dry_run,
                yes: cli.yes,
                json: cli.json,
                ..Default::default()
            },
            cli.json,
        )?;
        return Ok(());
    }

    if cli.reclaim {
        crate::watcher::reclaim_steam_memory()?;
        return Ok(());
    }

    if cli.list {
        list_all_games(
            &config,
            cli.json,
            cli.dmenu,
            cli.plain,
            cli.sort.unwrap_or_default(),
        );
        return Ok(());
    }

    if let Some(ref args) = cli.set_path {
        handle_set_path(&mut config, &args[0], &args[1])?;
        return Ok(());
    }

    if let Some(ref args) = cli.set_launch_options {
        handle_set_launch_options(&mut config, &args[0], &args[1])?;
        return Ok(());
    }

    if let Some(ref q) = cli.del_game {
        handle_remove_game(&mut config, Some(q.clone()), cli.yes, cli.quiet, cli.json)?;
        return Ok(());
    }

    if let Some(ref args) = cli.compat_flag {
        let game = args.first().cloned();
        let tool = args.get(1).cloned();
        handle_compat(&mut config, game, tool, cli.quiet, cli.json)?;
        return Ok(());
    }

    if cli.libraries {
        list_libraries(cli.json, cli.plain);
        return Ok(());
    }

    if cli.settings {
        interactive_settings(&mut config);
        return Ok(());
    }

    if let Some(ref pair) = cli.set {
        handle_set_command(&mut config, pair)?;
        return Ok(());
    }

    if let Some(ref path) = cli.add_library {
        add_custom_library(path)?;
        if !cli.quiet {
            println!("==> Successfully registered library: {}", path.display());
        }
        return Ok(());
    }

    if let Some(ref path) = cli.del_library {
        remove_custom_library(path)?;
        if !cli.quiet {
            println!("==> Successfully removed library: {}", path.display());
        }
        return Ok(());
    }

    if cli.add_game {
        let add_args = AddArgs::default();
        handle_add_game(&mut config, add_args, cli.quiet, cli.json)?;
        return Ok(());
    }

    if cli.monitor {
        check_status(true, 1.0, false);
        return Ok(());
    }

    if cli.status {
        check_status(cli.watch, 1.0, cli.json);
        return Ok(());
    }

    if cli.shutdown {
        handle_shutdown(cli.quiet, cli.json)?;
        return Ok(());
    }

    if let Some(ref q) = cli.info {
        handle_info(
            &config,
            if q.is_empty() { None } else { Some(q.as_str()) },
            cli.json,
        )?;
        return Ok(());
    }

    let (game_args, dry_run, no_overlay, overlay, direct, keep_steam, yes) = resolve_launch_options(
        cli.game_args,
        cli.dry_run,
        cli.no_overlay,
        cli.overlay,
        cli.direct,
        cli.keep_steam,
        cli.yes,
    );

    let exec_opts = LaunchExecutionOptions {
        extra_cli_args: &game_args,
        dry_run,
        no_overlay,
        overlay,
        direct,
        keep_steam,
        yes,
    };

    // Direct launch via positional argument: vapour <QUERY> [GAME_ARGS...]
    if let Some(ref query) = cli.query {
        return execute_game_launch(&config, query, &exec_opts);
    }

    // No arguments provided: Interactive TUI mode
    if !is_interactive() {
        return Err(VapourError::Usage(
            "Interactive selector requires a terminal. Usage: vapour [GAME_OR_APPID] or run 'vapour list' to inspect available games.".into(),
        ));
    }

    run_interactive_selector(&config, &exec_opts, cli.sort.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::tui::extract_acronyms;
    use super::*;

    #[test]
    fn test_extract_acronyms_generic() {
        let acr1 = extract_acronyms("Grand Space Adventure 2");
        assert!(acr1.contains(&"gsa2".to_string()));

        let acr2 = extract_acronyms("Team Arena: Battle Royale");
        assert!(acr2.contains(&"tabr".to_string()));
        assert!(acr2.contains(&"ta".to_string()));
        assert!(acr2.contains(&"br".to_string()));

        let acr3 = extract_acronyms("The Elder Realm 5");
        assert!(acr3.contains(&"ter5".to_string()));
        assert!(acr3.contains(&"er5".to_string()));

        let acr4 = extract_acronyms("Hero's Duty 4");
        assert!(acr4.contains(&"hd4".to_string()));
    }

    #[test]
    fn test_cli_short_flags() {
        let cli = Cli::parse_from(vec!["vapour", "-i", "cs2"]);
        assert_eq!(cli.info, Some("cs2".into()));

        let cli_k = Cli::parse_from(vec!["vapour", "-k"]);
        assert!(cli_k.shutdown);

        let cli_c = Cli::parse_from(vec!["vapour", "-c"]);
        assert!(cli_c.settings);

        let cli_m = Cli::parse_from(vec!["vapour", "-m"]);
        assert!(cli_m.monitor);

        let cli_sw = Cli::parse_from(vec!["vapour", "-s", "-w"]);
        assert!(cli_sw.status);
        assert!(cli_sw.watch);
    }

    #[test]
    fn test_cli_trailing_flags_extraction() {
        let args: Vec<String> = vec![
            "vapour".into(),
            "cs2".into(),
            "-novid".into(),
            "--dry-run".into(),
        ];
        let cli = Cli::parse_from(args);
        assert_eq!(cli.query, Some("cs2".into()));

        let mut dry_run = cli.dry_run;
        let mut extra_cli_args = Vec::new();
        for arg in cli.game_args {
            match arg.as_str() {
                "--dry-run" | "-d" => dry_run = true,
                _ => extra_cli_args.push(arg),
            }
        }
        assert!(dry_run);
        assert_eq!(extra_cli_args, vec!["-novid"]);
    }

    #[test]
    fn test_subcommands_parsing() {
        // Test 'vapour add'
        let cli_add = Cli::parse_from(vec![
            "vapour",
            "add",
            "Hollow Knight",
            "/opt/hk/start.sh",
            "--compat",
            "Proton Experimental",
        ]);
        match cli_add.command {
            Some(Commands::Add(args)) => {
                assert_eq!(args.name, Some("Hollow Knight".to_string()));
                assert_eq!(args.exe, Some(PathBuf::from("/opt/hk/start.sh")));
                assert_eq!(args.compat, Some("Proton Experimental".to_string()));
            }
            _ => panic!("Expected Commands::Add"),
        }

        // Test 'vapour rm' alias and flag
        let cli_rm = Cli::parse_from(vec!["vapour", "rm", "Hollow Knight", "--yes"]);
        match cli_rm.command {
            Some(Commands::Remove(args)) => {
                assert_eq!(args.game, Some("Hollow Knight".to_string()));
                assert!(args.yes);
            }
            _ => panic!("Expected Commands::Remove"),
        }

        // Test 'vapour list --json'
        let cli_list = Cli::parse_from(vec!["vapour", "list", "--json"]);
        match cli_list.command {
            Some(Commands::List(args)) => {
                assert!(args.json);
                assert!(!args.dmenu);
            }
            _ => panic!("Expected Commands::List"),
        }

        // Test 'vapour doctor --plain'
        let cli_doc = Cli::parse_from(vec!["vapour", "doctor", "--plain"]);
        match cli_doc.command {
            Some(Commands::Doctor(args)) => {
                assert!(args.plain);
                assert!(!args.json);
            }
            _ => panic!("Expected Commands::Doctor"),
        }

        // Test 'vapour compat 730 Proton-GE'
        let cli_compat = Cli::parse_from(vec!["vapour", "compat", "730", "GE-Proton9-23"]);
        match cli_compat.command {
            Some(Commands::Compat(args)) => {
                assert_eq!(args.game, Some("730".to_string()));
                assert_eq!(args.tool, Some("GE-Proton9-23".to_string()));
            }
            _ => panic!("Expected Commands::Compat"),
        }

        // Test 'vapour completions zsh'
        let cli_comp = Cli::parse_from(vec!["vapour", "completions", "zsh"]);
        match cli_comp.command {
            Some(Commands::Completions(args)) => {
                assert_eq!(args.shell, clap_complete::Shell::Zsh);
            }
            _ => panic!("Expected Commands::Completions"),
        }

        // Test 'vapour list --sort size'
        let cli_sort_list = Cli::parse_from(vec!["vapour", "list", "--sort", "size"]);
        match cli_sort_list.command {
            Some(Commands::List(args)) => {
                assert_eq!(args.sort, crate::library::SortOrder::Size);
            }
            _ => panic!("Expected Commands::List"),
        }

        // Test 'vapour status --watch -i 2.5'
        let cli_status_watch = Cli::parse_from(vec!["vapour", "status", "--watch", "-i", "2.5"]);
        match cli_status_watch.command {
            Some(Commands::Status(args)) => {
                assert!(args.watch);
                assert!((args.interval - 2.5).abs() < f64::EPSILON);
            }
            _ => panic!("Expected Commands::Status"),
        }

        // Test 'vapour monitor'
        let cli_monitor = Cli::parse_from(vec!["vapour", "monitor", "-i", "0.5"]);
        match cli_monitor.command {
            Some(Commands::Monitor(args)) => {
                assert!((args.interval - 0.5).abs() < f64::EPSILON);
            }
            _ => panic!("Expected Commands::Monitor"),
        }

        // Test 'vapour reclaim'
        let cli_reclaim = Cli::parse_from(vec!["vapour", "reclaim"]);
        assert!(matches!(cli_reclaim.command, Some(Commands::Reclaim)));

        // Test 'vapour daemon'
        let cli_daemon = Cli::parse_from(vec!["vapour", "daemon", "--foreground"]);
        assert!(matches!(cli_daemon.command, Some(Commands::Daemon(ref args)) if args.foreground));

        // Test 'vapour prune --dry-run'
        let cli_prune = Cli::parse_from(vec!["vapour", "prune", "--dry-run", "--compat-only"]);
        match cli_prune.command {
            Some(Commands::Prune(args)) => {
                assert!(args.dry_run);
                assert!(args.compat_only);
                assert!(!args.shader_only);
            }
            _ => panic!("Expected Commands::Prune"),
        }

        // Test 'vapour desktop-entries --clean'
        let cli_desktop = Cli::parse_from(vec!["vapour", "desktop-entries", "--clean"]);
        match cli_desktop.command {
            Some(Commands::DesktopEntries(args)) => {
                assert!(args.clean);
                assert!(!args.dry_run);
            }
            _ => panic!("Expected Commands::DesktopEntries"),
        }
    }

    #[test]
    fn test_unified_game_serialization() {
        let ug = UnifiedGame::Steam {
            appid: 730,
            name: "Counter-Strike 2".to_string(),
            install_dir: PathBuf::from("/path/to/cs2"),
            is_mounted: true,
            unmounted_label: None,
            size_bytes: 35000000000,
            last_played: 1730000000,
        };
        let json_str = serde_json::to_string(&ug).unwrap();
        assert!(json_str.contains("\"kind\":\"steam\""));
        assert!(json_str.contains("\"appid\":730"));
        assert!(json_str.contains("\"name\":\"Counter-Strike 2\""));
    }

    #[test]
    fn test_doctor_report_serialization() {
        let report = crate::doctor::run_doctor_checks(false);
        let json_str = report.to_json();
        assert!(json_str.contains("\"kernel\""));
        assert!(json_str.contains("\"cgroup_v2\""));
        assert!(json_str.contains("\"swap_zram\""));
        assert!(json_str.contains("\"all_healthy\""));
    }
}
