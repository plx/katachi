//! Codex CLI planner.
//!
//! Turns a resolved katachi + run profile into an [`ExecutionPlan`] that
//! invokes `codex exec` non-interactively with explicit approval/sandbox
//! settings, machine-readable output, and (in `temp-overlay` mode) a
//! temporary `CODEX_HOME` containing the selected configuration.

// Implementation defers to Step 12.

use katachi_core::error::PlanError;
use katachi_core::harness::PlanContext;
use katachi_core::plan::ExecutionPlan;

pub fn build_plan(_ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
    Err(PlanError::BuildFailed {
        message: "codex planner is not yet implemented at this step".into(),
    })
}
