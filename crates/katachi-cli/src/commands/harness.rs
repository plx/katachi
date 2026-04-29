//! `katachi harness <claude|codex|gemini> ...` dispatcher.
//!
//! Each harness has its own submodule that implements its actions; this
//! file just routes by [`HarnessName`].

pub mod claude;
pub mod gemini;

use anyhow::Result;

use crate::cli::{GlobalArgs, HarnessCmd, HarnessName};
use crate::exit::ExitCode;

pub fn dispatch(global: &GlobalArgs, cmd: HarnessCmd) -> Result<ExitCode> {
    match cmd.name {
        HarnessName::Claude => claude::dispatch(global, cmd.action),
        HarnessName::Gemini => gemini::dispatch(global, cmd.action),
        HarnessName::Codex => crate::commands::harness_codex::dispatch(global, &cmd),
    }
}
