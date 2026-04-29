//! End-to-end tests for `katachi harness claude ...` against a hermetic
//! tempdir fixture. Exercises scan/plan/execute/doctor/dump-roster/
//! project using a fake `claude` binary so no real install is required.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn katachi_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_katachi"))
}

struct Fixture {
    _td: TempDir,
    root: PathBuf,
    config_path: PathBuf,
    data_root: PathBuf,
    fake_claude: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let td = TempDir::new().unwrap();
        let root = td.path().to_path_buf();

        // Loose project artifacts.
        write(
            &root,
            ".claude/skills/greeter/SKILL.md",
            "---\nname: greeter\ndescription: say hi\nagent: reviewer\n---\nbody",
        );
        write(
            &root,
            ".claude/agents/reviewer.md",
            "---\nname: reviewer\ndescription: reviewer agent\nmodel: sonnet\n---\n",
        );
        write(&root, "CLAUDE.md", "project level instructions");

        // Roster.
        write(
            &root,
            "data/rosters/claude/greet.toml",
            r#"
version = 1
id = "greet"
description = "greet demo"

[selection]
skills = ["greeter"]

[run_profile]
backend = "cli"
model = "sonnet"
"#,
        );

        // Fake claude binary.
        let fake_claude = root.join("fake-claude");
        fs::write(
            &fake_claude,
            r#"#!/bin/sh
echo '{"type":"assistant","text":"ack"}'
echo '{"type":"result","subtype":"success"}'
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&fake_claude).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_claude, perms).unwrap();

        // Config: point at the fake claude binary and block ambient user roots.
        let config_path = root.join("config.toml");
        let nonexistent_user_root = root.join("no-user-root");
        let body = format!(
            r#"version = 1

[harnesses.claude]
enabled = true
binary = "{}"
plugin_roots = []
user_root = "{}"
project_roots = ["."]
"#,
            fake_claude.display(),
            nonexistent_user_root.display()
        );
        fs::write(&config_path, body).unwrap();

        let data_root = root.join("data");
        Self {
            _td: td,
            root,
            config_path,
            data_root,
            fake_claude,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = katachi_bin();
        cmd.env_remove("RUST_LOG")
            .env_remove("KATACHI_FIXTURE_HARNESSES")
            .env("KATACHI_CONFIG", &self.config_path)
            .env("KATACHI_DATA", &self.data_root)
            .env("KATACHI_CACHE", self.data_root.join("cache"))
            .args([
                "--cwd",
                self.root.to_str().unwrap(),
            ])
            .args(args);
        cmd.output().unwrap()
    }
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn assert_success(out: &std::process::Output, label: &str) {
    assert!(
        out.status.success(),
        "{label}: exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn scan_finds_loose_skill_and_agent() {
    let fx = Fixture::new();
    let out = fx.run(&["harness", "claude", "scan"]);
    assert_success(&out, "scan");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("greeter"), "scan missing skill: {stdout}");
    assert!(stdout.contains("reviewer"), "scan missing agent: {stdout}");
}

#[test]
fn doctor_reports_binary_resolution() {
    let fx = Fixture::new();
    let out = fx.run(&["harness", "claude", "doctor"]);
    assert_success(&out, "doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(fx.fake_claude.to_str().unwrap()));
}

#[test]
fn plan_renders_full_argv() {
    let fx = Fixture::new();
    let out = fx.run(&[
        "harness", "claude", "plan", "greet", "execute", "hello",
    ]);
    assert_success(&out, "plan");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--print"));
    assert!(stdout.contains("stream-json"));
    assert!(stdout.contains("--model"));
    assert!(stdout.contains("hello"));
}

#[test]
fn execute_runs_fake_claude_and_records_transcript() {
    let fx = Fixture::new();
    let out = fx.run(&["harness", "claude", "execute", "greet", "hi"]);
    assert_success(&out, "execute");

    let runs = fs::read_dir(fx.data_root.join("runs")).unwrap();
    let count = runs.count();
    assert!(count >= 1, "expected at least one run dir");
}

#[test]
fn dump_roster_emits_json() {
    let fx = Fixture::new();
    let out = fx.run(&["harness", "claude", "dump-roster", "greet"]);
    assert_success(&out, "dump-roster");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"roster\""));
    assert!(stdout.contains("\"greet\""));
}

#[test]
fn project_ts_emits_sdk_runner() {
    let fx = Fixture::new();
    let out = fx.run(&[
        "harness", "claude", "project", "greet", "--sdk", "ts",
    ]);
    assert_success(&out, "project");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("@anthropic-ai/claude-agent-sdk"));
    assert!(stdout.contains("model: \"sonnet\""));
}

#[test]
fn unknown_roster_id_exits_resolve() {
    let fx = Fixture::new();
    let out = fx.run(&[
        "harness", "claude", "plan", "ghost", "execute", "prompt",
    ]);
    assert_eq!(out.status.code(), Some(4), "expected Resolve exit code");
}
