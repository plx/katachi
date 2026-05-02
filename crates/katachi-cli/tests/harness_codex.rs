//! End-to-end tests for `katachi harness codex <subcommand>`.
//!
//! Each test builds a scratch `CODEX_HOME`, a katachi data root, and
//! a roster file, then drives the compiled `katachi` binary through the
//! various operator commands.

use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

fn katachi_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_katachi"))
}

struct Fx {
    _td: TempDir,
    workdir: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let td = TempDir::new().unwrap();
        let workdir = td.path().to_path_buf();
        let this = Self { _td: td, workdir };
        std::fs::create_dir_all(this.codex_home()).unwrap();
        std::fs::create_dir_all(this.data_root().join("rosters/codex")).unwrap();
        std::fs::create_dir_all(this.data_root().join("katachis")).unwrap();
        this.write_config();
        this
    }

    fn codex_home(&self) -> PathBuf {
        self.workdir.join("codex-home")
    }
    fn data_root(&self) -> PathBuf {
        self.workdir.join("data")
    }
    fn config_path(&self) -> PathBuf {
        self.workdir.join("config.toml")
    }

    fn write_config(&self) {
        let codex_home = self.codex_home().display().to_string();
        let body = format!(
            r#"version = 1

[harnesses.codex]
binary = "{bin}"
codex_home = "{home}"
project_roots = ["."]
respect_project_trust = false
"#,
            bin = self.fake_codex_path().display(),
            home = codex_home,
        );
        std::fs::write(self.config_path(), body).unwrap();
    }

    fn fake_codex_path(&self) -> PathBuf {
        self.workdir.join("fake-codex.sh")
    }

    fn install_fake_codex(&self) {
        let script = "#!/bin/sh\necho FAKE_CODEX args=$*\necho CODEX_HOME=$CODEX_HOME\nexit 0\n";
        std::fs::write(self.fake_codex_path(), script).unwrap();
        // chmod +x
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(self.fake_codex_path())
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(self.fake_codex_path(), perms).unwrap();
        }
    }

    fn write_home_config(&self, body: &str) {
        std::fs::write(self.codex_home().join("config.toml"), body).unwrap();
    }

    fn write_agents_md(&self, scope: &str, body: &str) {
        let target = match scope {
            "home" => self.codex_home().join("AGENTS.md"),
            "project" => self.workdir.join("AGENTS.md"),
            other => panic!("unknown scope {other}"),
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, body).unwrap();
    }

    fn write_roster(&self, id: &str, body: &str) {
        let p = self
            .data_root()
            .join("rosters/codex")
            .join(format!("{id}.toml"));
        std::fs::write(p, body).unwrap();
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

fn sample_roster() -> &'static str {
    r#"
version = 1
id = "audit"
description = "smoke-test audit"

[selection]
profiles = ["review"]
mcp_servers = ["chrome"]

[run_profile]
backend = "cli"
approval_policy = "never"
sandbox_mode = "read-only"
model = "gpt-5.4"
output_mode = "machine-readable"
"#
}

fn sample_roster_without_backend() -> &'static str {
    r#"
version = 1
id = "audit"
description = "smoke-test audit"

[selection]
profiles = ["review"]

[run_profile]
approval_policy = "never"
sandbox_mode = "read-only"
model = "gpt-5.4"
"#
}

fn sample_home_config() -> &'static str {
    r#"
model = "gpt-5.4"

[profiles.review]
approval_policy = "never"
sandbox_mode = "read-only"

[mcp_servers.chrome]
command = "chrome-mcp"
"#
}

