//! Codex CLI planner.
//!
//! The planner assembles an [`ExecutionPlan`] that will invoke
//! `codex exec` non-interactively against a materialized `CODEX_HOME`.
//!
//! Inputs are the roster selection, effective config, and materialization
//! plan; outputs are:
//!
//! - an [`ExecutionBackendPlan`] with the full `argv`, environment, and
//!   working directory
//! - a [`MaterializationPlan`] pointing at the overlay that must exist
//!   before execution
//! - a [`TranscriptMode`] driven by the roster's `output_mode`
//!
//! The planner does **not** realize the overlay on disk — the executor
//! does that just before spawning the child process.

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::error::PlanError;
use katachi_core::harness::PlanContext;
use katachi_core::model::{BackendKind, HarnessKind, MaterializationMode};
use katachi_core::plan::{
    ActionRequest, ExecutionBackendPlan, ExecutionPlan, MaterializationPlan, TranscriptMode,
    PLAN_SCHEMA_VERSION,
};

use crate::cli_flags::{approval_policy as approval_flag, flags, sandbox_mode as sandbox_flag, OutputMode};
use crate::effective::EffectiveCodexConfig;
use crate::materialize::{plan_materialization, CodexMaterializationPlan, HOME_SUBDIR};
use crate::roster_file::CodexRosterFile;
use crate::CodexSettings;

/// Inputs the Codex planner needs beyond what `PlanContext` supplies.
///
/// The shared resolver runs before the planner, so harness plumbing has
/// already converted the katachi id into a concrete backend + item set.
/// The planner then needs:
///
/// - the roster file itself (for run-profile + resolution policy)
/// - the composed [`EffectiveCodexConfig`]
/// - Codex runtime settings (binary path, trust flag, etc.)
pub struct CodexPlanInputs<'a> {
    pub ctx: &'a PlanContext<'a>,
    pub roster: &'a CodexRosterFile,
    pub effective: &'a EffectiveCodexConfig,
    pub settings: &'a CodexSettings,
}

/// Shape of the planned invocation. Kept separate from [`ExecutionPlan`]
/// so callers like `effective-config` can print a meaningful dry-run
/// summary without requiring the full shared envelope.
#[derive(Debug, Clone)]
pub struct PlannedCodexRun {
    pub backend: BackendKind,
    pub command: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Utf8PathBuf,
    pub transcript_mode: TranscriptMode,
    pub materialization: CodexMaterializationPlan,
    pub overlay_root: Option<Utf8PathBuf>,
}

/// Build a [`PlannedCodexRun`] from effective + roster + settings.
///
/// The overlay root is not known at planning time; callers fill it in
/// once they've materialized the overlay and rewrite the env accordingly.
pub fn plan(inputs: &CodexPlanInputs<'_>) -> Result<PlannedCodexRun, PlanError> {
    let backend = parse_backend(inputs.roster.run_profile.backend.as_deref(), &inputs.settings.default_backend)?;
    if backend == BackendKind::SdkPy && !inputs.settings.enable_python_sdk {
        return Err(PlanError::ProjectionLoss {
            backend: backend.to_string(),
            reason: "Python SDK backend requires `enable_python_sdk = true` in harness config"
                .into(),
        });
    }

    let materialization = plan_materialization(inputs.effective);
    let transcript_mode = transcript_mode_for(inputs.effective);
    let cwd = planning_cwd(inputs)?;

    let command = match backend {
        BackendKind::Cli => build_cli_argv(inputs)?,
        BackendKind::SdkTs => build_sdk_ts_argv(inputs)?,
        BackendKind::SdkPy => build_sdk_py_argv(inputs)?,
        BackendKind::McpServer | BackendKind::AppServer => {
            return Err(PlanError::ProjectionLoss {
                backend: backend.to_string(),
                reason: "backend not supported by the codex harness prototype".into(),
            });
        }
    };

    let mut env = materialization.env.clone();
    // The executor will rewrite CODEX_HOME to the absolute overlay path
    // after materialization. We leave the relative placeholder here so
    // plan.json is deterministic.
    env.entry("CODEX_HOME".into()).or_insert(HOME_SUBDIR.into());

    Ok(PlannedCodexRun {
        backend,
        command,
        env,
        cwd,
        transcript_mode,
        materialization,
        overlay_root: None,
    })
}

