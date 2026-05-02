//! Tests for `katachi run` and `katachi katachi` admin/read commands.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn katachi_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_katachi"))
}

#[allow(dead_code)]
fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

struct Fx {
    _td: TempDir,
    data_root: std::path::PathBuf,
    config_path: std::path::PathBuf,
}

impl Fx {
    fn new() -> Self {
        let td = TempDir::new().unwrap();
        let data_root = td.path().join("data");
        fs::create_dir_all(data_root.join("katachis")).unwrap();
        let config_path = td.path().join("config.toml");
        fs::write(&config_path, "version = 1\n").unwrap();
        Self {
            _td: td,
            data_root,
            config_path,
        }
    }

    fn write_katachi(&self, name: &str, body: &str) {
        let p = self.data_root.join("katachis").join(format!("{name}.toml"));
        fs::write(p, body).unwrap();
    }

    fn fake_run_dir(&self, run_id: &str, partial: bool) -> std::path::PathBuf {
        let runs = self.data_root.join("runs");
        fs::create_dir_all(&runs).unwrap();
        let name = if partial {
            format!("{run_id}.partial")
        } else {
            run_id.to_string()
        };
        let dir = runs.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = katachi_bin();
        cmd.env_remove("KATACHI_LOG")
            .env_remove("RUST_LOG")
            .env_remove("KATACHI_FIXTURE_HARNESSES")
            .env("KATACHI_DATA", &self.data_root)
            .env("KATACHI_CACHE", self.data_root.join("cache"))
            .env("KATACHI_CONFIG", &self.config_path)
            .args(args);
        cmd.output().unwrap()
    }
}

fn write_record(dir: &Path, run_id: &str, outcome: &str) {
    let body = format!(
        r#"{{
  "schema_version": 1,
  "run_id": "{run_id}",
  "started_at": "2026-04-01T00:00:00Z",
  "finished_at": "2026-04-01T00:00:01Z",
  "request": {{
    "schema_version": 1,
    "katachi_id": "demo",
    "action": {{ "type": "execute", "prompt": "hi" }},
    "cwd": "/tmp",
    "preferred_harnesses": [],
    "preferred_backends": [],
    "materialization": "temp_overlay",
    "dry_run": false
  }},
  "plan": {{
    "schema_version": 1,
    "run_id": "{run_id}",
    "summary": "echo hi",
    "harness": "claude",
    "backend": "cli",
    "materialization": {{ "mode": "ambient", "files": [], "env": {{}}, "overlay_root": null }},
    "execution": {{ "backend": "cli", "argv": ["echo", "hi"], "env": {{}}, "stdin_input": null, "cwd": null, "timeout_secs": null }},
    "transcript_mode": "raw_only"
  }},
  "events_count": 0,
  "result": {{ "outcome": "{outcome}", "exit_code": 0, "summary": "ok" }}
}}
"#
    );
    fs::write(dir.join("record.json"), body).unwrap();
}

fn write_transcript(dir: &Path) {
    let body = "{\"seq\":0,\"ts\":\"2026-04-01T00:00:00Z\",\"kind\":\"stdout_text\",\"text\":\"hi\\n\"}\n{\"seq\":1,\"ts\":\"2026-04-01T00:00:00Z\",\"kind\":\"stderr_text\",\"text\":\"warn\\n\"}\n";
    fs::write(dir.join("transcript.jsonl"), body).unwrap();
}