#[test]
fn scan_lists_items_from_home_and_project() {
    let fx = Fx::new();
    fx.install_fake_codex();
    fx.write_home_config(sample_home_config());
    fx.write_agents_md("home", "global instructions\n");
    fx.write_agents_md("project", "project instructions\n");

    let out = fx.run(&["harness", "codex", "scan"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("config_layer"));
    assert!(stdout.contains("profile"));
    assert!(stdout.contains("instruction_doc"));
    assert!(stdout.contains("mcp_server"));
}

#[test]
fn doctor_reports_config_and_binary() {
    let fx = Fx::new();
    fx.install_fake_codex();
    fx.write_home_config(sample_home_config());

    let out = fx.run(&["harness", "codex", "doctor"]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("Codex home"));
    assert!(stdout.contains("exists: yes"));
}

#[test]
fn effective_config_resolves_policy_from_profile() {
    let fx = Fx::new();
    fx.install_fake_codex();
    fx.write_home_config(sample_home_config());
    fx.write_roster("audit", sample_roster());

    let out = fx.run(&["harness", "codex", "effective-config", "audit"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("approval_policy: Some(\"never\")"));
    assert!(stdout.contains("sandbox_mode:    Some(\"read-only\")"));
    assert!(stdout.contains("chrome"));
}

#[test]
fn plan_prints_full_argv() {
    let fx = Fx::new();
    fx.install_fake_codex();
    fx.write_home_config(sample_home_config());
    fx.write_roster("audit", sample_roster());

    let out = fx.run(&["harness", "codex", "plan", "audit", "execute", "audit repo"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("fake-codex.sh") || stdout.contains("codex"));
    assert!(stdout.contains("--ask-for-approval never"));
    assert!(stdout.contains("--sandbox read-only"));
}

#[test]
fn prefer_backend_beats_config_default_when_roster_unpinned() {
    let fx = Fx::new();
    fx.install_fake_codex();
    fx.write_home_config(sample_home_config());
    fx.write_roster("audit", sample_roster_without_backend());
    let body = format!(
        r#"version = 1

[harnesses.codex]
binary = "{bin}"
default_backend = "sdk-ts"
codex_home = "{home}"
project_roots = ["."]
respect_project_trust = false
"#,
        bin = fx.fake_codex_path().display(),
        home = fx.codex_home().display(),
    );
    std::fs::write(fx.config_path(), body).unwrap();

    let out = fx.run(&[
        "--json",
        "--prefer-backend",
        "cli",
        "harness",
        "codex",
        "plan",
        "audit",
        "execute",
        "audit repo",
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["backend"], "cli");
}

#[test]
fn roster_backend_beats_prefer_backend() {
    let fx = Fx::new();
    fx.install_fake_codex();
    fx.write_home_config(sample_home_config());
    fx.write_roster(
        "audit",
        r#"
version = 1
id = "audit"

[selection]
profiles = ["review"]

[run_profile]
backend = "sdk-ts"
approval_policy = "never"
sandbox_mode = "read-only"
model = "gpt-5.4"
"#,
    );

    let out = fx.run(&[
        "--json",
        "--prefer-backend",
        "cli",
        "harness",
        "codex",
        "plan",
        "audit",
        "execute",
        "audit repo",
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap();
    assert_eq!(v["backend"], "sdk-ts");
}

#[test]
fn unknown_roster_backend_is_rejected() {
    let fx = Fx::new();
    fx.install_fake_codex();
    fx.write_home_config(sample_home_config());
    fx.write_roster(
        "audit",
        r#"
version = 1
id = "audit"

[selection]
profiles = ["review"]

[run_profile]
backend = "nope"
"#,
    );

    let out = fx.run(&["harness", "codex", "plan", "audit", "execute", "audit repo"]);
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("unknown codex roster backend `nope`"),
        "{stderr}"
    );
}

#[test]
fn execute_writes_run_record() {
    let fx = Fx::new();
    fx.install_fake_codex();
    fx.write_home_config(sample_home_config());
    fx.write_roster("audit", sample_roster());

    let out = fx.run(&["harness", "codex", "execute", "audit", "audit repo"]);
    assert!(
        out.status.success(),
        "expected success, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("finished with outcome"), "{stdout}");

    // Ensure a run directory was persisted.
    let runs = fx.data_root().join("runs");
    let entries: Vec<_> = std::fs::read_dir(&runs).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1, "expected one run in {runs:?}");
    let run_dir = entries.into_iter().next().unwrap().path();
    for f in ["request.json", "plan.json", "record.json", "stdout.log"] {
        assert!(run_dir.join(f).exists(), "missing {f}");
    }
    let stdout_log = std::fs::read_to_string(run_dir.join("stdout.log")).unwrap();
    assert!(stdout_log.contains("FAKE_CODEX"));
    assert!(stdout_log.contains("CODEX_HOME="));
}

#[test]
fn execute_dry_run_does_not_write_a_run() {
    let fx = Fx::new();
    fx.install_fake_codex();
    fx.write_home_config(sample_home_config());
    fx.write_roster("audit", sample_roster());

    let out = fx.run(&["--dry-run", "harness", "codex", "execute", "audit", "hi"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("dry run"));
    let runs = fx.data_root().join("runs");
    let count = std::fs::read_dir(&runs).map(|r| r.count()).unwrap_or(0);
    assert_eq!(count, 0, "dry run must not persist a record");
}

#[test]
fn missing_roster_reports_error() {
    let fx = Fx::new();
    fx.install_fake_codex();
    fx.write_home_config(sample_home_config());

    let out = fx.run(&["harness", "codex", "effective-config", "nope"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("no codex roster with id"));
}
