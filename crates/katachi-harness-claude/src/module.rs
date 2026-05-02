//! `ClaudeHarness`: the implementation of [`HarnessModule`] for Claude.
//!
//! Orchestrates scan / explain / resolve / plan / execute for the Claude
//! backend. Individual concerns (path discovery, roster parsing, planning)
//! live in sibling modules; this file wires them together behind the
//! shared trait.

use katachi_core::error::{ExecutionError, PlanError, ResolveError};
use katachi_core::harness::{
    ExecuteContext, ExplainContext, ExplainResult, HarnessModule, PlanContext, ResolveContext,
    RosterCatalog, ScanContext,
};
use katachi_core::model::HarnessKind;
use katachi_core::plan::{ExecutionPlan, ResolvedKatachi};
use katachi_core::record::ExecutionRecord;

/// Claude harness module.
#[derive(Debug, Default, Clone)]
pub struct ClaudeHarness;

impl ClaudeHarness {
    pub fn new() -> Self {
        Self
    }
}

impl HarnessModule for ClaudeHarness {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Claude
    }

    fn scan(&self, ctx: &ScanContext<'_>) -> Result<RosterCatalog, ResolveError> {
        let catalog = crate::discovery::scan(ctx).map_err(|err| ResolveError::UnknownItem {
            item: format!("claude scan failure: {err}"),
        })?;
        Ok(catalog)
    }

    fn explain(&self, ctx: &ExplainContext<'_>) -> Result<ExplainResult, ResolveError> {
        crate::explain::explain(ctx)
    }

    fn resolve(&self, _ctx: &ResolveContext<'_>) -> Result<ResolvedKatachi, ResolveError> {
        // The shared resolver drives the Claude harness via `scan` and the
        // selector machinery. Direct invocation isn't part of the plan.
        Err(ResolveError::UnknownKatachi {
            id: "<claude-direct-resolve-not-used>".into(),
        })
    }

    fn plan(&self, ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
        crate::plan::build_plan(ctx)
    }

    fn execute(&self, ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
        // Hand the shared executor our Claude-specific stdout normalizer
        // so stream-json output comes out of the run with typed events.
        let normalizer: Box<dyn Fn(&str) -> katachi_core::transcript::EventKind + Send + Sync> =
            Box::new(crate::transcript::parse_line);
        katachi_core::execute::run_with_normalizer(ctx, Some(normalizer.as_ref()))
    }

    fn normalize_stdout_line(&self, line: &str) -> katachi_core::transcript::EventKind {
        crate::transcript::parse_line(line)
    }
}
