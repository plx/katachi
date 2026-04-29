//! Gemini CLI planner.
//!
//! Turns a resolved katachi into a concrete [`ExecutionPlan`] invoking the
//! Gemini CLI in headless mode.

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::Value;

use katachi_core::error::PlanError;
use katachi_core::harness::PlanContext;
use katachi_core::model::{BackendKind, ItemRef};
use katachi_core::plan::{
    ActionRequest, ExecutionBackendPlan, ExecutionPlan, FileSource, MaterializationPlan,
    MaterializedFile, TranscriptMode, PLAN_SCHEMA_VERSION,
};

use crate::item::GeminiItemKind;
use crate::materialize::OverlayManifest;

/// Default invocation argv and args Gemini CLI understands in headless
/// mode. Kept centrally so tests can assert on it.
pub const DEFAULT_OUTPUT_FORMAT: &str = "stream-json";
pub const DEFAULT_APPROVAL_MODE: &str = "plan";

/// Normalized view of the Gemini run profile extracted from a target's
/// `run_profile_overlay`.
#[derive(Clone, Debug, Default)]
pub struct GeminiRunProfile {
    pub backend: Option<BackendKind>,
    pub model: Option<String>,
    pub approval_mode: Option<String>,
    pub output_format: Option<String>,
    pub include_directories: Vec<Utf8PathBuf>,
    pub extensions_mode: Option<ExtensionsMode>,
    pub extra_flags: Vec<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ExtensionsMode {
    /// Only use the selected extensions; implicitly disable others.
    SelectedOnly,
    /// Disable every extension.
    DisableAll,
    /// Use ambient extension discovery (unrestricted).
    Ambient,
}

impl ExtensionsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SelectedOnly => "selected-only",
            Self::DisableAll => "disable-all",
            Self::Ambient => "ambient",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "selected-only" => Some(Self::SelectedOnly),
            "disable-all" | "none" => Some(Self::DisableAll),
            "ambient" | "all" => Some(Self::Ambient),
            _ => None,
        }
    }
}

impl GeminiRunProfile {
    /// Extract a typed profile from the generic `extras` blob carried on a
    /// resolved katachi.
    pub fn from_overlay(extras: &Value) -> Self {
        let mut out = Self::default();
        if let Some(model) = extras.get("model").and_then(|v| v.as_str()) {
            out.model = Some(model.to_owned());
        }
        if let Some(mode) = extras.get("approval_mode").and_then(|v| v.as_str()) {
            out.approval_mode = Some(mode.to_owned());
        }
        if let Some(fmt) = extras.get("output_format").and_then(|v| v.as_str()) {
            out.output_format = Some(fmt.to_owned());
        }
        if let Some(arr) = extras.get("include_directories").and_then(|v| v.as_array()) {
            out.include_directories = arr
                .iter()
                .filter_map(|v| v.as_str().map(Utf8PathBuf::from))
                .collect();
        }
        if let Some(mode) = extras.get("extensions_mode").and_then(|v| v.as_str()) {
            out.extensions_mode = ExtensionsMode::parse(mode);
        }
        if let Some(arr) = extras.get("extra_flags").and_then(|v| v.as_array()) {
            out.extra_flags = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
        }
        out
    }

    pub fn output_format_or_default(&self) -> String {
        self.output_format
            .clone()
            .unwrap_or_else(|| DEFAULT_OUTPUT_FORMAT.to_string())
    }

    pub fn approval_mode_or_default(&self) -> String {
        self.approval_mode
            .clone()
            .unwrap_or_else(|| DEFAULT_APPROVAL_MODE.to_string())
    }
}