#[test]
fn run_list_empty() {
    let fx = Fx::new();
    let out = fx.run(&["run", "list"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("(no runs)"), "stdout: {stdout}");
}

#[test]
fn run_list_includes_committed_and_partial() {
    let fx = Fx::new();
    let id1 = "01900000-0000-7000-8000-000000000001";
    let id2 = "01900000-0000-7000-8000-000000000002";
    let dir1 = fx.fake_run_dir(id1, false);
    write_record(&dir1, id1, "success");
    let dir2 = fx.fake_run_dir(id2, true);
    write_record(&dir2, id2, "failure");
    write_transcript(&dir1);

    let out = fx.run(&["--json", "run", "list"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let runs = v["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    let states: Vec<&str> = runs.iter().map(|r| r["state"].as_str().unwrap()).collect();
    assert!(states.contains(&"committed"));
    assert!(states.contains(&"partial"));
}

#[test]
fn run_show_committed_returns_record_summary() {
    let fx = Fx::new();
    let id = "01900000-0000-7000-8000-000000000003";
    let dir = fx.fake_run_dir(id, false);
    write_record(&dir, id, "success");
    let out = fx.run(&["--json", "run", "show", id]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["state"], "committed");
    assert_eq!(v["record"]["result"]["outcome"], "success");
}

#[test]
fn run_show_unknown_returns_resolve() {
    let fx = Fx::new();
    let out = fx.run(&["run", "show", "no-such-id"]);
    assert_eq!(out.status.code(), Some(4));
}

#[test]
fn run_transcript_emits_parsed_events_in_json() {
    let fx = Fx::new();
    let id = "01900000-0000-7000-8000-000000000004";
    let dir = fx.fake_run_dir(id, false);
    write_record(&dir, id, "success");
    write_transcript(&dir);
    let out = fx.run(&["--json", "run", "transcript", id]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(v["state"], "committed");
    assert_eq!(events[0]["kind"], "stdout_text");
    assert_eq!(events[0]["ts"], "2026-04-01T00:00:00Z");
}

#[test]
fn run_transcript_human_uses_flat_event_schema_and_keeps_later_events() {
    let fx = Fx::new();
    let id = "01900000-0000-7000-8000-000000000014";
    let dir = fx.fake_run_dir(id, false);
    write_record(&dir, id, "success");
    fs::write(
        dir.join("transcript.jsonl"),
        "not json\n{\"seq\":2,\"ts\":\"2026-04-01T00:00:02Z\",\"kind\":\"assistant_message\",\"text\":\"hello\"}\n",
    )
    .unwrap();

    let out = fx.run(&["run", "transcript", id]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("2026-04-01T00:00:02Z"), "stdout: {stdout}");
    assert!(stdout.contains("assistant_message"), "stdout: {stdout}");
    assert!(stdout.contains("line 1:"), "stdout: {stdout}");
}

#[test]
fn katachi_list_empty_when_no_definitions() {
    let fx = Fx::new();
    let out = fx.run(&["katachi", "list"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("(no katachis)"));
}

#[test]
fn katachi_list_returns_definitions_sorted() {
    let fx = Fx::new();
    fx.write_katachi(
        "zeta",
        r#"
id = "zeta"
[[targets]]
harness = "claude"
"#,
    );
    fx.write_katachi(
        "alpha",
        r#"
id = "alpha"
[[targets]]
harness = "codex"
"#,
    );
    let out = fx.run(&["--json", "katachi", "list"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    let katachis = v["katachis"].as_array().unwrap();
    let ids: Vec<&str> = katachis.iter().map(|k| k["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["alpha", "zeta"]);
}

#[test]
fn katachi_show_existing_returns_definition() {
    let fx = Fx::new();
    fx.write_katachi(
        "demo",
        r#"
id = "demo"
description = "test"
[[targets]]
harness = "claude"
"#,
    );
    let out = fx.run(&["--json", "katachi", "show", "demo"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["id"], "demo");
    assert_eq!(v["description"], "test");
}

#[test]
fn katachi_show_unknown_returns_resolve() {
    let fx = Fx::new();
    let out = fx.run(&["katachi", "show", "no-such-id"]);
    assert_eq!(out.status.code(), Some(4));
}

#[test]
fn katachi_validate_unknown_selector_returns_resolve() {
    let fx = Fx::new();
    fx.write_katachi(
        "ghost",
        r#"
id = "ghost"
[[targets]]
harness = "claude"

[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "plugin", id = "does-not-exist" }
"#,
    );
    let out = fx.run(&["katachi", "validate", "ghost"]);
    let code = out.status.code().unwrap_or(-1);
    // Resolve error or no-enabled-harness. Either way, non-zero.
    assert!(
        code == 4 || code == 5,
        "expected ExitCode::Resolve (4) or Validate (5); got {code}; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
