//! End-to-end tests for the fake harness against the shared executor.
//!
//! Walks the full plan → execute → record pipeline with `FakeHarness`,
//! verifying that both success and failure paths produce persisted run
//! directories with the expected file set.

use camino::Utf8PathBuf;
use katachi_core::config::KatachiConfig;
use katachi_core::harness::{
    ExecuteContext, HarnessModule, PlanContext, ResolveContext, RosterCatalog,
};
use katachi_core::persist::{
    RunDirectory, FILE_MANIFEST, FILE_PLAN, FILE_RECORD, FILE_REQUEST, FILE_STDERR, FILE_STDOUT,
    FILE_TRANSCRIPT,
};
use katachi_core::plan::{ActionRequest, InvocationRequest};
use katachi_core::record::{Outcome, RunId};
use katachi_test_support::FakeHarness;
use tempfile::TempDir;
use time::OffsetDateTime;

struct RunArtifacts {
    _tempdir: TempDir,
    final_path: Utf8PathBuf,
    record: katachi_core::record::ExecutionRecord,
}

fn drive(harness: &FakeHarness) -> RunArtifacts {
    let td = TempDir::new().unwrap();
    let runs_root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();

    let run_id = RunId::new();
    let request = InvocationRequest::new(
        "fake-demo",
        ActionRequest::Execute {
            prompt: "say hi".into(),
        },
        runs_root.clone(),
    );

    let catalog = RosterCatalog::empty(harness.kind());
    let config = KatachiConfig::default();

    let resolved = harness
        .resolve(&ResolveContext {
            request: &request,
            catalog: &catalog,
            config: &config,
        })
        .expect("resolve");

    let plan = harness
        .plan(&PlanContext {
            request: &request,
            resolved: &resolved,
            run_id,
        })
        .expect("plan");

    let run_dir = RunDirectory::create(&runs_root, run_id).expect("run dir");
    let final_path = run_dir.final_path().to_owned();
    run_dir.write_request(&request).expect("request.json");
    run_dir.write_plan(&plan).expect("plan.json");

    let exec_ctx = ExecuteContext {
        request: &request,
        plan: &plan,
        run_dir: &run_dir,
        started_at: OffsetDateTime::now_utc(),
    };
    let record = harness.execute(&exec_ctx).expect("execute");

    run_dir.write_manifest().expect("manifest");
    let committed = run_dir.commit().expect("commit");
    assert_eq!(committed, final_path);

    RunArtifacts {
        _tempdir: td,
        final_path,
        record,
    }
}

#[test]
fn success_path_produces_complete_run_directory() {
    let harness = FakeHarness::new("printf 'hello\\nworld\\n'");
    let art = drive(&harness);

    assert_eq!(art.record.result.outcome, Outcome::Success);
    assert_eq!(art.record.result.exit_code, Some(0));

    for expected in &[
        FILE_REQUEST,
        FILE_PLAN,
        FILE_RECORD,
        FILE_TRANSCRIPT,
        FILE_STDOUT,
        FILE_STDERR,
        FILE_MANIFEST,
    ] {
        assert!(
            art.final_path.join(expected).exists(),
            "expected {expected} to exist in committed run dir"
        );
    }

    let stdout = std::fs::read_to_string(art.final_path.join(FILE_STDOUT)).unwrap();
    assert!(
        stdout.contains("hello"),
        "stdout missing 'hello': {stdout:?}"
    );
    assert!(
        stdout.contains("world"),
        "stdout missing 'world': {stdout:?}"
    );

    let transcript = std::fs::read_to_string(art.final_path.join(FILE_TRANSCRIPT)).unwrap();
    assert!(transcript.contains("\"kind\":\"user_message\""));
    assert!(transcript.contains("\"kind\":\"stdout_text\""));
    assert!(transcript.contains("\"kind\":\"result\""));
    assert!(transcript.contains("\"outcome\":\"success\""));

    // events_count = user_message + 2 stdout_text + result = 4
    assert_eq!(art.record.events_count, 4);
}

#[test]
fn failure_path_records_failure_outcome() {
    let harness = FakeHarness::new("echo oops >&2; exit 2");
    let art = drive(&harness);

    assert_eq!(art.record.result.outcome, Outcome::Failure);
    assert_eq!(art.record.result.exit_code, Some(2));

    let stderr = std::fs::read_to_string(art.final_path.join(FILE_STDERR)).unwrap();
    assert!(stderr.contains("oops"));

    let transcript = std::fs::read_to_string(art.final_path.join(FILE_TRANSCRIPT)).unwrap();
    assert!(transcript.contains("\"kind\":\"stderr_text\""));
    assert!(transcript.contains("\"outcome\":\"failure\""));
}

/// Read and parse `plan.json` from a committed run directory.
fn read_json(path: &Utf8PathBuf) -> serde_json::Value {
    let raw = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn snapshot_success_plan_and_record() {
    let harness = FakeHarness::new("echo hi");
    let art = drive(&harness);
    assert_eq!(art.record.result.outcome, Outcome::Success);

    let plan = read_json(&art.final_path.join(FILE_PLAN));
    insta::assert_json_snapshot!("fake_success_plan", plan, {
        ".run_id" => "[uuid]",
    });

    let record = read_json(&art.final_path.join(FILE_RECORD));
    insta::assert_json_snapshot!("fake_success_record", record, {
        ".run_id" => "[uuid]",
        ".plan.run_id" => "[uuid]",
        ".started_at" => "[ts]",
        ".finished_at" => "[ts]",
        ".request.cwd" => "[tmpdir]",
    });
}

#[test]
fn snapshot_failure_plan_and_record() {
    let harness = FakeHarness::new("exit 7");
    let art = drive(&harness);
    assert_eq!(art.record.result.outcome, Outcome::Failure);
    assert_eq!(art.record.result.exit_code, Some(7));

    let plan = read_json(&art.final_path.join(FILE_PLAN));
    insta::assert_json_snapshot!("fake_failure_plan", plan, {
        ".run_id" => "[uuid]",
    });

    let record = read_json(&art.final_path.join(FILE_RECORD));
    insta::assert_json_snapshot!("fake_failure_record", record, {
        ".run_id" => "[uuid]",
        ".plan.run_id" => "[uuid]",
        ".started_at" => "[ts]",
        ".finished_at" => "[ts]",
        ".request.cwd" => "[tmpdir]",
    });
}
