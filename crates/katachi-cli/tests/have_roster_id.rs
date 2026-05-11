//! `have ...` with katachi targets that reference a harness-native
//! roster via `roster_id`.

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

        // Real Claude project artifacts.
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

        // Claude harness-native roster the katachi will reference by id.
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

        let katachis = root.join("data/katachis");
        fs::create_dir_all(&katachis).unwrap();

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

    fn write_katachi(&self, name: &str, body: &str) {
        let p = self.data_root.join("katachis").join(format!("{name}.toml"));
        fs::write(p, body).unwrap();
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
fn roster_id_only_target_resolves_through_roster() {
    let fx = Fx::new();
    fx.write_katachi(
        "claude-greet",
        r#"
id = "claude-greet"
description = "via roster_id"

[[targets]]
harness = "claude"
roster_id = "greet"
"#,
    );
    let out = fx.run(&["have", "claude-greet", "describe"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("greeter"),
        "expected greeter in describe output: {stdout}"
    );
}

#[test]
fn missing_roster_returns_resolve_error() {
    let fx = Fx::new();
    fx.write_katachi(
        "ghost",
        r#"
id = "ghost"

[[targets]]
harness = "claude"
roster_id = "no-such-roster"
"#,
    );
    let out = fx.run(&["have", "ghost", "describe"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "expected ExitCode::Resolve; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn roster_id_plus_explicit_selectors_is_rejected() {
    let fx = Fx::new();
    fx.write_katachi(
        "mixed",
        r#"
id = "mixed"

[[targets]]
harness = "claude"
roster_id = "greet"

[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "skill", id = "greeter" }
"#,
    );
    let out = fx.run(&["have", "mixed", "describe"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "expected ExitCode::Resolve when mixing roster_id and selectors; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("roster_id"),
        "diagnostic should mention roster_id: {combined}"
    );
}

#[test]
fn empty_roster_id_is_rejected() {
    let fx = Fx::new();
    fx.write_katachi(
        "blank",
        r#"
id = "blank"

[[targets]]
harness = "claude"
roster_id = "  "
"#,
    );
    let out = fx.run(&["have", "blank", "describe"]);
    assert_eq!(
        out.status.code(),
        Some(4),
        "expected ExitCode::Resolve; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
