//! `HarnessModule` trait and context types.
//!
//! Each harness (Claude, Codex, Gemini, plus the phase-1 fake harness used
//! in tests) plugs into the shared plan -> execute -> record pipeline via
//! this trait. All inputs arrive via context structs; trait methods return
//! typed errors so the CLI can map them to stable exit codes.

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::config::KatachiConfig;
use crate::error::{ExecutionError, PlanError, ResolveError};
use crate::model::{HarnessKind, ItemRef};
use crate::paths::StoragePaths;
use crate::persist::RunDirectory;
use crate::plan::{ExecutionPlan, InvocationRequest, ResolvedKatachi};
use crate::record::{ExecutionRecord, RunId};
pub use crate::roster::{
    Constraint, DependencyEdge, DiscoveredItem, EdgeKind, EdgeRole, EdgeRoleMask, ItemSource,
    PackageRef, RosterCatalog,
};

/// Contract implemented once per harness. Stateless: all inputs arrive
/// via context structs.
pub trait HarnessModule: Send + Sync {
    fn kind(&self) -> HarnessKind;

    /// Inventory harness-native artifacts into a roster catalog. Phase 2
    /// will flesh this out; Phase 1 harnesses may return an empty catalog.
    fn scan(&self, ctx: &ScanContext<'_>) -> Result<RosterCatalog, ResolveError>;

    /// Explain a single roster item in human-readable form.
    fn explain(&self, ctx: &ExplainContext<'_>) -> Result<ExplainResult, ResolveError>;

    /// Resolve a katachi invocation into a backend + selected item set.
    fn resolve(&self, ctx: &ResolveContext<'_>) -> Result<ResolvedKatachi, ResolveError>;

    /// Translate a resolved katachi into a concrete execution plan.
    fn plan(&self, ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError>;

    /// Run the plan against a real backend and produce a record.
    ///
    /// Implementations stream transcript events, stdout, and stderr into
    /// `ctx.run_dir` while the child runs. The caller is responsible for
    /// writing `request.json` / `plan.json` before calling, and for
    /// writing `manifest.json` + committing the directory afterwards.
    fn execute(&self, ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError>;

    /// Normalize a single stdout line into a transcript event. The shared
    /// executor calls this when `transcript_mode` is `JsonStream`.
    ///
    /// The default keeps valid JSON as a raw `JsonEvent` payload and
    /// passes everything else through as `StdoutText`. Harnesses can
    /// override to recognize their own event shapes (assistant/user/
    /// tool_use/tool_result/result) and map them onto typed variants.
    fn normalize_stdout_line(&self, line: &str) -> crate::transcript::EventKind {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return crate::transcript::EventKind::StdoutText { text: line.into() };
        }
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(payload) => crate::transcript::EventKind::JsonEvent { payload },
            Err(_) => crate::transcript::EventKind::StdoutText { text: line.into() },
        }
    }
}

/// Context for [`HarnessModule::scan`].
#[derive(Debug, Clone, Copy)]
pub struct ScanContext<'a> {
    pub config: &'a KatachiConfig,
    pub paths: &'a StoragePaths,
    pub cwd: &'a Utf8Path,
}

/// Context for [`HarnessModule::explain`].
#[derive(Debug, Clone, Copy)]
pub struct ExplainContext<'a> {
    pub item: &'a ItemRef,
    pub catalog: &'a RosterCatalog,
    pub cwd: &'a Utf8Path,
}

/// Result of [`HarnessModule::explain`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExplainResult {
    pub item: ItemRef,
    pub summary: String,
    #[serde(default)]
    pub sections: Vec<ExplainSection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExplainSection {
    pub title: String,
    pub body: String,
}

/// Context for [`HarnessModule::resolve`].
#[derive(Debug, Clone, Copy)]
pub struct ResolveContext<'a> {
    pub request: &'a InvocationRequest,
    pub catalog: &'a RosterCatalog,
    pub config: &'a KatachiConfig,
}

/// Context for [`HarnessModule::plan`].
#[derive(Debug, Clone, Copy)]
pub struct PlanContext<'a> {
    pub request: &'a InvocationRequest,
    pub resolved: &'a ResolvedKatachi,
    pub run_id: RunId,
}

/// Context for [`HarnessModule::execute`].
///
/// `run_dir` is a created-but-not-committed [`RunDirectory`]. The executor
/// opens transcript/stdout/stderr writers from it; the caller commits
/// once execute returns.
pub struct ExecuteContext<'a> {
    pub request: &'a InvocationRequest,
    pub plan: &'a ExecutionPlan,
    pub run_dir: &'a RunDirectory,
    pub started_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_result_roundtrip() {
        let r = ExplainResult {
            item: ItemRef::new(HarnessKind::Gemini, "extension", "workspace-a11y"),
            summary: "an extension".into(),
            sections: vec![ExplainSection {
                title: "Notes".into(),
                body: "test".into(),
            }],
        };
        let j = serde_json::to_value(&r).unwrap();
        let back: ExplainResult = serde_json::from_value(j).unwrap();
        assert_eq!(back.item.id, "workspace-a11y");
        assert_eq!(back.sections.len(), 1);
    }

    /// Trivial impl to prove the trait is object-safe where it matters and
    /// compiles cleanly for stub harnesses.
    struct DummyHarness;
    impl HarnessModule for DummyHarness {
        fn kind(&self) -> HarnessKind {
            HarnessKind::Claude
        }
        fn scan(&self, _ctx: &ScanContext<'_>) -> Result<RosterCatalog, ResolveError> {
            Ok(RosterCatalog::empty(HarnessKind::Claude))
        }
        fn explain(&self, ctx: &ExplainContext<'_>) -> Result<ExplainResult, ResolveError> {
            Ok(ExplainResult {
                item: ctx.item.clone(),
                summary: "dummy".into(),
                sections: Vec::new(),
            })
        }
        fn resolve(&self, _ctx: &ResolveContext<'_>) -> Result<ResolvedKatachi, ResolveError> {
            Err(ResolveError::UnknownKatachi { id: "dummy".into() })
        }
        fn plan(&self, _ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
            Err(PlanError::BuildFailed {
                message: "dummy".into(),
            })
        }
        fn execute(&self, _ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
            Err(ExecutionError::NonZeroExit {
                command: "dummy".into(),
                status: "exit-code: 1".into(),
            })
        }
    }

    #[test]
    fn trait_is_usable_as_trait_object() {
        let h: Box<dyn HarnessModule> = Box::new(DummyHarness);
        assert_eq!(h.kind(), HarnessKind::Claude);
    }
}
