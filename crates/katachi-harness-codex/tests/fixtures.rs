//! Fixture-based integration tests for the Codex harness.
//!
//! Each test builds a filesystem fixture that mimics a real Codex
//! installation/project layout, then drives the discovery + effective
//! config pipeline and asserts the outcomes the spec requires.

use camino::{Utf8Path, Utf8PathBuf};
use tempfile::TempDir;

use katachi_harness_codex::agents::discover_agents;
use katachi_harness_codex::config_layers::{discover_config_layers, ConfigSource};
use katachi_harness_codex::discovery::{discover, resolve_project_roots, DiscoveryInputs};
use katachi_harness_codex::effective::{build_effective, BuildEffective};
use katachi_harness_codex::hooks::discover_hooks;
use katachi_harness_codex::legality;
use katachi_harness_codex::mcp::discover_mcp_servers;
use katachi_harness_codex::plugins::discover_plugins;
use katachi_harness_codex::projection;
use katachi_harness_codex::roster::discover_instruction_chain;
use katachi_harness_codex::roster_file::{CodexRosterFile, RunProfile};
use katachi_harness_codex::rules::discover_rules;
use katachi_harness_codex::skills::discover_skills;
use katachi_harness_codex::CodexSettings;

use katachi_core::diagnostic::Severity;
use katachi_core::model::{BackendKind, HarnessKind};

struct Fx {
    _td: TempDir,
    root: Utf8PathBuf,
}

