//! The [`GeminiHarness`] implementation of [`HarnessModule`].
//!
//! Discovery and planning is split across sibling modules (`settings`,
//! `context`, `extension`, `scan`, `plan`, etc.) — this file just wires
//! them into the trait surface.

use katachi_core::error::{ExecutionError, PlanError, ResolveError};
use katachi_core::harness::{
    ExecuteContext, ExplainContext, ExplainResult, HarnessModule, PlanContext, ResolveContext,
    RosterCatalog, ScanContext,
};
use katachi_core::model::HarnessKind;
use katachi_core::plan::{ExecutionPlan, ResolvedKatachi};
use katachi_core::record::ExecutionRecord;

use crate::config::GeminiConfig;
use crate::plan::build_plan;
use crate::scan::scan_gemini;

#[derive(Default, Debug, Clone)]
pub struct GeminiHarness;

impl GeminiHarness {
    pub fn new() -> Self {
        Self
    }
}

impl HarnessModule for GeminiHarness {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Gemini
    }

    fn scan(&self, ctx: &ScanContext<'_>) -> Result<RosterCatalog, ResolveError> {
        let gemini_cfg = GeminiConfig::from_katachi(ctx.config)
            .map_err(|e| ResolveError::UnknownItem { item: e.to_string() })?;
        Ok(scan_gemini(&gemini_cfg, ctx.cwd))
    }

    fn explain(&self, ctx: &ExplainContext<'_>) -> Result<ExplainResult, ResolveError> {
        let item = ctx
            .catalog
            .get(ctx.item)
            .ok_or_else(|| ResolveError::UnknownItem {
                item: ctx.item.to_string(),
            })?;
        Ok(ExplainResult {
            item: ctx.item.clone(),
            summary: item.display_name.clone(),
            sections: crate::scan::explain_sections_for(item, ctx.catalog),
        })
    }

    fn resolve(&self, _ctx: &ResolveContext<'_>) -> Result<ResolvedKatachi, ResolveError> {
        // Shared resolver in `katachi_core::resolve` drives Gemini targets;
        // the module-local `resolve()` trait method is reserved for future
        // extensions-aware resolution and currently returns a helpful
        // error when invoked directly.
        Err(ResolveError::UnknownKatachi {
            id: "use katachi_core::resolve::resolve() for Gemini".into(),
        })
    }

    fn plan(&self, ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
        build_plan(ctx)
    }

    fn execute(&self, ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
        katachi_core::execute::run(ctx)
    }
}
