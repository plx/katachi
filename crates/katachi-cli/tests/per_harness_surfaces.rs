//! Tests for newly implemented per-harness debug and projection surfaces:
//! `harness claude dump-settings`, `harness claude effective-config`,
//! `harness codex dump-roster`, `harness codex dump-settings`,
//! `harness gemini dump-roster`, `harness gemini effective-config`.
//! Also verifies Codex doctor returns ExitCode::Config when binary
//! missing.

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

fn make_executable(p: &Path) {
    let mut perms = fs::metadata(p).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(p, perms).unwrap();
}

// ---------------- Claude fixture ----------------

struct ClaudeFx {
    _td: TempDir,
    root: std::path::PathBuf,
    config_path: std::path::PathBuf,
    data_root: std::path::PathBuf,
}

impl ClaudeFx {
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
        write(
            &root,
            "data/rosters/claude/greet.toml",
            r#"
version = 1
id = "greet"

[selection]
skills = ["greeter"]

[run_profile]
backend = "cli"
model = "sonnet"
"#,
        );
        let fake_claude = root.join("fake-claude");
        fs::write(&fake_claude, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_claude);

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
fn claude_effective_config_emits_argv_and_backend() {
    let fx = ClaudeFx::new();
    let out = fx.run(&["--json", "harness", "claude", "effective-config", "greet"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["roster"], "greet");
    assert_eq!(v["backend"], "cli");
    assert!(v["argv"].is_array());
}

#[test]
fn claude_dump_settings_lists_overlay_files() {
    let fx = ClaudeFx::new();
    let out = fx.run(&["--json", "harness", "claude", "dump-settings", "greet"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["roster"], "greet");
    assert!(v["overlay_files"].is_array());
}

// ---------------- Codex fixture ----------------

struct CodexFx {
    _td: TempDir,
    workdir: std::path::PathBuf,
    #[allow(dead_code)]
    fake_path: std::path::PathBuf,
}

impl CodexFx {
    fn new() -> Self {
        let td = TempDir::new().unwrap();
        let workdir = td.path().to_path_buf();
        let fake_path = workdir.join("fake-codex.sh");
        let codex_home = workdir.join("codex-home");
        let data_root = workdir.join("data");
        fs::create_dir_all(&codex_home).unwrap();
        fs::create_dir_all(data_root.join("rosters/codex")).unwrap();
        fs::write(&fake_path, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_path);

        let body = format!(
            r#"version = 1
[harnesses.codex]
binary = "{bin}"
codex_home = "{home}"
project_roots = ["."]
respect_project_trust = false
"#,
            bin = fake_path.display(),
            home = codex_home.display(),
        );
        fs::write(workdir.join("config.toml"), body).unwrap();
        fs::write(
            codex_home.join("config.toml"),
            r#"model = "gpt-5.4"

[profiles.review]
approval_policy = "never"
sandbox_mode = "read-only"
"#,
        )
        .unwrap();
        fs::write(
            data_root.join("rosters/codex/audit.toml"),
            r#"
version = 1
id = "audit"
description = "smoke"

[selection]
profiles = ["review"]

[run_profile]
backend = "cli"
approval_policy = "never"
sandbox_mode = "read-only"
model = "gpt-5.4"
output_mode = "machine-readable"
"#,
        )
        .unwrap();

        Self {
            _td: td,
            workdir,
            fake_path,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = katachi_bin();
        cmd.env_remove("RUST_LOG")
            .env_remove("KATACHI_FIXTURE_HARNESSES")
            .env("KATACHI_CONFIG", self.workdir.join("config.toml"))
            .env("KATACHI_DATA", self.workdir.join("data"))
            .env("KATACHI_CACHE", self.workdir.join("data/cache"))
            .args(["--cwd"])
            .arg(&self.workdir)
            .args(args);
        cmd.output().unwrap()
    }
}

#[test]
fn codex_dump_roster_emits_summary_and_resolved_items() {
    let fx = CodexFx::new();
    let out = fx.run(&["--json", "harness", "codex", "dump-roster", "audit"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["roster"]["id"], "audit");
}

#[test]
fn codex_dump_settings_emits_layer_order() {
    let fx = CodexFx::new();
    let out = fx.run(&["--json", "harness", "codex", "dump-settings", "audit"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["roster"], "audit");
    assert!(v["layer_order"].is_array());
}

#[test]
fn codex_project_ts_emits_advisory_code() {
    let fx = CodexFx::new();
    let out = fx.run(&[
        "--json", "harness", "codex", "project", "audit", "--sdk", "ts",
    ]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["backend"], "sdk-ts");
    assert!(v["code"].as_str().unwrap().contains("@openai/codex-sdk"));
}

#[test]
fn codex_project_py_disabled_exits_plan() {
    let fx = CodexFx::new();
    let out = fx.run(&[
        "--json", "harness", "codex", "project", "audit", "--sdk", "py",
    ]);
    assert_eq!(out.status.code(), Some(6));
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert!(v["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["code"] == "codex.projection.sdk-py.disabled"));
}

#[test]
fn codex_project_py_enabled_emits_advisory_code() {
    let fx = CodexFx::new();
    let config_path = fx.workdir.join("config.toml");
    let body = fs::read_to_string(&config_path).unwrap();
    fs::write(&config_path, format!("{body}\nenable_python_sdk = true\n")).unwrap();
    let out = fx.run(&[
        "--json", "harness", "codex", "project", "audit", "--sdk", "py",
    ]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["backend"], "sdk-py");
    assert!(v["code"].as_str().unwrap().contains("codex_sdk"));
}

#[test]
fn codex_doctor_returns_config_when_binary_missing() {
    let td = TempDir::new().unwrap();
    let workdir = td.path().to_path_buf();
    let codex_home = workdir.join("codex-home");
    let data_root = workdir.join("data");
    fs::create_dir_all(&codex_home).unwrap();
    fs::create_dir_all(data_root.join("rosters/codex")).unwrap();
    fs::write(
        workdir.join("config.toml"),
        format!(
            r#"version = 1
[harnesses.codex]
binary = "katachi-no-such-codex-binary"
codex_home = "{}"
project_roots = ["."]
respect_project_trust = false
"#,
            codex_home.display()
        ),
    )
    .unwrap();
    let mut cmd = katachi_bin();
    cmd.env_remove("RUST_LOG")
        .env("KATACHI_CONFIG", workdir.join("config.toml"))
        .env("KATACHI_DATA", workdir.join("data"))
        .env("KATACHI_CACHE", workdir.join("data/cache"))
        .args(["--cwd"])
        .arg(&workdir)
        .args(["harness", "codex", "doctor"]);
    let out = cmd.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "expected ExitCode::Config (3); stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------- Gemini fixture ----------------

struct GeminiFx {
    _td: TempDir,
    data_root: std::path::PathBuf,
    config_path: std::path::PathBuf,
    cwd: std::path::PathBuf,
    fake_dir: std::path::PathBuf,
    ext_root: std::path::PathBuf,
}

impl GeminiFx {
    fn new() -> Self {
        let td = TempDir::new().unwrap();
        let data_root = td.path().join("data");
        fs::create_dir_all(data_root.join("rosters/gemini")).unwrap();
        let cwd = td.path().join("project");
        fs::create_dir_all(&cwd).unwrap();
        let fake_home = td.path().join("home");
        fs::create_dir_all(&fake_home).unwrap();
        let ext_root = fake_home.join("extensions");
        fs::create_dir_all(&ext_root).unwrap();
        let config_path = td.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
version = 1
[harnesses.gemini]
enabled = true
binary = "fake-gemini"
home = "{}"
user_roots = ["{}"]
project_roots = ["{}"]
extension_roots = ["{}"]
"#,
                fake_home.display(),
                fake_home.display(),
                cwd.display(),
                ext_root.display(),
            ),
        )
        .unwrap();
        let fake = td.path().join("fake-gemini");
        fs::write(&fake, "#!/usr/bin/env bash\nexit 0\n").unwrap();
        make_executable(&fake);
        let fake_dir = fake.parent().unwrap().to_path_buf();

        // Roster.
        fs::write(
            data_root.join("rosters/gemini/demo.toml"),
            r#"
version = 1
id = "demo"
[selection]

[run_profile]
backend = "cli"
model = "gemini-3-pro"
binary = "fake-gemini"
"#,
        )
        .unwrap();

        Self {
            _td: td,
            data_root,
            config_path,
            cwd,
            fake_dir,
            ext_root,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = katachi_bin();
        cmd.env_remove("KATACHI_LOG")
            .env_remove("RUST_LOG")
            .env("KATACHI_DATA", &self.data_root)
            .env("KATACHI_CACHE", self.data_root.join("cache"))
            .env("KATACHI_CONFIG", &self.config_path);
        let mut path = std::env::var("PATH").unwrap_or_default();
        path = format!("{}:{path}", self.fake_dir.display());
        cmd.env("PATH", &path);
        cmd.args(["--cwd", self.cwd.to_str().unwrap()]);
        cmd.args(args);
        cmd.output().unwrap()
    }
}

#[test]
fn gemini_dump_roster_emits_projection() {
    let fx = GeminiFx::new();
    let out = fx.run(&["--json", "harness", "gemini", "dump-roster", "demo"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["roster"]["id"], "demo");
    assert!(v["projected_definition"].is_object());
}

#[test]
fn gemini_effective_config_emits_argv() {
    let fx = GeminiFx::new();
    let out = fx.run(&["--json", "harness", "gemini", "effective-config", "demo"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["roster"], "demo");
    assert!(v["plan_argv"].is_array());
}

#[test]
fn gemini_project_ts_emits_advisory_code() {
    let fx = GeminiFx::new();
    let out = fx.run(&[
        "--json", "harness", "gemini", "project", "demo", "--sdk", "ts",
    ]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["backend"], "sdk-ts");
    assert!(v["code"]
        .as_str()
        .unwrap()
        .contains("@google/gemini-cli-sdk"));
}

#[test]
fn gemini_project_extension_blocks_sdk_ts() {
    let fx = GeminiFx::new();
    fs::create_dir_all(fx.ext_root.join("workspace-a11y")).unwrap();
    fs::write(
        fx.ext_root
            .join("workspace-a11y")
            .join("gemini-extension.json"),
        r#"{"name":"workspace-a11y","version":"0.1.0"}"#,
    )
    .unwrap();
    fs::write(
        fx.data_root.join("rosters/gemini/ext.toml"),
        r#"
version = 1
id = "ext"

[selection]
extensions = ["workspace-a11y"]
"#,
    )
    .unwrap();
    let out = fx.run(&[
        "--json", "harness", "gemini", "project", "ext", "--sdk", "ts",
    ]);
    assert_eq!(out.status.code(), Some(6));
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert!(v["validator_diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["code"] == "gemini.projection.sdk-ts-unsupported"));
}

#[test]
fn gemini_project_py_exits_plan() {
    let fx = GeminiFx::new();
    let out = fx.run(&[
        "--json", "harness", "gemini", "project", "demo", "--sdk", "py",
    ]);
    assert_eq!(out.status.code(), Some(6));
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert!(v["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["code"] == "gemini.project.projection"));
}
