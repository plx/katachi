//! End-to-end CLI tests for `katachi harness gemini ...`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

fn katachi_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_katachi"))
}

fn utf8(p: &std::path::Path) -> String {
    p.to_str().unwrap().to_owned()
}

struct Gx {
    _td: TempDir,
    data_root: PathBuf,
    config_path: PathBuf,
    cwd: PathBuf,
    fake_home: PathBuf,
    fake_gemini: PathBuf,
    ext_root: PathBuf,
}

impl Gx {
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

        // Write fake gemini script emitting stream-json.
        let fake = td.path().join("fake-gemini");
        fs::write(
            &fake,
            r#"#!/usr/bin/env bash
echo '{"type": "init", "model": "fake"}'
echo '{"type": "message", "role": "assistant", "text": "hello"}'
echo '{"type": "result", "summary": "done", "outcome": "success"}'
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&fake).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&fake, perms).unwrap();
        }

        Self {
            _td: td,
            data_root,
            config_path,
            cwd,
            fake_home,
            fake_gemini: fake,
            ext_root,
        }
    }

    fn write_extension(&self, name: &str, manifest: &str) {
        let ext_dir = self.ext_root.join(name);
        fs::create_dir_all(&ext_dir).unwrap();
        fs::write(ext_dir.join("gemini-extension.json"), manifest).unwrap();
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
        // Prepend our fake binary dir to PATH.
        let mut path = std::env::var("PATH").unwrap_or_default();
        let fake_dir = self.fake_gemini.parent().unwrap();
        path = format!("{}:{path}", fake_dir.display());
        cmd.env("PATH", &path);
        cmd.args(["--cwd", &utf8(&self.cwd)]);
        cmd.args(args);
        cmd.output().unwrap()
    }
}

fn expect_status(out: &std::process::Output, expected: i32) {
    assert_eq!(
        out.status.code(),
        Some(expected),
        "expected exit {expected}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn scan_reports_empty_catalog_when_no_artifacts() {
    let gx = Gx::new();
    let out = gx.run(&["harness", "gemini", "scan"]);
    expect_status(&out, 0);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("gemini roster scan"));
}

#[test]
fn scan_discovers_extension() {
    let gx = Gx::new();
    gx.write_extension(
        "workspace-a11y",
        r#"{"name": "workspace-a11y", "version": "0.1.0"}"#,
    );
    let out = gx.run(&["--json", "harness", "gemini", "scan"]);
    expect_status(&out, 0);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let items = v["items"].as_array().unwrap();
    assert!(items
        .iter()
        .any(|i| i["item_ref"]["kind"] == "extension" && i["item_ref"]["id"] == "workspace-a11y"));
}

#[test]
fn explain_prints_extension_detail() {
    let gx = Gx::new();
    gx.write_extension(
        "workspace-a11y",
        r#"{"name": "workspace-a11y", "version": "0.2.0", "description": "a11y tools"}"#,
    );
    let out = gx.run(&["harness", "gemini", "explain", "extension:workspace-a11y"]);
    expect_status(&out, 0);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("workspace-a11y"));
}

#[test]
fn plan_emits_gemini_argv_with_fake_binary() {
    let gx = Gx::new();
    gx.write_roster(
        "demo",
        r#"
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
"#,
    );
    let out = gx.run(&["harness", "gemini", "plan", "demo", "execute", "hello"]);
    expect_status(&out, 0);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("fake-gemini"),
        "stdout should include fake binary: {stdout}"
    );
    assert!(stdout.contains("--output-format"));
    assert!(stdout.contains("stream-json"));
    assert!(stdout.contains("--model"));
    assert!(stdout.contains("gemini-3-pro"));
}

#[test]
fn execute_runs_fake_binary_end_to_end() {
    let gx = Gx::new();
    gx.write_roster(
        "demo",
        r#"
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
"#,
    );
    // Ambient materialization, so we don't need symlink-able extensions.
    let out = gx.run(&[
        "--materialization",
        "ambient",
        "harness",
        "gemini",
        "execute",
        "demo",
        "hello",
    ]);
    expect_status(&out, 0);
    // Run record directory should now exist under runs/.
    let runs_dir = gx.data_root.join("runs");
    let entries: Vec<_> = fs::read_dir(&runs_dir).unwrap().flatten().collect();
    assert!(
        !entries.is_empty(),
        "expected at least one run directory under {}",
        runs_dir.display()
    );
    let run_dir = entries.into_iter().next().unwrap().path();
    // Executor should have produced these files.
    assert!(run_dir.join("request.json").exists());
    assert!(run_dir.join("plan.json").exists());
    assert!(run_dir.join("record.json").exists());
    assert!(run_dir.join("transcript.jsonl").exists());
    assert!(run_dir.join("transcript.gemini.jsonl").exists());
}

