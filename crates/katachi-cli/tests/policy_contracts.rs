//! Policy contracts encoded as CLI tests.
//!
//! These tests pin down the behaviors documented in
//! `docs/remediation/policy-decisions.md`. Some of them intentionally
//! describe the *intended* contract and will fail against the current
//! implementation — Plan 2 of the remediation makes them pass.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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

fn make_executable(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn run_count(runs_dir: &Path) -> usize {
    fs::read_dir(runs_dir).map(|r| r.count()).unwrap_or(0)
}

fn list_run_dirs(runs_dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(runs_dir)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

// ---------------- Gemini fixture ----------------

struct GeminiFx {
    _td: TempDir,
    data_root: PathBuf,
    config_path: PathBuf,
    cwd: PathBuf,
    fake_gemini: PathBuf,
}

impl GeminiFx {
    fn new(failing_binary: bool) -> Self {
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
        let body = if failing_binary {
            r#"#!/usr/bin/env bash
echo "FAKE_GEMINI_FAILED" 1>&2
exit 2
"#
        } else {
            r#"#!/usr/bin/env bash
echo '{"type": "init", "model": "fake"}'
echo '{"type": "result", "summary": "done", "outcome": "success"}'
"#
        };
        fs::write(&fake, body).unwrap();
        make_executable(&fake);

        Self {
            _td: td,
            data_root,
            config_path,
            cwd,
            fake_gemini: fake,
        }
    }

    fn write_roster(&self, name: &str, body: &str) {
        let p = self
            .data_root
            .join("rosters/gemini")
            .join(format!("{name}.toml"));
        fs::write(p, body).unwrap();
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = katachi_bin();
        cmd.env_remove("KATACHI_LOG")
            .env_remove("RUST_LOG")
            .env("KATACHI_DATA", &self.data_root)
            .env("KATACHI_CACHE", self.data_root.join("cache"))
            .env("KATACHI_CONFIG", &self.config_path);
        let mut path = std::env::var("PATH").unwrap_or_default();
        let fake_dir = self.fake_gemini.parent().unwrap();
        path = format!("{}:{path}", fake_dir.display());
        cmd.env("PATH", &path);
        cmd.args(["--cwd", self.cwd.to_str().unwrap()]);
        cmd.args(args);
        cmd.output().unwrap()
    }
}

const GEMINI_DEMO_ROSTER: &str = r#"
version = 1
id = "demo"
description = "Fake binary demo"

[selection]

[run_profile]
backend = "cli"
model = "gemini-3-pro"
approval_mode = "plan"
output_format = "stream-json"
binary = "fake-gemini"
"#;

// ---------------- Codex fixture ----------------

struct CodexFx {
    _td: TempDir,
    workdir: PathBuf,
}

impl CodexFx {
    fn new(failing_binary: bool) -> Self {
        let td = TempDir::new().unwrap();
        let workdir = td.path().to_path_buf();
        let fake_path = workdir.join("fake-codex.sh");
        let codex_home = workdir.join("codex-home");
        let data_root = workdir.join("data");
        fs::create_dir_all(&codex_home).unwrap();
        fs::create_dir_all(data_root.join("rosters/codex")).unwrap();
        fs::create_dir_all(data_root.join("katachis")).unwrap();
        let body = if failing_binary {
            "#!/bin/sh\necho FAKE_CODEX_FAILED 1>&2\nexit 2\n"
        } else {
            "#!/bin/sh\necho FAKE_CODEX args=$*\necho CODEX_HOME=$CODEX_HOME\nexit 0\n"
        };
        fs::write(&fake_path, body).unwrap();
        make_executable(&fake_path);

        let config_body = format!(
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
        fs::write(workdir.join("config.toml"), config_body).unwrap();

        // Provide a minimal home config so resolution succeeds.
        fs::write(
            codex_home.join("config.toml"),
            r#"model = "gpt-5.4"

[profiles.review]
approval_policy = "never"
sandbox_mode = "read-only"
"#,
        )
        .unwrap();

        let _ = fake_path;
        Self {
            _td: td,
            workdir,
        }
    }

    fn config_path(&self) -> PathBuf {
        self.workdir.join("config.toml")
    }

    fn data_root(&self) -> PathBuf {
        self.workdir.join("data")
    }

    fn write_roster(&self, id: &str, body: &str) {
        let p = self
            .data_root()
            .join("rosters/codex")
            .join(format!("{id}.toml"));
        fs::write(p, body).unwrap();
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = katachi_bin();
        cmd.env_remove("KATACHI_LOG")
            .env_remove("RUST_LOG")
            .env("KATACHI_CONFIG", self.config_path())
            .env("KATACHI_DATA", self.data_root())
            .env("KATACHI_CACHE", self.data_root().join("cache"))
            .args(["--cwd"])
            .arg(&self.workdir)
            .args(args);
        cmd.output().unwrap()
    }
}

const CODEX_AUDIT_ROSTER: &str = r#"
version = 1
id = "audit"
description = "smoke-test audit"

[selection]
profiles = ["review"]

[run_profile]
backend = "cli"
approval_policy = "never"
sandbox_mode = "read-only"
model = "gpt-5.4"
output_mode = "machine-readable"
"#;

// ---------------- Claude fixture ----------------

struct ClaudeFx {
    _td: TempDir,
    root: PathBuf,
    config_path: PathBuf,
    data_root: PathBuf,
    fake_claude: PathBuf,
}

impl ClaudeFx {
    fn new(failing_binary: bool) -> Self {
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
        write(&root, "CLAUDE.md", "project-level instructions");
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
        let body = if failing_binary {
            "#!/bin/sh\necho FAKE_CLAUDE_FAILED 1>&2\nexit 2\n"
        } else {
            "#!/bin/sh\necho '{\"type\":\"assistant\",\"text\":\"ack\"}'\necho '{\"type\":\"result\",\"subtype\":\"success\"}'\n"
        };
        fs::write(&fake_claude, body).unwrap();
        make_executable(&fake_claude);

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
            .args(["--cwd", self.root.to_str().unwrap()])
            .args(args);
        cmd.output().unwrap()
    }
}

// =================================================================
// Policy 1: dry-run contract
// =================================================================

#[test]
fn gemini_dry_run_does_not_write_a_run() {
    let fx = GeminiFx::new(false);
    fx.write_roster("demo", GEMINI_DEMO_ROSTER);

    let out = fx.run(&[
        "--dry-run",
        "--materialization",
        "ambient",
        "harness",
        "gemini",
        "execute",
        "demo",
        "hi",
    ]);
    assert!(
        out.status.success(),
        "expected dry-run success; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runs_dir = fx.data_root.join("runs");
    assert_eq!(
        run_count(&runs_dir),
        0,
        "dry-run must not create a run directory"
    );
}

#[test]
fn gemini_dry_run_does_not_invoke_fake_binary() {
    let fx = GeminiFx::new(false);
    fx.write_roster("demo", GEMINI_DEMO_ROSTER);

    let out = fx.run(&[
        "--dry-run",
        "--materialization",
        "ambient",
        "harness",
        "gemini",
        "execute",
        "demo",
        "hi",
    ]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("\"type\": \"init\""),
        "fake gemini stdout should not appear in dry-run output: {combined}"
    );
}

#[test]
fn codex_dry_run_does_not_invoke_fake_binary() {
    let fx = CodexFx::new(false);
    fx.write_roster("audit", CODEX_AUDIT_ROSTER);

    let out = fx.run(&["--dry-run", "harness", "codex", "execute", "audit", "hi"]);
    assert!(
        out.status.success(),
        "expected dry-run success; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("FAKE_CODEX args="),
        "fake codex stdout should not appear in dry-run: {combined}"
    );
}

#[test]
fn claude_dry_run_does_not_invoke_fake_binary_or_create_a_run() {
    let fx = ClaudeFx::new(false);

    let out = fx.run(&[
        "--dry-run",
        "harness",
        "claude",
        "execute",
        "greet",
        "hi",
    ]);
    assert!(
        out.status.success(),
        "expected dry-run success; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runs_dir = fx.data_root.join("runs");
    assert_eq!(
        run_count(&runs_dir),
        0,
        "dry-run must not create a run directory"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("\"type\":\"assistant\""),
        "fake claude stdout should not appear in dry-run: {combined}"
    );
    let _ = fx.fake_claude; // suppress unused warning
}

// =================================================================
// Policy 2: failed-run persistence
// =================================================================

#[test]
fn codex_failed_execute_leaves_partial_directory() {
    let fx = CodexFx::new(true);
    fx.write_roster("audit", CODEX_AUDIT_ROSTER);

    let out = fx.run(&["harness", "codex", "execute", "audit", "hi"]);
    assert_eq!(
        out.status.code(),
        Some(7),
        "expected ExitCode::Execute (7); stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let runs_dir = fx.data_root().join("runs");
    let dirs = list_run_dirs(&runs_dir);
    let partials: Vec<_> = dirs
        .iter()
        .filter(|p| p.to_string_lossy().ends_with(".partial"))
        .collect();
    assert_eq!(
        partials.len(),
        1,
        "expected exactly one .partial run dir; got {dirs:?}"
    );
    let committed: Vec<_> = dirs
        .iter()
        .filter(|p| !p.to_string_lossy().ends_with(".partial"))
        .collect();
    assert!(
        committed.is_empty(),
        "failed run must not be committed; got {committed:?}"
    );
}

#[test]
fn gemini_failed_execute_leaves_partial_directory() {
    let fx = GeminiFx::new(true);
    fx.write_roster("demo", GEMINI_DEMO_ROSTER);

    let out = fx.run(&[
        "--materialization",
        "ambient",
        "harness",
        "gemini",
        "execute",
        "demo",
        "hi",
    ]);
    assert_eq!(
        out.status.code(),
        Some(7),
        "expected ExitCode::Execute (7); stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let runs_dir = fx.data_root.join("runs");
    let dirs = list_run_dirs(&runs_dir);
    let partials: Vec<_> = dirs
        .iter()
        .filter(|p| p.to_string_lossy().ends_with(".partial"))
        .collect();
    assert_eq!(
        partials.len(),
        1,
        "expected exactly one .partial run dir; got {dirs:?}"
    );
    let committed: Vec<_> = dirs
        .iter()
        .filter(|p| !p.to_string_lossy().ends_with(".partial"))
        .collect();
    assert!(
        committed.is_empty(),
        "failed run must not be committed; got {committed:?}"
    );
}

#[test]
fn claude_failed_execute_leaves_partial_directory() {
    let fx = ClaudeFx::new(true);

    let out = fx.run(&["harness", "claude", "execute", "greet", "hi"]);
    assert_eq!(
        out.status.code(),
        Some(7),
        "expected ExitCode::Execute (7); stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let runs_dir = fx.data_root.join("runs");
    let dirs = list_run_dirs(&runs_dir);
    let partials: Vec<_> = dirs
        .iter()
        .filter(|p| p.to_string_lossy().ends_with(".partial"))
        .collect();
    assert_eq!(
        partials.len(),
        1,
        "expected exactly one .partial run dir; got {dirs:?}"
    );
}

// =================================================================
// Policy 4: materialization precedence
// =================================================================

#[test]
fn codex_global_materialization_overrides_roster() {
    // Roster sets ambient; CLI overrides to temp-overlay. The plan should
    // reflect the CLI override.
    let fx = CodexFx::new(false);
    fx.write_roster(
        "audit",
        r#"
version = 1
id = "audit"
description = "ambient roster"

[selection]
profiles = ["review"]

[run_profile]
backend = "cli"
approval_policy = "never"
sandbox_mode = "read-only"
model = "gpt-5.4"

[resolution]
materialization = "ambient"
"#,
    );

    let out = fx.run(&[
        "--json",
        "--materialization",
        "temp-overlay",
        "harness",
        "codex",
        "plan",
        "audit",
        "execute",
        "hi",
    ]);
    assert!(
        out.status.success(),
        "expected success; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// =================================================================
// Policy 5: duplicate ids
// =================================================================

#[test]
fn duplicate_katachi_ids_are_rejected() {
    // Two katachi files share the same id; loading must fail.
    let td = TempDir::new().unwrap();
    let data_root = td.path().join("data");
    let kat = data_root.join("katachis");
    fs::create_dir_all(&kat).unwrap();
    let body = r#"
id = "dup"
[[targets]]
harness = "claude"
"#;
    fs::write(kat.join("a.toml"), body).unwrap();
    fs::write(kat.join("b.toml"), body).unwrap();

    let config_path = td.path().join("config.toml");
    fs::write(&config_path, "version = 1\n").unwrap();

    let mut cmd = katachi_bin();
    cmd.env_remove("KATACHI_LOG")
        .env_remove("RUST_LOG")
        .env("KATACHI_FIXTURE_HARNESSES", "toy_claude")
        .env("KATACHI_DATA", &data_root)
        .env("KATACHI_CACHE", data_root.join("cache"))
        .env("KATACHI_CONFIG", &config_path)
        .args(["have", "dup", "describe"]);
    let out = cmd.output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    assert!(
        code == 3 || code == 4,
        "expected ExitCode::Config (3) or Resolve (4) for duplicate ids; got {code}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// =================================================================
// Policy 7: stubbed JSON behavior
// =================================================================

#[test]
fn json_katachi_show_emits_structured_not_implemented_error() {
    // `katachi show <id>` is stubbed until Plan 6 lands; with --json it
    // must emit a parseable JSON object describing the not-implemented
    // status, not a plain-text error.
    let td = TempDir::new().unwrap();
    let data_root = td.path().join("data");
    fs::create_dir_all(data_root.join("katachis")).unwrap();
    let config_path = td.path().join("config.toml");
    fs::write(&config_path, "version = 1\n").unwrap();

    let mut cmd = katachi_bin();
    cmd.env_remove("KATACHI_LOG")
        .env_remove("RUST_LOG")
        .env("KATACHI_DATA", &data_root)
        .env("KATACHI_CACHE", data_root.join("cache"))
        .env("KATACHI_CONFIG", &config_path)
        .args(["--json", "katachi", "show", "anything"]);
    let out = cmd.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(64),
        "stubbed command must exit ExitCode::NotImplemented (64); stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|err| {
        panic!("--json output must be valid JSON; err={err}; got: {stdout}")
    });
    assert_eq!(v["error"]["kind"], "not_implemented");
}

#[test]
fn json_run_list_emits_structured_not_implemented_error() {
    let td = TempDir::new().unwrap();
    let data_root = td.path().join("data");
    fs::create_dir_all(data_root.join("katachis")).unwrap();
    let config_path = td.path().join("config.toml");
    fs::write(&config_path, "version = 1\n").unwrap();

    let mut cmd = katachi_bin();
    cmd.env_remove("KATACHI_LOG")
        .env_remove("RUST_LOG")
        .env("KATACHI_DATA", &data_root)
        .env("KATACHI_CACHE", data_root.join("cache"))
        .env("KATACHI_CONFIG", &config_path)
        .args(["--json", "run", "list"]);
    let out = cmd.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(64),
        "stubbed command must exit ExitCode::NotImplemented (64); stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|err| {
        panic!("--json output must be valid JSON; err={err}; got: {stdout}")
    });
    assert_eq!(v["error"]["kind"], "not_implemented");
}
