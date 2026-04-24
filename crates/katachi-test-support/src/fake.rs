//! A test-only harness that runs a fixed shell script.
//!
//! `FakeHarness` implements [`HarnessModule`] end-to-end by generating an
//! execution plan that invokes `/bin/sh -c <script>` and delegating
//! execution to the shared [`katachi_core::execute`] runner. Tests use it
//! to exercise the plan → execute → record pipeline without depending on
//! any real harness binary.

use std::collections::BTreeMap;

use katachi_core::diagnostic::Diagnostic;
use katachi_core::error::{ExecutionError, PlanError, ResolveError};
use katachi_core::execute;
use katachi_core::harness::{
    ExecuteContext, ExplainContext, ExplainResult, HarnessModule, PlanContext, ResolveContext,
    RosterCatalog, ScanContext,
};
use katachi_core::model::{BackendKind, HarnessKind};
use katachi_core::plan::{
    ExecutionBackendPlan, ExecutionPlan, MaterializationPlan, ResolvedKatachi, RunProfile,
    TranscriptMode, PLAN_SCHEMA_VERSION,
};
use katachi_core::record::ExecutionRecord;

#[derive(Clone, Debug)]
pub struct FakeHarness {
    script: String,
    transcript_mode: TranscriptMode,
    kind: HarnessKind,
}

impl FakeHarness {
    /// Build a harness that runs `script` through `/bin/sh -c`.
    pub fn new(script: impl Into<String>) -> Self {
        Self {
            script: script.into(),
            transcript_mode: TranscriptMode::RawOnly,
            kind: HarnessKind::Claude,
        }
    }

    pub fn with_kind(mut self, kind: HarnessKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn with_transcript_mode(mut self, mode: TranscriptMode) -> Self {
        self.transcript_mode = mode;
        self
    }

    pub fn script(&self) -> &str {
        &self.script
    }
}

impl HarnessModule for FakeHarness {
    fn kind(&self) -> HarnessKind {
        self.kind
    }

    fn scan(&self, _ctx: &ScanContext<'_>) -> Result<RosterCatalog, ResolveError> {
        Ok(RosterCatalog::empty(self.kind))
    }

    fn explain(&self, ctx: &ExplainContext<'_>) -> Result<ExplainResult, ResolveError> {
        Ok(ExplainResult {
            item: ctx.item.clone(),
            summary: format!("fake item {}", ctx.item),
            sections: Vec::new(),
        })
    }

    fn resolve(&self, ctx: &ResolveContext<'_>) -> Result<ResolvedKatachi, ResolveError> {
        Ok(ResolvedKatachi {
            katachi_id: ctx.request.katachi_id.clone(),
            harness: self.kind,
            backend: BackendKind::Cli,
            selected_items: Vec::new(),
            run_profile: RunProfile::default(),
            diagnostics: Vec::<Diagnostic>::new(),
        })
    }

    fn plan(&self, ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
        Ok(ExecutionPlan {
            schema_version: PLAN_SCHEMA_VERSION,
            run_id: ctx.run_id,
            summary: format!("fake harness: {}", truncate(&self.script, 60)),
            harness: self.kind,
            backend: BackendKind::Cli,
            materialization: MaterializationPlan::ambient(),
            execution: ExecutionBackendPlan {
                backend: BackendKind::Cli,
                argv: vec!["/bin/sh".into(), "-c".into(), self.script.clone()],
                stdin_input: None,
                env: BTreeMap::new(),
                cwd: None,
                timeout_secs: None,
            },
            transcript_mode: self.transcript_mode,
        })
    }

    fn execute(&self, ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
        execute::run(ctx)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}
