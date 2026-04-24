//! CLI definition: clap derive types for the full `katachi` command tree.
//!
//! Most subcommands are placeholders until later phases; they return
//! [`ExitCode::NotImplemented`] via [`run_placeholder`].

use camino::Utf8PathBuf;
use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "katachi",
    version,
    about = "Named loadouts for coding-agent harnesses",
    long_about = None,
    propagate_version = true,
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub cmd: Command,
}

/// Flags that apply to every subcommand.
#[derive(Args, Debug, Clone, Default)]
pub struct GlobalArgs {
    /// Override the config file location.
    #[arg(long, global = true, value_name = "PATH", env = "KATACHI_CONFIG")]
    pub config: Option<Utf8PathBuf>,

    /// Override the data root (runs, katachis, rosters).
    #[arg(long, global = true, value_name = "PATH", env = "KATACHI_DATA")]
    pub data_root: Option<Utf8PathBuf>,

    /// Override the cache root.
    #[arg(long, global = true, value_name = "PATH", env = "KATACHI_CACHE")]
    pub cache_root: Option<Utf8PathBuf>,

    /// Run as if started in this working directory.
    #[arg(long, global = true, value_name = "PATH")]
    pub cwd: Option<Utf8PathBuf>,

    /// Emit structured JSON output where supported.
    #[arg(long, global = true)]
    pub json: bool,

    /// Override the materialization strategy for this invocation.
    #[arg(long, global = true, value_enum, value_name = "MODE")]
    pub materialization: Option<MaterializationArg>,

    /// Harness preference override (repeatable, highest preference first).
    #[arg(long, global = true, value_name = "NAME")]
    pub prefer_harness: Vec<String>,

    /// Backend preference override (repeatable, highest preference first).
    #[arg(long, global = true, value_name = "NAME")]
    pub prefer_backend: Vec<String>,

    /// Plan without executing. Prints the plan and exits.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Increase log verbosity. Repeat for more (`-v`, `-vv`).
    #[arg(long, short = 'v', global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum MaterializationArg {
    Ambient,
    TempOverlay,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Work with a named katachi (user-facing loadout).
    Have(HaveCmd),

    /// Manage katachi definitions on disk.
    Katachi(KatachiCmd),

    /// Drive a specific harness directly.
    Harness(HarnessCmd),

    /// Inspect previous runs.
    Run(RunCmd),

    /// Environment and configuration diagnostics.
    Doctor,
}

// ---------- `have` ----------

#[derive(Args, Debug)]
pub struct HaveCmd {
    /// Logical katachi id.
    pub id: String,

    #[command(subcommand)]
    pub action: HaveAction,
}

#[derive(Subcommand, Debug)]
pub enum HaveAction {
    /// Report what this katachi resolves to without executing.
    Describe,

    /// Print the dependency graph for this katachi.
    Graph {
        /// Render format.
        #[arg(long, value_enum, default_value_t = GraphFormat::Text)]
        format: GraphFormat,
    },

    /// Plan-only actions (no execution).
    Plan {
        #[command(subcommand)]
        what: HavePlanAction,
    },

    /// Execute this katachi against the given prompt.
    Execute {
        /// Prompt passed to the resolved harness.
        prompt: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum HavePlanAction {
    /// Show the execution plan for `execute <prompt>`.
    Execute { prompt: String },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum GraphFormat {
    Text,
    Json,
    Dot,
}

// ---------- `katachi` (admin) ----------

#[derive(Args, Debug)]
pub struct KatachiCmd {
    #[command(subcommand)]
    pub action: KatachiAction,
}

#[derive(Subcommand, Debug)]
pub enum KatachiAction {
    /// List known katachi definitions.
    List,
    /// Show a katachi definition by id.
    Show { id: String },
    /// Validate a katachi definition against the current rosters.
    Validate { id: String },
}

// ---------- `harness` ----------

#[derive(Args, Debug)]
pub struct HarnessCmd {
    /// Which harness to drive.
    #[arg(value_enum)]
    pub name: HarnessName,

    #[command(subcommand)]
    pub action: HarnessAction,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum HarnessName {
    Claude,
    Codex,
    Gemini,
}

impl HarnessName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum HarnessAction {
    /// Scan the filesystem for harness-native artifacts.
    Scan,
    /// Show detail on a discovered item.
    Explain {
        #[arg(value_name = "ITEM_ID")]
        item_id: String,
    },
    /// Render the discovered roster graph.
    Graph {
        #[arg(long, value_enum, default_value_t = GraphFormat::Text)]
        format: GraphFormat,
    },
    /// Plan-only actions (no execution).
    Plan {
        #[arg(value_name = "ROSTER_ID")]
        roster_id: String,
        #[command(subcommand)]
        what: HarnessPlanAction,
    },
    /// Execute a roster-defined loadout against a prompt.
    Execute {
        #[arg(value_name = "ROSTER_ID")]
        roster_id: String,
        prompt: String,
    },
    /// Emit the effective config for a roster without running.
    EffectiveConfig {
        #[arg(value_name = "ROSTER_ID")]
        roster_id: String,
    },
    /// Harness-specific diagnostics.
    Doctor,
}

#[derive(Subcommand, Debug)]
pub enum HarnessPlanAction {
    /// Show the execution plan for `execute <prompt>`.
    Execute { prompt: String },
}

// ---------- `run` ----------

#[derive(Args, Debug)]
pub struct RunCmd {
    #[command(subcommand)]
    pub action: RunAction,
}

#[derive(Subcommand, Debug)]
pub enum RunAction {
    /// List recent runs.
    List,
    /// Show the metadata for a run.
    Show { run_id: String },
    /// Print the captured transcript for a run.
    Transcript { run_id: String },
}
