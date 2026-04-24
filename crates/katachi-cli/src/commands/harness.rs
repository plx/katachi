//! `katachi harness <claude|codex|gemini> ...` dispatcher.
//!
//! Step 1: a concrete `gemini scan` implementation that produces a
//! `RosterCatalog` using the real Gemini harness module. Other harness
//! actions remain as `NotImplemented` stubs until their phase lands.

pub mod gemini;

use anyhow::Result;

use crate::cli::{GlobalArgs, HarnessAction, HarnessCmd, HarnessName};
use crate::exit::ExitCode;

pub fn dispatch(global: &GlobalArgs, cmd: HarnessCmd) -> Result<ExitCode> {
    match cmd.name {
        HarnessName::Gemini => gemini::dispatch(global, cmd.action),
        HarnessName::Claude | HarnessName::Codex => not_implemented(global, cmd),
    }
}

fn not_implemented(_global: &GlobalArgs, cmd: HarnessCmd) -> Result<ExitCode> {
    let action_label = match cmd.action {
        HarnessAction::Scan => "scan",
        HarnessAction::Explain { .. } => "explain",
        HarnessAction::Graph { .. } => "graph",
        HarnessAction::Plan { .. } => "plan",
        HarnessAction::Execute { .. } => "execute",
    };
    eprintln!(
        "katachi: `harness {} {}` is not yet implemented in this phase",
        cmd.name.as_str(),
        action_label
    );
    Ok(ExitCode::NotImplemented)
}
