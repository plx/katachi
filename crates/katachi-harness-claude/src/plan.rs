//! CLI/SDK planning for the Claude harness.

use katachi_core::error::PlanError;
use katachi_core::harness::PlanContext;
use katachi_core::plan::ExecutionPlan;

/// Top-level planner entry. Dispatches by `ctx.resolved.backend` in later
/// steps; for now it reports a placeholder error so the skeleton compiles.
pub fn build_plan(_ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
    Err(PlanError::BuildFailed {
        message: "claude plan is not yet implemented".into(),
    })
}
