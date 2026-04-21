//! A test-only harness backed by a hand-authored `RosterCatalog`.
//!
//! `FixtureHarness` satisfies [`HarnessModule`] by returning a pre-built
//! catalog from `scan()`. The Phase-2 resolver drives it against
//! [`KatachiDefinition`]s to exercise closure, ambiguity, and validation
//! without any real filesystem discovery.

use katachi_core::error::{ExecutionError, PlanError, ResolveError};
use katachi_core::harness::{
    DependencyEdge, DiscoveredItem, EdgeKind, ExecuteContext, ExplainContext, ExplainResult,
    HarnessModule, ItemSource, PackageRef, PlanContext, ResolveContext, RosterCatalog, ScanContext,
};
use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::plan::{ExecutionPlan, ResolvedKatachi};
use katachi_core::record::ExecutionRecord;

#[derive(Debug, Clone)]
pub struct FixtureHarness {
    kind: HarnessKind,
    catalog: RosterCatalog,
}

impl FixtureHarness {
    pub fn builder(kind: HarnessKind) -> FixtureHarnessBuilder {
        FixtureHarnessBuilder::new(kind)
    }

    pub fn catalog(&self) -> &RosterCatalog {
        &self.catalog
    }

    /// A toy Claude roster: a plugin that packages two skills, with a
    /// semantic edge from the first skill to an agent.
    ///
    /// Graph (packaging = `--`, semantic = `~~`):
    ///   plugin:web-a11y --> skill:axe-runner
    ///   plugin:web-a11y --> skill:axe-reporter
    ///   skill:axe-runner ~~> agent:reviewer
    ///   skill:bystander (standalone, unrelated)
    pub fn toy_claude() -> Self {
        let plugin = item(HarnessKind::Claude, "plugin", "web-a11y");
        let mut axe_runner = item(HarnessKind::Claude, "skill", "axe-runner");
        axe_runner.packaging = Some(PackageRef {
            item_ref: plugin.item_ref.clone(),
            required: true,
        });
        axe_runner.capabilities.push("a11y".into());
        let mut axe_reporter = item(HarnessKind::Claude, "skill", "axe-reporter");
        axe_reporter.packaging = Some(PackageRef {
            item_ref: plugin.item_ref.clone(),
            required: true,
        });
        let reviewer = item(HarnessKind::Claude, "agent", "reviewer");
        let bystander = item(HarnessKind::Claude, "skill", "bystander");

        Self::builder(HarnessKind::Claude)
            .item(plugin.clone())
            .item(axe_runner.clone())
            .item(axe_reporter.clone())
            .item(reviewer.clone())
            .item(bystander)
            .edge(edge(
                &plugin.item_ref,
                &axe_runner.item_ref,
                EdgeKind::Packaging,
                true,
            ))
            .edge(edge(
                &plugin.item_ref,
                &axe_reporter.item_ref,
                EdgeKind::Packaging,
                true,
            ))
            .edge(edge(
                &axe_runner.item_ref,
                &reviewer.item_ref,
                EdgeKind::Semantic,
                false,
            ))
            .build()
    }

    /// A deliberately cyclic roster: two skills reference each other.
    pub fn toy_with_cycle() -> Self {
        let a = item(HarnessKind::Claude, "skill", "a");
        let b = item(HarnessKind::Claude, "skill", "b");
        Self::builder(HarnessKind::Claude)
            .item(a.clone())
            .item(b.clone())
            .edge(edge(&a.item_ref, &b.item_ref, EdgeKind::Semantic, false))
            .edge(edge(&b.item_ref, &a.item_ref, EdgeKind::Semantic, false))
            .build()
    }
}

impl HarnessModule for FixtureHarness {
    fn kind(&self) -> HarnessKind {
        self.kind
    }
    fn scan(&self, _ctx: &ScanContext<'_>) -> Result<RosterCatalog, ResolveError> {
        Ok(self.catalog.clone())
    }
    fn explain(&self, ctx: &ExplainContext<'_>) -> Result<ExplainResult, ResolveError> {
        let item = self
            .catalog
            .get(ctx.item)
            .ok_or_else(|| ResolveError::UnknownItem {
                item: ctx.item.to_string(),
            })?;
        Ok(ExplainResult {
            item: ctx.item.clone(),
            summary: item.display_name.clone(),
            sections: Vec::new(),
        })
    }
    fn resolve(&self, _ctx: &ResolveContext<'_>) -> Result<ResolvedKatachi, ResolveError> {
        Err(ResolveError::UnknownKatachi {
            id: "fixture-does-not-implement-resolve".into(),
        })
    }
    fn plan(&self, _ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
        Err(PlanError::BuildFailed {
            message: "fixture harness does not implement plan".into(),
        })
    }
    fn execute(&self, _ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
        Err(ExecutionError::NonZeroExit {
            command: "fixture".into(),
            status: "unimplemented".into(),
        })
    }
}

/// Builder for hand-authored fixture catalogs.
pub struct FixtureHarnessBuilder {
    kind: HarnessKind,
    catalog: RosterCatalog,
}

impl FixtureHarnessBuilder {
    pub fn new(kind: HarnessKind) -> Self {
        Self {
            kind,
            catalog: RosterCatalog::empty(kind),
        }
    }

    pub fn item(mut self, item: DiscoveredItem) -> Self {
        self.catalog
            .insert_item(item)
            .expect("fixture item must be unique");
        self
    }

    pub fn edge(mut self, edge: DependencyEdge) -> Self {
        self.catalog
            .insert_edge(edge)
            .expect("fixture edge endpoints must exist");
        self
    }

    pub fn build(self) -> FixtureHarness {
        FixtureHarness {
            kind: self.kind,
            catalog: self.catalog,
        }
    }
}

fn item(harness: HarnessKind, kind: &str, id: &str) -> DiscoveredItem {
    DiscoveredItem {
        item_ref: ItemRef::new(harness, kind, id),
        display_name: id.to_string(),
        source: ItemSource::default(),
        packaging: None,
        raw: serde_json::Value::Null,
        capabilities: Vec::new(),
        constraints: Vec::new(),
    }
}

fn edge(from: &ItemRef, to: &ItemRef, kind: EdgeKind, required: bool) -> DependencyEdge {
    DependencyEdge {
        from: from.clone(),
        to: to.clone(),
        kind,
        required,
        note: None,
    }
}
