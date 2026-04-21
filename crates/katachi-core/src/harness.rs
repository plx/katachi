//! `HarnessModule` trait and context types.
//!
//! Each harness (Claude, Codex, Gemini, plus the phase-1 fake harness used
//! in tests) plugs into the shared plan -> execute -> record pipeline via
//! this trait. All inputs arrive via context structs; trait methods return
//! typed errors so the CLI can map them to stable exit codes.
//!
//! The Phase-2 roster types (`DiscoveredItem`, `DependencyEdge`,
//! `RosterCatalog`) are defined here as deliberately minimal stubs so the
//! trait signatures stabilize now and stay stable as Phase 2 fills them in.

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::config::KatachiConfig;
use crate::diagnostic::Diagnostic;
use crate::error::{ExecutionError, PlanError, ResolveError};
use crate::model::{HarnessKind, ItemRef};
use crate::paths::StoragePaths;
use crate::persist::RunDirectory;
use crate::plan::{ExecutionPlan, InvocationRequest, ResolvedKatachi};
use crate::record::{ExecutionRecord, RunId};

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

/// Phase-2 stub: a harness-scoped inventory of discovered items + edges.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RosterCatalog {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessKind>,
    #[serde(default)]
    pub items: Vec<DiscoveredItem>,
    #[serde(default)]
    pub edges: Vec<DependencyEdge>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

impl RosterCatalog {
    pub fn empty(harness: HarnessKind) -> Self {
        Self {
            harness: Some(harness),
            ..Default::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.edges.is_empty()
    }
}

/// Phase-2 stub: a single discovered roster item.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveredItem {
    pub item_ref: ItemRef,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub raw: serde_json::Value,
}

/// Phase-2 stub: a typed edge between two roster items.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub from: ItemRef,
    pub to: ItemRef,
    pub kind: EdgeKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Item ships inside the other's package/plugin/extension.
    Packaging,
    /// Item references or augments the other at runtime.
    Semantic,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_catalog_has_harness_tag() {
        let c = RosterCatalog::empty(HarnessKind::Claude);
        assert_eq!(c.harness, Some(HarnessKind::Claude));
        assert!(c.is_empty());
    }

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

    #[test]
    fn edge_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(EdgeKind::Packaging).unwrap(),
            serde_json::json!("packaging"),
        );
        assert_eq!(
            serde_json::to_value(EdgeKind::Semantic).unwrap(),
            serde_json::json!("semantic"),
        );
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
        fn execute(
            &self,
            _ctx: &ExecuteContext<'_>,
        ) -> Result<ExecutionRecord, ExecutionError> {
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
