//! `HarnessModule` implementation for Codex.

use camino::Utf8PathBuf;
use katachi_core::error::{ExecutionError, PlanError, ResolveError};
use katachi_core::harness::{
    ExecuteContext, ExplainContext, ExplainResult, ExplainSection, HarnessModule, PlanContext,
    ResolveContext, RosterCatalog, ScanContext,
};
use katachi_core::model::HarnessKind;
use katachi_core::plan::{ExecutionPlan, ResolvedKatachi};
use katachi_core::record::ExecutionRecord;

use crate::discovery::{discover, DiscoveryInputs};
use crate::CodexSettings;

/// Entry-point type for the Codex harness.
#[derive(Debug, Default, Clone)]
pub struct CodexHarness;

impl CodexHarness {
    pub fn new() -> Self {
        Self
    }
}

impl HarnessModule for CodexHarness {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Codex
    }

    fn scan(&self, ctx: &ScanContext<'_>) -> Result<RosterCatalog, ResolveError> {
        let settings = CodexSettings::load(ctx.config);
        let cwd = ctx.cwd.to_path_buf();
        discover(&DiscoveryInputs { settings, cwd })
    }

    fn explain(&self, ctx: &ExplainContext<'_>) -> Result<ExplainResult, ResolveError> {
        let item = ctx
            .catalog
            .get(ctx.item)
            .ok_or_else(|| ResolveError::UnknownItem {
                item: ctx.item.to_string(),
            })?;
        let mut sections: Vec<ExplainSection> = Vec::new();
        if let Some(path) = &item.source.path {
            sections.push(ExplainSection {
                title: "Source".into(),
                body: path.to_string(),
            });
        }
        if let Some(scope) = &item.source.scope {
            sections.push(ExplainSection {
                title: "Scope".into(),
                body: scope.clone(),
            });
        }
        if !item.raw.is_null() {
            let body =
                serde_json::to_string_pretty(&item.raw).unwrap_or_else(|_| "<unprintable>".into());
            sections.push(ExplainSection {
                title: "Raw".into(),
                body,
            });
        }
        // Add edges pointing both ways for context.
        let outbound: Vec<_> = ctx.catalog.edges_from(ctx.item).collect();
        if !outbound.is_empty() {
            let body = outbound
                .iter()
                .map(|e| {
                    format!(
                        "{} -> {}  ({}){}",
                        e.from,
                        e.to,
                        e.note.as_deref().unwrap_or(""),
                        if e.required { " [required]" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(ExplainSection {
                title: "Outbound edges".into(),
                body,
            });
        }
        let inbound: Vec<_> = ctx.catalog.edges_to(ctx.item).collect();
        if !inbound.is_empty() {
            let body = inbound
                .iter()
                .map(|e| {
                    format!(
                        "{} <- {}  ({}){}",
                        e.to,
                        e.from,
                        e.note.as_deref().unwrap_or(""),
                        if e.required { " [required]" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(ExplainSection {
                title: "Inbound edges".into(),
                body,
            });
        }
        Ok(ExplainResult {
            item: ctx.item.clone(),
            summary: item.display_name.clone(),
            sections,
        })
    }

    fn resolve(&self, _ctx: &ResolveContext<'_>) -> Result<ResolvedKatachi, ResolveError> {
        // The shared resolver in `katachi_core::resolve` is the primary
        // resolution path; we only need to participate via `scan`.
        Err(ResolveError::UnknownKatachi {
            id: "codex harness does not directly implement resolve — use katachi_core::resolve"
                .into(),
        })
    }

    fn plan(&self, ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
        crate::planner::build_plan(ctx)
    }

    fn execute(&self, ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
        crate::executor::run(ctx)
    }
}

#[allow(dead_code)]
pub fn settings_from_env_cwd(cwd: &camino::Utf8Path) -> (CodexSettings, Utf8PathBuf) {
    (CodexSettings::default(), cwd.to_path_buf())
}