/// Build a Gemini execution plan from a resolved katachi.
pub fn build_plan(ctx: &PlanContext<'_>) -> Result<ExecutionPlan, PlanError> {
    if ctx.resolved.backend == BackendKind::SdkPy {
        return Err(PlanError::ProjectionLoss {
            backend: "sdk-py".into(),
            reason: "Gemini has no Python SDK".into(),
        });
    }

    let profile = GeminiRunProfile::from_overlay(&ctx.resolved.run_profile.extras);
    let prompt = action_prompt(&ctx.request.action);

    let selected_extensions: Vec<String> = ctx
        .resolved
        .selected_items
        .iter()
        .filter(|item| item.item.kind == GeminiItemKind::Extension.as_str())
        .map(|i| i.item.id.clone())
        .collect();

    match ctx.resolved.backend {
        BackendKind::Cli => build_cli_plan(ctx, &profile, prompt, &selected_extensions),
        BackendKind::SdkTs => build_sdk_ts_plan(ctx, &profile, prompt, &selected_extensions),
        other => Err(PlanError::ProjectionLoss {
            backend: other.to_string(),
            reason: "Gemini only supports cli and sdk-ts backends".into(),
        }),
    }
}

/// Read the binary name from the run profile's `extras` (defaults to
/// `gemini`). Surfaces as `binary` in the roster's `[run_profile]`
/// section so tests can point at a fake executable.
pub fn binary_name_from_overlay(extras: &Value) -> String {
    extras
        .get("binary")
        .and_then(|v| v.as_str())
        .unwrap_or("gemini")
        .to_owned()
}

fn build_cli_plan(
    ctx: &PlanContext<'_>,
    profile: &GeminiRunProfile,
    prompt: Option<&str>,
    selected_extensions: &[String],
) -> Result<ExecutionPlan, PlanError> {
    build_cli_plan_with_overlay(ctx, profile, prompt, selected_extensions, None)
}

/// Build a CLI plan optionally integrating a materialized overlay's
/// manifest (env overrides + recorded files).
pub fn build_cli_plan_with_overlay(
    ctx: &PlanContext<'_>,
    profile: &GeminiRunProfile,
    prompt: Option<&str>,
    selected_extensions: &[String],
    overlay: Option<&OverlayManifest>,
) -> Result<ExecutionPlan, PlanError> {
    let binary = binary_name_from_overlay(&ctx.resolved.run_profile.extras);
    let output_format = profile.output_format_or_default();
    let approval_mode = profile.approval_mode_or_default();

    let mut argv = vec![binary.clone()];

    // Common headless mode flags.
    argv.push("--output-format".into());
    argv.push(output_format.clone());

    argv.push("--approval-mode".into());
    argv.push(approval_mode.clone());

    if let Some(model) = &profile.model {
        argv.push("--model".into());
        argv.push(model.clone());
    }

    for dir in &profile.include_directories {
        argv.push("--include-directory".into());
        argv.push(dir.to_string());
    }

    let extensions_mode = profile
        .extensions_mode
        .unwrap_or(if selected_extensions.is_empty() {
            ExtensionsMode::Ambient
        } else {
            ExtensionsMode::SelectedOnly
        });
    match extensions_mode {
        ExtensionsMode::DisableAll => argv.push("--no-extensions".into()),
        ExtensionsMode::SelectedOnly => {
            for name in selected_extensions {
                argv.push("--extension".into());
                argv.push(name.clone());
            }
            // When selected set is empty, disable all to stay reproducible.
            if selected_extensions.is_empty() {
                argv.push("--no-extensions".into());
            }
        }
        ExtensionsMode::Ambient => {
            // Use whatever extensions the ambient install knows about.
        }
    }

    for flag in &profile.extra_flags {
        argv.push(flag.clone());
    }

    // Prompt is passed via `-p <prompt>` for Gemini headless.
    if let Some(p) = prompt {
        argv.push("-p".into());
        argv.push(p.to_owned());
    }

    let transcript_mode = match output_format.as_str() {
        "stream-json" | "json" => TranscriptMode::JsonStream,
        _ => TranscriptMode::RawOnly,
    };

    let materialization = match overlay {
        Some(m) => crate::materialize::to_materialization_plan(m),
        None => materialization_plan(ctx.request.materialization),
    };
    let cwd = overlay
        .map(|m| m.project_dir.clone())
        .unwrap_or_else(|| ctx.request.cwd.clone());
    let mut env = BTreeMap::new();
    if let Some(m) = overlay {
        for (k, v) in &m.env {
            env.insert(k.clone(), v.clone());
        }
    }
    let exec = ExecutionBackendPlan {
        backend: BackendKind::Cli,
        argv,
        stdin_input: None,
        env,
        cwd: Some(cwd),
        timeout_secs: None,
    };

    let summary = format!(
        "gemini headless ({} extensions, model={}, approval={})",
        selected_extensions.len(),
        profile.model.as_deref().unwrap_or("<default>"),
        approval_mode
    );

    Ok(ExecutionPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        run_id: ctx.run_id,
        summary,
        harness: ctx.resolved.harness,
        backend: BackendKind::Cli,
        materialization,
        execution: exec,
        transcript_mode,
    })
}

