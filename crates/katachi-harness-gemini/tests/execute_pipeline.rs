//! Integration tests for the full plan → execute → record pipeline,
//! using a fake `gemini` shell script as the backend.

use std::fs;
use std::path::PathBuf;

use camino::Utf8PathBuf;
use tempfile::TempDir;

use katachi_core::diagnostic::Diagnostic;
use katachi_core::harness::{ExecuteContext, HarnessModule, PlanContext};
use katachi_core::model::{BackendKind, HarnessKind, ItemRef};
use katachi_core::persist::RunDirectory;
use katachi_core::plan::{
    ActionRequest, InvocationRequest, ResolvedItemRef, ResolvedKatachi, RunProfile,
    SelectionReason,
};
use katachi_core::record::{Outcome, RunId};
use katachi_harness_gemini::plan::{build_cli_plan_with_overlay, GeminiRunProfile};
use katachi_harness_gemini::GeminiHarness;

fn write_fake_gemini(dir: &std::path::Path) -> Utf8PathBuf {
    let script = dir.join("fake-gemini.sh");
    fs::write(
        &script,
        r#"#!/usr/bin/env bash
set -euo pipefail
echo '{"type": "init", "model": "gemini-3-pro"}'
echo '{"type": "message", "role": "assistant", "text": "hello from fake gemini"}'
echo '{"type": "tool_use", "name": "grep", "input": {"pattern": "foo"}}'
echo '{"type": "tool_result", "name": "grep", "output": "ok"}'
echo '{"type": "result", "summary": "all done", "outcome": "success"}'
"#,
    )
    .unwrap();
    let mut perms = fs::metadata(&script).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    fs::set_permissions(&script, perms).unwrap();
    Utf8PathBuf::from_path_buf(script).unwrap()
}

fn resolved_for_test(binary: &Utf8PathBuf) -> ResolvedKatachi {
    ResolvedKatachi {
        katachi_id: "smoke".into(),
        harness: HarnessKind::Gemini,
        backend: BackendKind::Cli,
        selected_items: vec![ResolvedItemRef {
            item: ItemRef::new(HarnessKind::Gemini, "skill", "placeholder"),
            reason: SelectionReason::Direct,
            pulled_in_by: None,
        }],
        run_profile: RunProfile {
            backend: Some(BackendKind::Cli),
            extras: serde_json::json!({
                "binary": binary.as_str(),
                "output_format": "stream-json"
            }),
        },
        diagnostics: Vec::<Diagnostic>::new(),
    }
}

#[test]
fn end_to_end_execute_produces_record_and_sidecar() {
    let td = TempDir::new().unwrap();
    let bin = write_fake_gemini(td.path());
    let cwd = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();

    let runs_root = td.path().join("runs");
    fs::create_dir_all(&runs_root).unwrap();
    let runs_root_utf8 = Utf8PathBuf::from_path_buf(runs_root).unwrap();

    let mut req = InvocationRequest::new(
        "smoke",
        ActionRequest::Execute {
            prompt: "hi there".into(),
        },
        cwd.clone(),
    );
    req.materialization = katachi_core::model::MaterializationMode::Ambient;

    let resolved = resolved_for_test(&bin);
    let run_id = RunId::new();
    let profile = GeminiRunProfile::from_overlay(&resolved.run_profile.extras);
    let plan = build_cli_plan_with_overlay(
        &PlanContext {
            request: &req,
            resolved: &resolved,
            run_id,
        },
        &profile,
        Some("hi there"),
        &[],
        None,
    )
    .unwrap();

    // Make sure argv[0] is our fake binary.
    assert_eq!(plan.execution.argv[0], bin.as_str());

    let run_dir = RunDirectory::create(&runs_root_utf8, run_id).unwrap();
    run_dir.write_request(&req).unwrap();
    run_dir.write_plan(&plan).unwrap();

    let harness = GeminiHarness::new();
    let exec_ctx = ExecuteContext {
        request: &req,
        plan: &plan,
        run_dir: &run_dir,
        started_at: time::OffsetDateTime::now_utc(),
    };
    let record = harness.execute(&exec_ctx).unwrap();
    run_dir.write_manifest().unwrap();
    let committed: PathBuf = run_dir.commit().unwrap().into_std_path_buf();

    assert_eq!(record.result.outcome, Outcome::Success);

    // The shared executor produces transcript.jsonl.
    let transcript = committed.join("transcript.jsonl");
    assert!(transcript.exists(), "transcript should be written");
    let raw = fs::read_to_string(&transcript).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert!(lines.len() >= 6, "should have at least 6 events, got: {raw}");

    // Gemini sidecar should also be present because transcript_mode was
    // JsonStream.
    let sidecar = committed.join("transcript.gemini.jsonl");
    assert!(
        sidecar.exists(),
        "gemini sidecar should be written for stream-json runs"
    );
    let sidecar_raw = fs::read_to_string(&sidecar).unwrap();
    assert!(
        sidecar_raw.contains("assistant_message") || sidecar_raw.contains("tool_use"),
        "sidecar should contain Gemini-projected events: {sidecar_raw}"
    );
}

#[test]
fn raw_only_plan_has_no_sidecar() {
    let td = TempDir::new().unwrap();
    let bin = write_fake_gemini(td.path());
    let cwd = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();

    let runs_root = td.path().join("runs");
    fs::create_dir_all(&runs_root).unwrap();
    let runs_root_utf8 = Utf8PathBuf::from_path_buf(runs_root).unwrap();

    let mut req = InvocationRequest::new(
        "smoke",
        ActionRequest::Execute {
            prompt: "hi".into(),
        },
        cwd.clone(),
    );
    req.materialization = katachi_core::model::MaterializationMode::Ambient;

    // Request raw output format so transcript_mode = RawOnly.
    let resolved = ResolvedKatachi {
        katachi_id: "smoke".into(),
        harness: HarnessKind::Gemini,
        backend: BackendKind::Cli,
        selected_items: Vec::new(),
        run_profile: RunProfile {
            backend: Some(BackendKind::Cli),
            extras: serde_json::json!({
                "binary": bin.as_str(),
                "output_format": "text"
            }),
        },
        diagnostics: Vec::<Diagnostic>::new(),
    };
    let run_id = RunId::new();
    let profile = GeminiRunProfile::from_overlay(&resolved.run_profile.extras);
    let plan = build_cli_plan_with_overlay(
        &PlanContext {
            request: &req,
            resolved: &resolved,
            run_id,
        },
        &profile,
        Some("hi"),
        &[],
        None,
    )
    .unwrap();
    assert_eq!(plan.transcript_mode, katachi_core::plan::TranscriptMode::RawOnly);

    let run_dir = RunDirectory::create(&runs_root_utf8, run_id).unwrap();
    let harness = GeminiHarness::new();
    let exec_ctx = ExecuteContext {
        request: &req,
        plan: &plan,
        run_dir: &run_dir,
        started_at: time::OffsetDateTime::now_utc(),
    };
    let _ = harness.execute(&exec_ctx).unwrap();
    run_dir.write_manifest().unwrap();
    let committed = run_dir.commit().unwrap();

    assert!(!committed.join("transcript.gemini.jsonl").exists());
}