/// Wrap [`plan`] into the shared [`ExecutionPlan`] envelope. This is what
/// [`crate::harness::CodexHarness::plan`] actually calls.
pub fn build_plan(ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
    // The shared resolver has stashed the roster file + effective config
    // in `resolved.run_profile.extras` as JSON. We deserialize here so
    // callers can drive the planner through the trait without having to
    // manage these types themselves.
    let bundle: PlannerBundle = serde_json::from_value(ctx.resolved.run_profile.extras.clone())
        .map_err(|e| PlanError::BuildFailed {
            message: format!("plan missing codex bundle in run_profile.extras: {e}"),
        })?;

    let inputs = CodexPlanInputs {
        ctx,
        roster: &bundle.roster,
        effective: &bundle.effective,
        settings: &bundle.settings,
    };

    let planned = plan(&inputs)?;

    let mode = match ctx.request.materialization {
        MaterializationMode::Ambient => MaterializationMode::Ambient,
        MaterializationMode::TempOverlay => MaterializationMode::TempOverlay,
    };
    let materialization = MaterializationPlan {
        mode,
        overlay_root: planned.overlay_root.clone(),
        files: planned.materialization.files.clone(),
        env: planned.materialization.env.clone(),
    };
    let execution = ExecutionBackendPlan {
        backend: planned.backend,
        argv: planned.command.clone(),
        stdin_input: None,
        env: planned.env.clone(),
        cwd: Some(planned.cwd.clone()),
        timeout_secs: bundle.roster.run_profile.timeout_secs,
    };
    let summary = summarize(&planned);
    Ok(ExecutionPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        run_id: ctx.run_id,
        summary,
        harness: HarnessKind::Codex,
        backend: planned.backend,
        materialization,
        execution,
        transcript_mode: planned.transcript_mode,
    })
}

/// The payload the resolver stashes in `run_profile.extras`.
///
/// Kept here so the trait-based plan path has a single well-known shape
/// to consume. The CLI planner in `commands/harness_codex.rs` builds this
/// directly.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PlannerBundle {
    pub roster: CodexRosterFile,
    pub effective: EffectiveCodexConfig,
    pub settings: CodexSettings,
}

fn planning_cwd(inputs: &CodexPlanInputs<'_>) -> Result<Utf8PathBuf, PlanError> {
    // The runtime directs Codex at the materialized project via --cd; we
    // still record the *original* cwd as the working directory of the
    // child process so relative paths in prompts stay meaningful.
    Ok(inputs.ctx.request.cwd.clone())
}

fn transcript_mode_for(effective: &EffectiveCodexConfig) -> TranscriptMode {
    match effective
        .policy
        .output_mode
        .as_deref()
        .map(crate::cli_flags::output_mode)
    {
        Some(OutputMode::ExperimentalJson) => TranscriptMode::JsonStream,
        _ => TranscriptMode::RawOnly,
    }
}

fn parse_backend(
    roster_backend: Option<&str>,
    default: &str,
) -> Result<BackendKind, PlanError> {
    let raw = roster_backend.unwrap_or(default);
    raw.parse::<BackendKind>().map_err(|_| PlanError::BuildFailed {
        message: format!("unknown backend `{raw}` for codex"),
    })
}