fn build_sdk_ts_plan(
    ctx: &PlanContext<'_>,
    profile: &GeminiRunProfile,
    prompt: Option<&str>,
    _selected_extensions: &[String],
) -> Result<ExecutionPlan, PlanError> {
    // Reject unsupported features. Error messages use the exact names
    // listed in the implementation spec so documentation stays stable.
    let unsupported: Vec<&str> = ctx
        .resolved
        .selected_items
        .iter()
        .filter_map(|i| match i.item.kind.as_str() {
            "extension" => Some("extensions"),
            "subagent" => Some("subagents"),
            "hook_set" => Some("hooks"),
            "policy_set" => Some("policies"),
            _ => None,
        })
        .collect();
    if !unsupported.is_empty() {
        let mut names: Vec<String> = unsupported.iter().map(|s| (*s).to_owned()).collect();
        names.sort();
        names.dedup();
        return Err(PlanError::ProjectionLoss {
            backend: "sdk-ts".into(),
            reason: format!(
                "Gemini TypeScript SDK does not support: {}",
                names.join(", ")
            ),
        });
    }

    // Supported selection kinds pass through. Surface a warning via the
    // plan summary if the selection is empty (an SDK-ts call with no
    // configured tools is legal but unusual).
    let prompt = prompt.unwrap_or("").to_owned();
    let model = profile
        .model
        .clone()
        .unwrap_or_else(|| "gemini-3-pro".to_owned());
    let skills: Vec<String> = ctx
        .resolved
        .selected_items
        .iter()
        .filter(|i| i.item.kind == "skill")
        .map(|i| i.item.id.clone())
        .collect();
    let contexts: Vec<String> = ctx
        .resolved
        .selected_items
        .iter()
        .filter(|i| i.item.kind == "context_source")
        .map(|i| i.item.id.clone())
        .collect();
    let payload = serde_json::json!({
        "sdk": "@google/gemini-cli-sdk",
        "model": model,
        "prompt": prompt,
        "cwd": ctx.request.cwd.as_str(),
        "include_directories": profile
            .include_directories
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>(),
        "skills": skills,
        "contexts": contexts,
    });
    let binary = binary_name_from_overlay(&ctx.resolved.run_profile.extras);
    // Use a configurable wrapper binary (defaults to `node`) so tests
    // can point at a fake entrypoint.
    let argv0 = if binary == "gemini" { "node".to_owned() } else { binary.clone() };
    let argv = vec![
        argv0,
        "-e".into(),
        format!(
            "require('@google/gemini-cli-sdk').run({});",
            serde_json::to_string(&payload).unwrap_or_default()
        ),
    ];
    Ok(ExecutionPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        run_id: ctx.run_id,
        summary: format!("gemini sdk-ts (model={model}, skills={})", skills.len()),
        harness: ctx.resolved.harness,
        backend: BackendKind::SdkTs,
        materialization: MaterializationPlan::ambient(),
        execution: ExecutionBackendPlan {
            backend: BackendKind::SdkTs,
            argv,
            stdin_input: None,
            env: BTreeMap::new(),
            cwd: Some(ctx.request.cwd.clone()),
            timeout_secs: None,
        },
        transcript_mode: TranscriptMode::JsonStream,
    })
}

fn materialization_plan(
    mode: katachi_core::model::MaterializationMode,
) -> MaterializationPlan {
    use katachi_core::model::MaterializationMode as MM;
    match mode {
        MM::Ambient => MaterializationPlan::ambient(),
        MM::TempOverlay => MaterializationPlan {
            mode: MM::TempOverlay,
            overlay_root: None,
            files: Vec::new(),
            env: BTreeMap::new(),
        },
    }
}

