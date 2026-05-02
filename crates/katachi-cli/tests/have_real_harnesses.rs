//! `katachi have ...` against real harness modules (no fixtures).
//!
//! These tests build temp project trees containing real harness-native
//! artifacts (Claude skills, etc.) and verify that `have describe` and
//! `have graph` resolve through the real registry without
//! `KATACHI_FIXTURE_HARNESSES`.

#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn katachi_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_katachi"))
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

struct ClaudeProject {
    _td: TempDir,
    root: std::path::PathBuf,
    config_path: std::path::PathBuf,
    data_root: std::path::PathBuf,
}

impl ClaudeProject {
    fn new() -> Self {
        let td = TempDir::new().unwrap();
        let root = td.path().to_path_buf();

        // Loose project skill referencing an agent.
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

        // Katachi definition selecting the project skill.
        let katachis = root.join("data/katachis");
        fs::create_dir_all(&katachis).unwrap();
        fs::write(
            katachis.join("greet.toml"),
            r#"
id = "greet"
description = "smoke test"

[[targets]]
harness = "claude"
backend = "cli"

[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "skill", id = "greeter" }
"#,
        )
        .unwrap();

        let nonexistent_user_root = root.join("no-user-root");
        let config_path = root.join("config.toml");
        let body = format!(
            r#"version = 1

[harnesses.claude]
enabled = true
binary = "fake-claude"
plugin_roots = []
user_root = "{}"
project_roots = ["."]

[harnesses.codex]
enabled = false

[harnesses.gemini]
enabled = false
"#,
            nonexistent_user_root.display()
        );
        fs::write(&config_path, body).unwrap();
        let data_root = root.join("data");

        Self {
            _td: td,
            root,
            config_path,
            data_root,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = katachi_bin();
        cmd.env_remove("RUST_LOG")
            .env_remove("KATACHI_FIXTURE_HARNESSES")
            .env("KATACHI_CONFIG", &self.config_path)
            .env("KATACHI_DATA", &self.data_root)
            .env("KATACHI_CACHE", self.data_root.join("cache"))
            .args(["--cwd", self.root.to_str().unwrap()])
            .args(args);
        cmd.output().unwrap()
    }
}

#[test]
fn have_describe_resolves_real_claude_skill() {
    let fx = ClaudeProject::new();
    let out = fx.run(&["have", "greet", "describe"]);
    assert!(
        out.status.success(),
        "expected success; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("greeter"),
        "stdout should mention selected skill: {stdout}"
    );
}

#[test]
fn have_describe_respects_disabled_harness() {
    // Build a config that disables claude, then point a katachi at it.
    let fx = ClaudeProject::new();
    fs::write(
        &fx.config_path,
        r#"version = 1

[harnesses.claude]
enabled = false

[harnesses.codex]
enabled = false

[harnesses.gemini]
enabled = false
"#,
    )
    .unwrap();
    let out = fx.run(&["have", "greet", "describe"]);
    let code = out.status.code().unwrap_or(-1);
    assert_eq!(
        code, 4,
        "expected ExitCode::Resolve when no enabled harness matches; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn have_graph_text_renders_selected_items_and_edges() {
    let fx = ClaudeProject::new();
    let out = fx.run(&["have", "greet", "graph", "--format", "text"]);
    assert!(
        out.status.success(),
        "expected success; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("katachi: greet"), "graph: {stdout}");
    assert!(stdout.contains("greeter"), "graph: {stdout}");
}

#[test]
fn have_graph_dot_emits_digraph_block() {
    let fx = ClaudeProject::new();
    let out = fx.run(&["have", "greet", "graph", "--format", "dot"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.starts_with("digraph "), "dot output: {stdout}");
    assert!(stdout.contains("rankdir=LR"));
}

#[test]
fn have_graph_json_is_parseable() {
    let fx = ClaudeProject::new();
    let out = fx.run(&["have", "greet", "graph", "--json"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(v["katachi_id"], "greet");
    assert_eq!(v["harness"], "claude");
    let items = v["items"].as_array().expect("items array");
    assert!(
        items.iter().any(|it| it["item_ref"]["id"] == "greeter"),
        "items missing greeter: {items:?}"
    );
}