fn build_cli_argv(inputs: &CodexPlanInputs<'_>) -> Result<Vec<String>, PlanError> {
    let mut argv: Vec<String> = vec![
        inputs.settings.binary.clone(),
        flags::EXEC.into(),
        flags::SKIP_GIT_REPO_CHECK.into(),
    ];

    // Output mode (machine-readable -> experimental JSON stream).
    let transcript_mode = transcript_mode_for(inputs.effective);
    if transcript_mode == TranscriptMode::JsonStream {
        argv.push(flags::EXPERIMENTAL_JSON.into());
    }

    // Approval policy.
    if let Some(raw) = &inputs.effective.policy.approval_policy {
        if let Some(canonical) = approval_flag(raw) {
            argv.push(flags::APPROVAL_POLICY.into());
            argv.push(canonical.into());
        } else {
            return Err(PlanError::BuildFailed {
                message: format!("unsupported approval policy `{raw}`"),
            });
        }
    }

    // Sandbox mode.
    if let Some(raw) = &inputs.effective.policy.sandbox_mode {
        if let Some(canonical) = sandbox_flag(raw) {
            argv.push(flags::SANDBOX.into());
            argv.push(canonical.into());
        } else {
            return Err(PlanError::BuildFailed {
                message: format!("unsupported sandbox mode `{raw}`"),
            });
        }
    }

    // Model override.
    if let Some(model) = &inputs.effective.policy.model {
        argv.push(flags::MODEL.into());
        argv.push(model.clone());
    }

    // Profile selection.
    if let Some(profile) = &inputs.effective.policy.profile {
        argv.push(flags::PROFILE.into());
        argv.push(profile.clone());
    }

    // Structured-output schema.
    if let Some(schema) = &inputs.effective.policy.output_schema_file {
        argv.push(flags::OUTPUT_SCHEMA.into());
        argv.push(schema.to_string());
    }

    // Working directory points at the materialized project overlay.
    // The final path is plugged in by the executor; at planning time
    // we emit the relative overlay path as a deterministic placeholder.
    argv.push(flags::CD.into());
    argv.push(format!("./{}", crate::materialize::PROJECT_SUBDIR));

    // Finally, the prompt — or stdin indicator when the request has none.
    match &inputs.ctx.request.action {
        ActionRequest::Execute { prompt } | ActionRequest::Plan { prompt } => {
            argv.push(prompt.clone());
        }
        ActionRequest::Describe | ActionRequest::Graph => {
            // Describe/graph never call into the planner, but guard anyway.
            argv.push(String::new());
        }
    }

    Ok(argv)
}

fn build_sdk_ts_argv(inputs: &CodexPlanInputs<'_>) -> Result<Vec<String>, PlanError> {
    // Minimal shim: TypeScript SDK projections still need a materialized
    // `CODEX_HOME`. We project to a small node invocation that the
    // `katachi` user can customize; the concrete bridge can grow later.
    let prompt = match &inputs.ctx.request.action {
        ActionRequest::Execute { prompt } | ActionRequest::Plan { prompt } => prompt.clone(),
        _ => String::new(),
    };
    Ok(vec![
        "node".into(),
        "--eval".into(),
        format!(
            "require('@openai/codex-sdk').run({{ prompt: {}, cd: {} }})",
            json_string_literal(&prompt),
            json_string_literal(&format!("./{}", crate::materialize::PROJECT_SUBDIR))
        ),
    ])
}

fn build_sdk_py_argv(inputs: &CodexPlanInputs<'_>) -> Result<Vec<String>, PlanError> {
    let prompt = match &inputs.ctx.request.action {
        ActionRequest::Execute { prompt } | ActionRequest::Plan { prompt } => prompt.clone(),
        _ => String::new(),
    };
    Ok(vec![
        "python".into(),
        "-m".into(),
        "codex_sdk.run".into(),
        "--prompt".into(),
        prompt,
        "--cd".into(),
        format!("./{}", crate::materialize::PROJECT_SUBDIR),
    ])
}