fn action_prompt(action: &ActionRequest) -> Option<&str> {
    match action {
        ActionRequest::Execute { prompt } | ActionRequest::Plan { prompt } => Some(prompt.as_str()),
        _ => None,
    }
}

/// Subset of the plan used by the overlay materializer to emit an
/// explicit manifest.
pub fn materialized_file_inline(dest: &Utf8Path, contents: &str) -> MaterializedFile {
    MaterializedFile {
        dest: dest.to_owned(),
        source: FileSource::Inline {
            contents: contents.to_owned(),
        },
    }
}

/// Helpers used by the temp overlay materializer.
pub fn selected_items_of_kind<'a>(
    ctx: &'a PlanContext<'_>,
    kind: GeminiItemKind,
) -> Vec<&'a ItemRef> {
    ctx.resolved
        .selected_items
        .iter()
        .filter(|i| i.item.kind == kind.as_str())
        .map(|i| &i.item)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use katachi_core::diagnostic::Diagnostic;
    use katachi_core::model::{HarnessKind, MaterializationMode};
    use katachi_core::plan::{
        ActionRequest, InvocationRequest, ResolvedItemRef, ResolvedKatachi, RunProfile,
        SelectionReason,
    };
    use katachi_core::record::RunId;
    use serde_json::json;

    fn resolved(
        backend: BackendKind,
        items: Vec<&str>,
        extras: Value,
    ) -> ResolvedKatachi {
        let selected_items = items
            .into_iter()
            .map(|s| {
                let (kind, id) = s.split_once(':').unwrap();
                ResolvedItemRef {
                    item: ItemRef::new(HarnessKind::Gemini, kind, id),
                    reason: SelectionReason::Direct,
                    pulled_in_by: None,
                }
            })
            .collect();
        ResolvedKatachi {
            katachi_id: "demo".into(),
            harness: HarnessKind::Gemini,
            backend,
            selected_items,
            run_profile: RunProfile {
                backend: Some(backend),
                extras,
            },
            diagnostics: Vec::<Diagnostic>::new(),
        }
    }

    fn request(prompt: &str) -> InvocationRequest {
        let mut req = InvocationRequest::new(
            "demo",
            ActionRequest::Execute {
                prompt: prompt.to_owned(),
            },
            Utf8PathBuf::from("/tmp"),
        );
        req.materialization = MaterializationMode::TempOverlay;
        req
    }

    #[test]
    fn cli_plan_includes_core_flags() {
        let res = resolved(
            BackendKind::Cli,
            vec!["extension:workspace-a11y"],
            json!({"model": "gemini-3-pro-preview", "approval_mode": "plan"}),
        );
        let req = request("do the thing");
        let ctx = PlanContext {
            request: &req,
            resolved: &res,
            run_id: RunId::new(),
        };
        let plan = build_plan(&ctx).unwrap();
        assert_eq!(plan.backend, BackendKind::Cli);
        assert!(plan
            .execution
            .argv
            .iter()
            .any(|a| a == "--output-format"));
        assert!(plan.execution.argv.iter().any(|a| a == "stream-json"));
        assert!(plan.execution.argv.iter().any(|a| a == "--approval-mode"));
        assert!(plan.execution.argv.iter().any(|a| a == "plan"));
        assert!(plan.execution.argv.iter().any(|a| a == "--model"));
        assert!(plan.execution.argv.iter().any(|a| a == "gemini-3-pro-preview"));
        // `--extension workspace-a11y`
        let pos = plan
            .execution
            .argv
            .iter()
            .position(|a| a == "--extension")
            .unwrap();
        assert_eq!(plan.execution.argv[pos + 1], "workspace-a11y");
        assert_eq!(plan.transcript_mode, TranscriptMode::JsonStream);
    }

    #[test]
    fn cli_plan_disables_all_extensions_when_mode_is_disable_all() {
        let res = resolved(
            BackendKind::Cli,
            vec!["extension:foo"],
            json!({"extensions_mode": "disable-all"}),
        );
        let req = request("x");
        let ctx = PlanContext {
            request: &req,
            resolved: &res,
            run_id: RunId::new(),
        };
        let plan = build_plan(&ctx).unwrap();
        assert!(plan.execution.argv.iter().any(|a| a == "--no-extensions"));
    }

    #[test]
    fn sdk_ts_rejects_extensions() {
        let res = resolved(
            BackendKind::SdkTs,
            vec!["extension:foo", "skill:bar"],
            json!({}),
        );
        let req = request("x");
        let ctx = PlanContext {
            request: &req,
            resolved: &res,
            run_id: RunId::new(),
        };
        let err = build_plan(&ctx).unwrap_err();
        match err {
            PlanError::ProjectionLoss { backend, reason } => {
                assert_eq!(backend, "sdk-ts");
                assert!(reason.contains("extensions"));
            }
            _ => panic!("expected ProjectionLoss"),
        }
    }

    #[test]
    fn sdk_ts_accepts_skill_only() {
        let res = resolved(BackendKind::SdkTs, vec!["skill:audit"], json!({}));
        let req = request("hello");
        let ctx = PlanContext {
            request: &req,
            resolved: &res,
            run_id: RunId::new(),
        };
        let plan = build_plan(&ctx).unwrap();
        assert_eq!(plan.backend, BackendKind::SdkTs);
        assert!(plan.summary.contains("skills=1"));
    }

    #[test]
    fn sdk_ts_accepts_context_and_settings_only() {
        let res = resolved(
            BackendKind::SdkTs,
            vec![
                "context_source:context:project:GEMINI.md",
                "settings_layer:settings:user",
            ],
            json!({"model": "gemini-3-pro"}),
        );
        let req = request("hello");
        let ctx = PlanContext {
            request: &req,
            resolved: &res,
            run_id: RunId::new(),
        };
        let plan = build_plan(&ctx).unwrap();
        assert_eq!(plan.backend, BackendKind::SdkTs);
    }

    #[test]
    fn sdk_ts_rejects_subagent_and_hook() {
        let cases = vec![
            vec!["subagent:explorer"],
            vec!["hook_set:a11y"],
            vec!["policy_set:readonly"],
        ];
        for items in cases {
            let res = resolved(BackendKind::SdkTs, items.clone(), json!({}));
            let req = request("x");
            let ctx = PlanContext {
                request: &req,
                resolved: &res,
                run_id: RunId::new(),
            };
            let err = build_plan(&ctx).unwrap_err();
            match err {
                PlanError::ProjectionLoss { backend, .. } => {
                    assert_eq!(backend, "sdk-ts", "case: {items:?}");
                }
                _ => panic!("wrong err variant"),
            }
        }
    }

    #[test]
    fn sdk_ts_empty_selection_is_still_legal() {
        let res = resolved(BackendKind::SdkTs, vec![], json!({}));
        let req = request("");
        let ctx = PlanContext {
            request: &req,
            resolved: &res,
            run_id: RunId::new(),
        };
        let plan = build_plan(&ctx).unwrap();
        assert!(plan.summary.contains("skills=0"));
    }

    #[test]
    fn sdk_py_rejected_immediately() {
        let res = resolved(BackendKind::SdkPy, vec![], json!({}));
        let req = request("x");
        let ctx = PlanContext {
            request: &req,
            resolved: &res,
            run_id: RunId::new(),
        };
        let err = build_plan(&ctx).unwrap_err();
        assert!(matches!(err, PlanError::ProjectionLoss { .. }));
    }

    #[test]
    fn run_profile_parses_from_overlay() {
        let extras = json!({
            "model": "m",
            "approval_mode": "yolo",
            "output_format": "text",
            "include_directories": ["docs", "apps/web"],
            "extensions_mode": "selected-only",
            "extra_flags": ["--verbose"]
        });
        let profile = GeminiRunProfile::from_overlay(&extras);
        assert_eq!(profile.model.as_deref(), Some("m"));
        assert_eq!(profile.approval_mode.as_deref(), Some("yolo"));
        assert_eq!(profile.output_format.as_deref(), Some("text"));
        assert_eq!(profile.include_directories.len(), 2);
        assert_eq!(profile.extensions_mode, Some(ExtensionsMode::SelectedOnly));
        assert_eq!(profile.extra_flags, vec!["--verbose".to_string()]);
    }
}
