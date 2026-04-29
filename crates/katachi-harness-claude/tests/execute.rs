//! Integration tests for the execute path.
//!
//! Spawn a fake `claude` binary (a small shell script) and verify that
//! the Claude harness captures stdout/stderr, records a transcript, and
//! writes a sensible `record.json`.

#![cfg(unix)]

use std::fs;

use camino::Utf8PathBuf;
use katachi_core::harness::{ExecuteContext, HarnessModule};
use katachi_core::model::{BackendKind, HarnessKind};
use katachi_core::persist::RunDirectory;
use katachi_core::plan::{
    ActionRequest, ExecutionBackendPlan, ExecutionPlan, InvocationRequest, MaterializationPlan,
    TranscriptMode, PLAN_SCHEMA_VERSION,
};
use katachi_core::record::RunId;
use katachi_harness_claude::ClaudeHarness;
use tempfile::TempDir;
use time::OffsetDateTime;

fn write_fake_claude(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    let path = dir.join("fake-claude");
    fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

fn minimal_plan(binary: &std::path::Path, run_id: RunId) -> ExecutionPlan {
    ExecutionPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        run_id,
        summary: "test claude".into(),
        harness: HarnessKind::Claude,
        backend: BackendKind::Cli,
        materialization: MaterializationPlan::ambient(),
        execution: ExecutionBackendPlan {
            backend: BackendKind::Cli,
            argv: vec![binary.to_string_lossy().into(), "--demo".into()],
            stdin_input: None,
            env: Default::default(),
            cwd: None,
            timeout_secs: Some(10),
        },
        transcript_mode: TranscriptMode::JsonStream,
    }
}

#[test]
fn execute_captures_stream_json_transcript() {
    let td = TempDir::new().unwrap();
    let script = r#"#!/bin/sh
echo '{"type":"assistant","text":"hi"}'
echo '{"type":"result","subtype":"success"}'
"#;
    let binary = write_fake_claude(td.path(), script);

    let runs_root = Utf8PathBuf::from_path_buf(td.path().to_path_buf())
        .unwrap()
        .join("runs");
    let run_id = RunId::new();
    let run_dir = RunDirectory::create(&runs_root, run_id).unwrap();

    let request = InvocationRequest::new(
        "roster",
        ActionRequest::Execute {
            prompt: "hello".into(),
        },
        Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap(),
    );
    let plan = minimal_plan(&binary, run_id);
    run_dir.write_request(&request).unwrap();
    run_dir.write_plan(&plan).unwrap();

    let harness = ClaudeHarness::new();
    let record = harness
        .execute(&ExecuteContext {
            request: &request,
            plan: &plan,
            run_dir: &run_dir,
            started_at: OffsetDateTime::now_utc(),
        })
        .unwrap();

    assert_eq!(
        record.result.outcome,
        katachi_core::record::Outcome::Success
    );
    assert!(record.events_count >= 2);

    let transcript_path = run_dir.partial_path().join("transcript.jsonl");
    let transcript = fs::read_to_string(transcript_path).unwrap();
    let kinds: Vec<_> = transcript
        .lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["kind"].as_str().unwrap().to_string()
        })
        .collect();
    // user_message (echoed prompt), assistant_message (normalized),
    // result (normalized "success"), and a final terminal "result" from
    // the executor itself.
    assert!(kinds.iter().any(|k| k == "user_message"));
    assert!(kinds.iter().any(|k| k == "assistant_message"));
    assert!(kinds.iter().any(|k| k == "result"));
}

#[test]
fn execute_failure_records_non_zero_exit() {
    let td = TempDir::new().unwrap();
    let script = r#"#!/bin/sh
echo "boom" >&2
exit 7
"#;
    let binary = write_fake_claude(td.path(), script);

    let runs_root = Utf8PathBuf::from_path_buf(td.path().to_path_buf())
        .unwrap()
        .join("runs");
    let run_id = RunId::new();
    let run_dir = RunDirectory::create(&runs_root, run_id).unwrap();

    let request = InvocationRequest::new(
        "roster",
        ActionRequest::Execute {
            prompt: "hi".into(),
        },
        Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap(),
    );
    let plan = minimal_plan(&binary, run_id);
    run_dir.write_request(&request).unwrap();
    run_dir.write_plan(&plan).unwrap();

    let harness = ClaudeHarness::new();
    let record = harness
        .execute(&ExecuteContext {
            request: &request,
            plan: &plan,
            run_dir: &run_dir,
            started_at: OffsetDateTime::now_utc(),
        })
        .unwrap();

    assert_eq!(
        record.result.outcome,
        katachi_core::record::Outcome::Failure
    );
    assert_eq!(record.result.exit_code, Some(7));
    let stderr = fs::read_to_string(run_dir.partial_path().join("stderr.log")).unwrap();
    assert!(stderr.contains("boom"));
}