#[test]
fn doctor_reports_config() {
    let gx = Gx::new();
    let out = gx.run(&["harness", "gemini", "doctor"]);
    expect_status(&out, 0);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("gemini doctor"));
    assert!(stdout.contains("fake-gemini"));
}

#[test]
fn doctor_json_marks_binary_found_when_resolvable() {
    let gx = Gx::new();
    let out = gx.run(&["--json", "harness", "gemini", "doctor"]);
    expect_status(&out, 0);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["binary_found"], serde_json::Value::Bool(true));
    assert!(v["resolved_binary"].is_string());
}

#[test]
fn doctor_returns_config_exit_when_binary_missing() {
    let gx = Gx::new();
    // Rewrite config to point at a binary name that is not on PATH.
    fs::write(
        &gx.config_path,
        format!(
            r#"
version = 1

[harnesses.gemini]
enabled = true
binary = "katachi-nonexistent-gemini-xyz"
home = "{}"
user_roots = ["{}"]
project_roots = ["{}"]
extension_roots = ["{}"]
"#,
            gx.fake_home.display(),
            gx.fake_home.display(),
            gx.cwd.display(),
            gx.ext_root.display(),
        ),
    )
    .unwrap();
    let out = gx.run(&["harness", "gemini", "doctor"]);
    // ExitCode::Config = 3.
    expect_status(&out, 3);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("not on PATH"));
}

#[test]
fn doctor_json_returns_config_exit_when_binary_missing() {
    let gx = Gx::new();
    fs::write(
        &gx.config_path,
        format!(
            r#"
version = 1

[harnesses.gemini]
enabled = true
binary = "katachi-nonexistent-gemini-xyz"
home = "{}"
user_roots = ["{}"]
project_roots = ["{}"]
extension_roots = ["{}"]
"#,
            gx.fake_home.display(),
            gx.fake_home.display(),
            gx.cwd.display(),
            gx.ext_root.display(),
        ),
    )
    .unwrap();
    let out = gx.run(&["--json", "harness", "gemini", "doctor"]);
    expect_status(&out, 3);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["binary_found"], serde_json::Value::Bool(false));
    assert!(v["resolved_binary"].is_null());
}

#[test]
fn dump_settings_prints_layers() {
    let gx = Gx::new();
    // Write a user settings file.
    fs::write(
        gx.fake_home.join("settings.json"),
        r#"{"model": "gemini-3-flash"}"#,
    )
    .unwrap();
    gx.write_roster(
        "demo",
        r#"
version = 1
id = "demo"
[selection]
"#,
    );
    let out = gx.run(&["harness", "gemini", "dump-settings", "demo"]);
    expect_status(&out, 0);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("[user]"));
    assert!(stdout.contains("gemini-3-flash"));
}

#[test]
fn plan_rejects_sdk_ts_when_extension_selected() {
    let gx = Gx::new();
    gx.write_extension(
        "workspace-a11y",
        r#"{"name": "workspace-a11y", "version": "0.1.0"}"#,
    );
    gx.write_roster(
        "sdk",
        r#"
version = 1
id = "sdk"

[selection]
extensions = ["workspace-a11y"]

[run_profile]
backend = "sdk-ts"
"#,
    );
    let out = gx.run(&["harness", "gemini", "plan", "sdk", "execute", "hi"]);
    // Planner maps ProjectionLoss to ExitCode::Plan (6).
    // When validation also fires, ExitCode::Validate (5) may happen first.
    let code = out.status.code().unwrap_or(-1);
    assert!(
        code == 5 || code == 6,
        "expected plan or validate error; got {code}\nstderr:{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn graph_renders_text_format() {
    let gx = Gx::new();
    gx.write_extension("workspace-a11y", r#"{"name": "workspace-a11y"}"#);
    let out = gx.run(&["harness", "gemini", "graph", "--format", "text"]);
    expect_status(&out, 0);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("gemini roster graph"));
}