fn json_string_literal(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

fn summarize(planned: &PlannedCodexRun) -> String {
    let approval = planned
        .env
        .get("CODEX_HOME")
        .cloned()
        .unwrap_or_else(|| "ambient".into());
    format!(
        "codex {} (home={approval}, argv={} args)",
        planned.backend,
        planned.command.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effective::{EffectiveCodexConfig, EffectivePolicy};
    use crate::roster_file::RunProfile as RosterRunProfile;
    use katachi_core::harness::PlanContext;
    use katachi_core::model::HarnessKind;
    use katachi_core::plan::{ActionRequest, InvocationRequest, ResolvedKatachi, RunProfile};
    use katachi_core::record::RunId;
    use std::collections::BTreeMap;

    fn minimal_effective(policy: EffectivePolicy) -> EffectiveCodexConfig {
        EffectiveCodexConfig {
            merged_config: serde_json::Value::Object(Default::default()),
            layer_order: Vec::new(),
            active_profile: None,
            instruction_chain: Vec::new(),
            hooks: Vec::new(),
            rules: Vec::new(),
            mcp_servers: BTreeMap::new(),
            skills: Vec::new(),
            agents: Vec::new(),
            policy,
            run_profile: RosterRunProfile::default(),
        }
    }

    fn request(prompt: &str) -> InvocationRequest {
        InvocationRequest::new(
            "k",
            ActionRequest::Execute {
                prompt: prompt.into(),
            },
            Utf8PathBuf::from("/tmp/project"),
        )
    }

    fn resolved() -> ResolvedKatachi {
        ResolvedKatachi {
            katachi_id: "k".into(),
            harness: HarnessKind::Codex,
            backend: BackendKind::Cli,
            selected_items: Vec::new(),
            run_profile: RunProfile::default(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn cli_argv_contains_expected_flags() {
        let policy = EffectivePolicy {
            approval_policy: Some("never".into()),
            sandbox_mode: Some("read-only".into()),
            model: Some("gpt-5.4".into()),
            profile: Some("review".into()),
            output_mode: Some("machine-readable".into()),
            output_schema_file: Some(Utf8PathBuf::from("schemas/x.json")),
            writable_dirs: Vec::new(),
        };
        let effective = minimal_effective(policy);
        let roster = CodexRosterFile {
            version: 1,
            id: "r".into(),
            description: None,
            selection: Default::default(),
            run_profile: RosterRunProfile::default(),
            resolution: Default::default(),
        };
        let settings = CodexSettings::default();
        let request = request("audit the repo");
        let resolved = resolved();
        let run_id = RunId::new();
        let ctx = PlanContext {
            request: &request,
            resolved: &resolved,
            run_id,
        };
        let inputs = CodexPlanInputs {
            ctx: &ctx,
            roster: &roster,
            effective: &effective,
            settings: &settings,
        };
        let plan = plan(&inputs).unwrap();
        let joined = plan.command.join(" ");
        assert!(joined.contains("codex"));
        assert!(joined.contains("exec"));
        assert!(joined.contains("--experimental-json"));
        assert!(joined.contains("--ask-for-approval never"));
        assert!(joined.contains("--sandbox read-only"));
        assert!(joined.contains("-m gpt-5.4"));
        assert!(joined.contains("--profile review"));
        assert!(joined.contains("--output-schema schemas/x.json"));
        assert!(joined.contains("--cd ./project"));
        assert!(plan.env.contains_key("CODEX_HOME"));
    }

    #[test]
    fn backend_sdk_py_rejected_when_disabled() {
        let effective = minimal_effective(EffectivePolicy::default());
        let mut roster = CodexRosterFile {
            version: 1,
            id: "r".into(),
            description: None,
            selection: Default::default(),
            run_profile: RosterRunProfile::default(),
            resolution: Default::default(),
        };
        roster.run_profile.backend = Some("sdk-py".into());
        let settings = CodexSettings {
            enable_python_sdk: false,
            ..CodexSettings::default()
        };
        let request = request("x");
        let resolved = resolved();
        let run_id = RunId::new();
        let ctx = PlanContext {
            request: &request,
            resolved: &resolved,
            run_id,
        };
        let inputs = CodexPlanInputs {
            ctx: &ctx,
            roster: &roster,
            effective: &effective,
            settings: &settings,
        };
        let err = plan(&inputs).unwrap_err();
        match err {
            PlanError::ProjectionLoss { backend, .. } => assert_eq!(backend, "sdk-py"),
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn build_plan_consumes_bundle() {
        let policy = EffectivePolicy {
            approval_policy: Some("never".into()),
            sandbox_mode: Some("read-only".into()),
            ..EffectivePolicy::default()
        };
        let effective = minimal_effective(policy);
        let roster = CodexRosterFile {
            version: 1,
            id: "r".into(),
            description: None,
            selection: Default::default(),
            run_profile: RosterRunProfile::default(),
            resolution: Default::default(),
        };
        let settings = CodexSettings::default();
        let bundle = PlannerBundle {
            roster: roster.clone(),
            effective: effective.clone(),
            settings: settings.clone(),
        };
        let mut resolved = resolved();
        resolved.run_profile.extras = serde_json::to_value(&bundle).unwrap();
        let request = request("go");
        let run_id = RunId::new();
        let ctx = PlanContext {
            request: &request,
            resolved: &resolved,
            run_id,
        };
        let plan = build_plan(&ctx).unwrap();
        assert_eq!(plan.backend, BackendKind::Cli);
        assert_eq!(plan.harness, HarnessKind::Codex);
        assert!(plan.execution.argv.contains(&"exec".to_string()));
    }
}

// Allow the Utf8Path import where it's only used for trait bounds later.
#[allow(dead_code)]
fn _pathbuf_from(p: &Utf8Path) -> Utf8PathBuf {
    p.to_path_buf()
}
