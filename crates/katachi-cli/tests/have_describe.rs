//! End-to-end tests for `katachi have <id> describe`.
//!
//! These drive the built `katachi` binary against a tempdir containing a
//! katachi TOML and the fixture harness hatch (`KATACHI_FIXTURE_HARNESSES`).

use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

fn katachi_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_katachi"))
}

struct Fixture {
    _td: TempDir,
    data_root: PathBuf,
    config_path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let td = TempDir::new().unwrap();
        let data_root = td.path().join("data");
        let katachis = data_root.join("katachis");
        std::fs::create_dir_all(&katachis).unwrap();
        let config_path = td.path().join("config.toml");
        std::fs::write(&config_path, "version = 1\n").unwrap();
        Self {
            _td: td,
            data_root,
            config_path,
        }
    }

    fn katachis_dir(&self) -> PathBuf {
        self.data_root.join("katachis")
    }

    fn write_katachi(&self, name: &str, body: &str) {
        let p = self.katachis_dir().join(format!("{name}.toml"));
        std::fs::write(p, body).unwrap();
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = katachi_bin();
        cmd.env_remove("KATACHI_LOG")
            .env_remove("RUST_LOG")
            .env("KATACHI_FIXTURE_HARNESSES", "toy_claude")
            .env("KATACHI_DATA", &self.data_root)
            .env("KATACHI_CACHE", self.data_root.join("cache"))
            .env("KATACHI_CONFIG", &self.config_path)
            .args(args);
        cmd.output().unwrap()
    }
}

fn happy_path_toml() -> &'static str {
    r#"
id = "a11y"
description = "Accessibility auditing loadout."

[[targets]]
harness = "claude"
backend = "cli"

[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "plugin", id = "web-a11y" }
"#
}

fn assert_ok_status(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "expected success, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn assert_exit(out: &std::process::Output, expected: i32) {
    assert_eq!(
        out.status.code(),
        Some(expected),
        "expected exit {expected}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn describe_happy_path_human_snapshot() {
    let fx = Fixture::new();
    fx.write_katachi("a11y", happy_path_toml());

    let out = fx.run(&["have", "a11y", "describe"]);
    assert_ok_status(&out);

    let stdout = String::from_utf8(out.stdout).unwrap();
    insta::assert_snapshot!("describe_happy_human", stdout);
}

#[test]
fn describe_happy_path_json_snapshot() {
    let fx = Fixture::new();
    fx.write_katachi("a11y", happy_path_toml());

    let out = fx.run(&["--json", "have", "a11y", "describe"]);
    assert_ok_status(&out);

    let stdout = String::from_utf8(out.stdout).unwrap();
    // JSON snapshot: pretty-printed directly by the CLI.
    insta::assert_snapshot!("describe_happy_json", stdout);
}

#[test]
fn describe_unknown_katachi_exits_resolve() {
    let fx = Fixture::new();

    let out = fx.run(&["have", "no-such-loadout", "describe"]);
    assert_exit(&out, 4);

    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("no katachi with id"),
        "stderr missing expected message: {stderr}"
    );
}

#[test]
fn describe_dangling_packaging_exits_validate() {
    let fx = Fixture::new();
    // Selecting a skill directly (without its required package in the set)
    // trips `validate.dangling-packaging`, which is an error → exit 5.
    fx.write_katachi(
        "partial",
        r#"
id = "partial"

[[targets]]
harness = "claude"
backend = "cli"

[[targets.selectors.selectors]]
type = "item_ref"
item_ref = { harness = "claude", kind = "skill", id = "axe-runner" }
"#,
    );

    let out = fx.run(&["have", "partial", "describe"]);
    assert_exit(&out, 5);

    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("validate.dangling-packaging"),
        "stdout missing expected diagnostic code: {stdout}"
    );
}

#[test]
fn describe_missing_katachis_dir_exits_resolve() {
    // Build a fixture but delete the katachis dir.
    let td = TempDir::new().unwrap();
    let data_root = td.path().join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    let config_path = td.path().join("config.toml");
    std::fs::write(&config_path, "version = 1\n").unwrap();

    let mut cmd = katachi_bin();
    cmd.env_remove("KATACHI_LOG")
        .env_remove("RUST_LOG")
        .env("KATACHI_FIXTURE_HARNESSES", "toy_claude")
        .env("KATACHI_DATA", &data_root)
        .env("KATACHI_CACHE", data_root.join("cache"))
        .env("KATACHI_CONFIG", &config_path)
        .args(["have", "anything", "describe"]);
    let out = cmd.output().unwrap();

    // Directory-missing is reported as a resolve failure.
    assert_exit(&out, 4);
}

/// Sanity: ensure the binary name stamped in from cargo actually points at
/// our CLI and not something else on PATH.
#[test]
fn binary_prints_help() {
    let out = katachi_bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("katachi"), "help missing name: {stdout}");
}
