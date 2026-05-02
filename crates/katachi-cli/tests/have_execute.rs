//! Cross-harness `have plan execute` and `have execute` tests using a
//! `roster_id`-backed katachi against the real Claude harness with a
//! fake binary.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
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

struct Fx {
    _td: TempDir,
    root: std::path::PathBuf,
    config_path: std::path::PathBuf,
    data_root: std::path::PathBuf,
}

impl Fx {
    fn new() -> Self {
        let td = TempDir::new().unwrap();
        let root = td.path().to_path_buf();

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

        // Claude roster `greet` selects the project skill.
        let claude_roster = root.join("data/rosters/claude/greet.toml");
        fs::create_dir_all(claude_roster.parent().unwrap()).unwrap();
        fs::write(
            &claude_roster,
            r#"
version = 1
id = "greet"

[selection]
skills = ["greeter"]

[run_profile]
backend = "cli"
model = "sonnet"
"#,
        )
        .unwrap();

        // Katachi pointing at the roster_id.
        let katachis = root.join("data/katachis");
        fs::create_dir_all(&katachis).unwrap();
        fs::write(
            katachis.join("greet.toml"),
            r#"
id = "greet"

[[targets]]
harness = "claude"
roster_id = "greet"
"#,
        )
        .unwrap();

        let nonexistent_user_root = root.join("no-user-root");
        let config_path = root.join("config.toml");
        let body = format!(
            r#"version = 1

[harnesses.claude]
enabled = true
binary = "{}"
plugin_roots = []
user_root = "{}"
project_roots = ["."]

[harnesses.codex]
enabled = false

[harnesses.gemini]
enabled = false
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
fn have_plan_execute_renders_plan_for_claude_roster() {
    let fx = Fx::new();
    let out = fx.run(&["have", "greet", "plan", "execute", "hi"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("--print"), "plan should include claude argv: {stdout}");
    assert!(stdout.contains("hi"));
}

#[test]
fn have_execute_runs_fake_claude_and_persists_run() {
    let fx = Fx::new();
    let out = fx.run(&["have", "greet", "execute", "hi"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runs = fs::read_dir(fx.data_root.join("runs")).unwrap();
    let count = runs.count();
    assert!(count >= 1, "expected at least one run dir");
}

#[test]
fn have_execute_dry_run_does_not_create_a_run_directory() {
    let fx = Fx::new();
    let out = fx.run(&["--dry-run", "have", "greet", "execute", "hi"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runs_dir = fx.data_root.join("runs");
    let count = fs::read_dir(&runs_dir).map(|r| r.count()).unwrap_or(0);
    assert_eq!(count, 0, "dry-run must not create a run directory");
}

#[test]
fn have_execute_without_roster_id_returns_not_implemented() {
    let fx = Fx::new();
    // Add a katachi that has explicit selectors but no roster_id.
    fs::write(
        fx.data_root.join("katachis/raw.toml"),
        r#"
id = "raw"

[[targets]]
harness = "claude"

[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "skill", id = "greeter" }
"#,
    )
    .unwrap();
    let out = fx.run(&["have", "raw", "execute", "hi"]);
    assert_eq!(
        out.status.code(),
        Some(64),
        "expected NotImplemented; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