impl Fx {
    fn new() -> Self {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        Self { _td: td, root }
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn join(&self, rel: &str) -> Utf8PathBuf {
        self.root.join(rel)
    }

    fn settings(&self) -> CodexSettings {
        CodexSettings {
            codex_home: self.root.join("home"),
            project_roots: vec![Utf8PathBuf::from(".")],
            marketplace_roots: vec![self.root.join("market")],
            respect_project_trust: false,
            preserve_failed_overlays: true,
            enable_python_sdk: false,
            ..CodexSettings::default()
        }
    }
}

#[test]
fn trusted_vs_untrusted_project_behaviour() {
    let fx = Fx::new();
    fx.write("home/config.toml", r#"model = "gpt-5.4""#);
    fx.write("project/.codex/config.toml", r#"approval_policy = "never""#);

    // Case 1: trust required -> project layer is discovered but inactive.
    let mut settings = fx.settings();
    settings.respect_project_trust = true;
    let mut diags = Vec::new();
    let layers = discover_config_layers(
        &settings,
        &[fx.join("project")],
        &fx.join("project"),
        &mut diags,
    );
    assert_eq!(layers.len(), 2);
    let project = layers
        .iter()
        .find(|l| l.source == ConfigSource::Project)
        .unwrap();
    assert!(!project.active, "project layer should be inactive");

    // Case 2: trust disabled -> both layers active.
    let mut settings = fx.settings();
    settings.respect_project_trust = false;
    let mut diags = Vec::new();
    let layers = discover_config_layers(
        &settings,
        &[fx.join("project")],
        &fx.join("project"),
        &mut diags,
    );
    let project = layers
        .iter()
        .find(|l| l.source == ConfigSource::Project)
        .unwrap();
    assert!(project.active, "project layer should be active");
}

#[test]
fn nested_project_configs_stack_root_to_cwd() {
    let fx = Fx::new();
    let project = fx.join("project");
    fx.write("project/.codex/config.toml", r#"model = "root""#);
    fx.write("project/a/.codex/config.toml", r#"model = "a""#);
    fx.write("project/a/b/.codex/config.toml", r#"model = "b""#);

    let settings = fx.settings();
    let cwd = project.join("a/b");
    let mut diags = Vec::new();
    let layers =
        discover_config_layers(&settings, std::slice::from_ref(&project), &cwd, &mut diags);
    let project_layers: Vec<_> = layers
        .iter()
        .filter(|l| l.source == ConfigSource::Project)
        .collect();
    assert_eq!(
        project_layers.len(),
        3,
        "expected three nested project layers: {project_layers:?}"
    );
    // Deepest (cwd) has highest precedence.
    let precedences: Vec<u32> = project_layers.iter().map(|l| l.precedence).collect();
    assert!(
        precedences.windows(2).all(|w| w[0] <= w[1]),
        "precedence must be monotonically increasing"
    );

    // Effective merge: deepest wins.
    let eff = build_effective(BuildEffective {
        layers: &layers,
        instructions: &[],
        hooks: &[],
        rules: &[],
        mcps: &[],
        skills: &[],
        agents: &[],
        run_profile: RunProfile::default(),
        active_profile: None,
        only_active: true,
    });
    assert_eq!(
        eff.merged_config["model"].as_str(),
        Some("b"),
        "deepest layer must win"
    );
}

#[test]
fn layered_agents_md_preserved_in_order() {
    let fx = Fx::new();
    fx.write("home/AGENTS.md", "global\n");
    fx.write("project/AGENTS.md", "project-root\n");
    fx.write("project/sub/AGENTS.md", "project-sub\n");
    fx.write("project/.codex/config.toml", r#"model = "gpt-5.4""#);

    let settings = fx.settings();
    let roots = resolve_project_roots(&settings.project_roots, &fx.join("project"));
    let mut diags = Vec::new();
    let chain = discover_instruction_chain(
        &settings,
        &[fx.join("project")],
        &fx.join("project/sub"),
        &mut diags,
    );
    let _ = roots;
    let bodies: Vec<_> = chain.iter().map(|d| d.body.trim().to_string()).collect();
    assert_eq!(
        bodies,
        vec!["global", "project-root", "project-sub"],
        "instruction chain order mismatch"
    );
}

#[test]
fn hooks_are_additive_across_layers() {
    let fx = Fx::new();
    fx.write("home/config.toml", "a = 1");
    fx.write("home/hooks.json", r#"{"before_tool":[{"match":"*"}]}"#);
    fx.write("project/.codex/config.toml", "b = 2");
    fx.write(
        "project/.codex/hooks.json",
        r#"{"after_tool":[{"match":"*"}]}"#,
    );

    let settings = fx.settings();
    let mut diags = Vec::new();
    let layers = discover_config_layers(
        &settings,
        &[fx.join("project")],
        &fx.join("project"),
        &mut diags,
    );
    let hooks = discover_hooks(&layers, &mut diags);
    assert_eq!(hooks.len(), 2, "hooks must be additive: {hooks:?}");

    let eff = build_effective(BuildEffective {
        layers: &layers,
        instructions: &[],
        hooks: &hooks,
        rules: &[],
        mcps: &[],
        skills: &[],
        agents: &[],
        run_profile: RunProfile::default(),
        active_profile: None,
        only_active: true,
    });
    assert_eq!(eff.hooks.len(), 2);
}

#[test]
fn admin_rule_conflict_surfaces_warning() {
    let fx = Fx::new();
    fx.write("home/config.toml", "a = 1");
    // System-tier rule that mentions require_approval.
    std::fs::create_dir_all(fx.join("home/rules").as_std_path()).unwrap();
    fx.write("home/rules/readonly.md", "rule: require_approval\n");

    let settings = fx.settings();
    let mut diags = Vec::new();
    let layers = discover_config_layers(
        &settings,
        &[fx.join("project")],
        &fx.join("project"),
        &mut diags,
    );
    let rules = discover_rules(&layers, &mut diags);
    let mut eff = build_effective(BuildEffective {
        layers: &layers,
        instructions: &[],
        hooks: &[],
        rules: &rules,
        mcps: &[],
        skills: &[],
        agents: &[],
        run_profile: RunProfile::default(),
        active_profile: None,
        only_active: true,
    });
    // Mark the rule as admin-tier so the legality validator's admin-rule
    // heuristic kicks in.
    for r in &mut eff.rules {
        r.tier = "admin".into();
    }
    eff.policy.approval_policy = Some("never".into());

    let diags = legality::validate_requirements(&eff);
    assert!(
        diags.iter().any(|d| d.code == "codex.legality.admin-rule"),
        "expected admin-rule diagnostic: {diags:?}"
    );
}

#[test]
fn requirements_violation_is_blocking() {
    let fx = Fx::new();
    fx.write(
        "home/config.toml",
        r#"
approval_policy = "never"
[requirements]
forbid_approval_policies = ["never"]
"#,
    );

    let settings = fx.settings();
    let mut diags = Vec::new();
    let layers = discover_config_layers(
        &settings,
        &[fx.join("project")],
        &fx.join("project"),
        &mut diags,
    );
    let eff = build_effective(BuildEffective {
        layers: &layers,
        instructions: &[],
        hooks: &[],
        rules: &[],
        mcps: &[],
        skills: &[],
        agents: &[],
        run_profile: RunProfile::default(),
        active_profile: None,
        only_active: true,
    });
    let diags = legality::validate_requirements(&eff);
    assert!(diags
        .iter()
        .any(|d| d.code == "codex.legality.requirements" && d.severity == Severity::Error));
}

#[test]
fn plugin_packaging_discovered_with_edges() {
    let fx = Fx::new();
    fx.write(
        "market/accessibility/plugin.toml",
        r#"id = "accessibility"
name = "Accessibility"
"#,
    );
    fx.write("market/accessibility/skills/axe/SKILL.md", "axe skill\n");
    fx.write(
        "market/accessibility/agents/reviewer/agent.toml",
        r#"model = "gpt""#,
    );

    let settings = fx.settings();
    let mut diags = Vec::new();
    let plugins = discover_plugins(&settings, &mut diags);
    assert_eq!(plugins.len(), 1);
    let packaged = plugins[0].packaged_items();
    assert_eq!(packaged.len(), 2);
    for (item, edge) in &packaged {
        assert!(item.packaging.is_some(), "packaged items carry pkg ref");
        assert_eq!(
            edge.kind,
            katachi_core::roster::EdgeKind::Packaging,
            "plugin edges must be Packaging"
        );
    }
}

#[test]
fn missing_mcp_requirement_is_blocking() {
    let fx = Fx::new();
    fx.write("home/config.toml", r#"model = "gpt-5.4""#);
    fx.write(
        ".codex/skills/axe/SKILL.md",
        r#"---
mcp_requirements: [does-not-exist]
---
"#,
    );

    let settings = fx.settings();
    let mut diags = Vec::new();
    let layers = discover_config_layers(
        &settings,
        std::slice::from_ref(&fx.root),
        &fx.root,
        &mut diags,
    );
    let skills = discover_skills(&settings, std::slice::from_ref(&fx.root), &mut diags);
    let mcps = discover_mcp_servers(&layers, &mut diags);
    let eff = build_effective(BuildEffective {
        layers: &layers,
        instructions: &[],
        hooks: &[],
        rules: &[],
        mcps: &mcps,
        skills: &skills,
        agents: &[],
        run_profile: RunProfile::default(),
        active_profile: None,
        only_active: true,
    });
    let diags = legality::validate_missing_dependencies(&eff);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, "codex.legality.missing-mcp");
}

#[test]
fn backend_projection_sdk_py_disabled_blocks() {
    let fx = Fx::new();
    fx.write("home/config.toml", "model = \"gpt\"");
    let settings = fx.settings(); // enable_python_sdk = false
    let mut diags = Vec::new();
    let layers = discover_config_layers(
        &settings,
        std::slice::from_ref(&fx.root),
        &fx.root,
        &mut diags,
    );
    let eff = build_effective(BuildEffective {
        layers: &layers,
        instructions: &[],
        hooks: &[],
        rules: &[],
        mcps: &[],
        skills: &[],
        agents: &[],
        run_profile: RunProfile::default(),
        active_profile: None,
        only_active: true,
    });
    let mut roster = roster_placeholder();
    roster.run_profile.backend = Some("sdk-py".into());
    let diags = projection::analyze(BackendKind::SdkPy, &roster, &eff, &settings);
    assert!(projection::has_blocking(&diags));
}

#[test]
fn full_discovery_emits_catalog_items_across_kinds() {
    let fx = Fx::new();
    fx.write(
        "home/config.toml",
        r#"
model = "gpt-5.4"

[profiles.review]
approval_policy = "never"

[mcp_servers.chrome]
command = "chrome-mcp"
"#,
    );
    fx.write("home/AGENTS.md", "global\n");
    fx.write(
        ".codex/skills/axe/SKILL.md",
        r#"---
mcp_requirements: [chrome]
---
"#,
    );
    fx.write(".codex/agents/reviewer/agent.toml", r#"model = "gpt""#);

    let settings = fx.settings();
    let catalog = discover(&DiscoveryInputs {
        settings,
        cwd: fx.root.clone(),
    })
    .unwrap();

    let kinds: std::collections::HashSet<String> =
        catalog.iter_items().map(|(i, _)| i.kind.clone()).collect();
    for expected in [
        "config_layer",
        "profile",
        "instruction_doc",
        "skill",
        "custom_agent",
        "mcp_server",
    ] {
        assert!(
            kinds.contains(expected),
            "expected kind `{expected}` in catalog: {kinds:?}"
        );
    }
    // The skill->MCP edge should be present.
    assert!(
        catalog
            .iter_edges()
            .any(|e| e.note.as_deref() == Some("skill_requires_mcp")),
        "expected skill -> MCP edge"
    );

    // Confirm harness attribution.
    for (i, _) in catalog.iter_items() {
        assert_eq!(i.harness, HarnessKind::Codex);
    }
}

fn roster_placeholder() -> CodexRosterFile {
    CodexRosterFile {
        version: 1,
        id: "r".into(),
        description: None,
        selection: Default::default(),
        run_profile: RunProfile::default(),
        resolution: Default::default(),
    }
}

// Use everything so `cargo test` doesn't warn about unused items during
// development. Marking these as used via a trivial helper test.
#[test]
fn all_helpers_compile() {
    let fx = Fx::new();
    let _ = fx.settings();
    let _ = discover_agents(
        &fx.settings(),
        std::slice::from_ref(&fx.root),
        &mut Vec::new(),
    );
    let _: &Utf8Path = fx.root.as_ref();
}
