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

struct CodexFx {
    _td: TempDir,
    root: std::path::PathBuf,
    config_path: std::path::PathBuf,
    data_root: std::path::PathBuf,
}

impl CodexFx {
    fn new() -> Self {
        let td = TempDir::new().unwrap();
        let root = td.path().to_path_buf();
        let data_root = root.join("data");
        fs::create_dir_all(data_root.join("katachis")).unwrap();
        fs::create_dir_all(data_root.join("rosters/codex")).unwrap();
        let codex_home = root.join("codex-home");
        fs::create_dir_all(&codex_home).unwrap();
        fs::write(
            codex_home.join("config.toml"),
            r#"model = "gpt-5.4"

[profiles.review]
approval_policy = "never"
sandbox_mode = "read-only"
"#,
        )
        .unwrap();
        let fake = root.join("fake-codex.sh");
        fs::write(&fake, "#!/bin/sh\necho FAKE_CODEX_HAVE\nexit 0\n").unwrap();
        let mut perms = fs::metadata(&fake).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake, perms).unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"version = 1

[harnesses.claude]
enabled = false

[harnesses.codex]
enabled = true
binary = "{}"
codex_home = "{}"
project_roots = ["."]
respect_project_trust = false

[harnesses.gemini]
enabled = false
"#,
                fake.display(),
                codex_home.display()
            ),
        )
        .unwrap();
        fs::write(
            data_root.join("rosters/codex/audit.toml"),
            r#"
version = 1
id = "audit"

[selection]
profiles = ["review"]

[run_profile]
backend = "cli"
approval_policy = "never"
sandbox_mode = "read-only"
model = "gpt-5.4"
"#,
        )
        .unwrap();
        Self {
            _td: td,
            root,
            config_path,
            data_root,
        }
    }

    fn write_katachi(&self, name: &str, body: &str) {
        fs::write(
            self.data_root.join("katachis").join(format!("{name}.toml")),
            body,
        )
        .unwrap();
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

struct GeminiFx {
    _td: TempDir,
    root: std::path::PathBuf,
    config_path: std::path::PathBuf,
    data_root: std::path::PathBuf,
}

impl GeminiFx {
    fn new() -> Self {
        let td = TempDir::new().unwrap();
        let root = td.path().to_path_buf();
        let data_root = root.join("data");
        fs::create_dir_all(data_root.join("katachis")).unwrap();
        fs::create_dir_all(data_root.join("rosters/gemini")).unwrap();
        let home = root.join("gemini-home");
        let ext_root = home.join("extensions");
        fs::create_dir_all(&ext_root).unwrap();
        let fake = root.join("fake-gemini");
        fs::write(
            &fake,
            "#!/bin/sh\necho '{\"type\":\"result\",\"summary\":\"done\",\"outcome\":\"success\"}'\nexit 0\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&fake).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake, perms).unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"version = 1

[harnesses.claude]
enabled = false

[harnesses.codex]
enabled = false

[harnesses.gemini]
enabled = true
binary = "fake-gemini"
home = "{}"
user_roots = ["{}"]
project_roots = ["{}"]
extension_roots = ["{}"]
"#,
                home.display(),
                home.display(),
                root.display(),
                ext_root.display()
            ),
        )
        .unwrap();
        fs::write(
            data_root.join("rosters/gemini/demo.toml"),
            r#"
version = 1
id = "demo"

[selection]

[run_profile]
backend = "cli"
model = "gemini-3-pro"
approval_mode = "plan"
output_format = "stream-json"
binary = "fake-gemini"
"#,
        )
        .unwrap();
        Self {
            _td: td,
            root,
            config_path,
            data_root,
        }
    }

    fn write_katachi(&self, name: &str, body: &str) {
        fs::write(
            self.data_root.join("katachis").join(format!("{name}.toml")),
            body,
        )
        .unwrap();
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = katachi_bin();
        let path = format!(
            "{}:{}",
            self.root.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.env_remove("RUST_LOG")
            .env_remove("KATACHI_FIXTURE_HARNESSES")
            .env("KATACHI_CONFIG", &self.config_path)
            .env("KATACHI_DATA", &self.data_root)
            .env("KATACHI_CACHE", self.data_root.join("cache"))
            .env("PATH", path)
            .args(["--cwd", self.root.to_str().unwrap()])
            .args(args);
        cmd.output().unwrap()
    }
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
    assert!(
        stdout.contains("--print"),
        "plan should include claude argv: {stdout}"
    );
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
fn have_execute_without_roster_id_executes_selector_only_target() {
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
    assert!(
        out.status.success(),
        "expected selector-only target to execute; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn have_codex_plan_and_execute_work_for_roster_and_selector_targets() {
    let fx = CodexFx::new();
    fx.write_katachi(
        "audit",
        r#"
id = "audit"

[[targets]]
harness = "codex"
roster_id = "audit"
"#,
    );
    fx.write_katachi(
        "raw",
        r#"
id = "raw"

[[targets]]
harness = "codex"
backend = "cli"
run_profile_overlay = { approval_policy = "never", sandbox_mode = "read-only", model = "gpt-5.4" }

[[targets.selectors.selectors]]
type = "glob"
kind = "profile"
pattern = "*review"
"#,
    );

    let plan = fx.run(&["have", "audit", "plan", "execute", "hi"]);
    assert!(
        plan.status.success(),
        "roster plan stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&plan.stdout),
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan_stdout = String::from_utf8(plan.stdout).unwrap();
    assert!(plan_stdout.contains("fake-codex.sh"), "{plan_stdout}");

    let exec = fx.run(&["have", "raw", "execute", "hi"]);
    assert!(
        exec.status.success(),
        "selector execute stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&exec.stdout),
        String::from_utf8_lossy(&exec.stderr)
    );
    let runs = fs::read_dir(fx.data_root.join("runs")).unwrap();
    assert_eq!(runs.count(), 1);
}

#[test]
fn have_codex_target_overrides_roster_backend_and_run_profile() {
    let fx = CodexFx::new();
    fs::write(
        fx.data_root.join("rosters/codex/audit.toml"),
        r#"
version = 1
id = "audit"

[selection]
profiles = ["review"]

[run_profile]
backend = "sdk-ts"
approval_policy = "never"
sandbox_mode = "read-only"
model = "roster-model"
"#,
    )
    .unwrap();
    fx.write_katachi(
        "audit",
        r#"
id = "audit"

[[targets]]
harness = "codex"
roster_id = "audit"
backend = "cli"
run_profile_overlay = { model = "target-model" }
"#,
    );

    let out = fx.run(&["--json", "have", "audit", "plan", "execute", "hi"]);
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["backend"], "cli");
    let argv = v["plan"]["execution"]["argv"].as_array().unwrap();
    assert!(
        argv.iter().any(|arg| arg == "target-model"),
        "argv missing target model override: {argv:?}"
    );
    assert!(
        !argv.iter().any(|arg| arg == "roster-model"),
        "argv should not contain roster model after override: {argv:?}"
    );
}

#[test]
fn have_gemini_plan_and_execute_work_for_roster_and_selector_targets() {
    let fx = GeminiFx::new();
    fx.write_katachi(
        "demo",
        r#"
id = "demo"

[[targets]]
harness = "gemini"
roster_id = "demo"
"#,
    );
    fx.write_katachi(
        "raw",
        r#"
id = "raw"

[[targets]]
harness = "gemini"
backend = "cli"
run_profile_overlay = { model = "gemini-3-pro", approval_mode = "plan", output_format = "stream-json", binary = "fake-gemini" }
"#,
    );

    let plan = fx.run(&["have", "demo", "plan", "execute", "hi"]);
    assert!(
        plan.status.success(),
        "roster plan stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&plan.stdout),
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan_stdout = String::from_utf8(plan.stdout).unwrap();
    assert!(plan_stdout.contains("fake-gemini"), "{plan_stdout}");

    let exec = fx.run(&["have", "raw", "execute", "hi"]);
    assert!(
        exec.status.success(),
        "selector execute stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&exec.stdout),
        String::from_utf8_lossy(&exec.stderr)
    );
    let runs = fs::read_dir(fx.data_root.join("runs")).unwrap();
    assert_eq!(runs.count(), 1);
}
