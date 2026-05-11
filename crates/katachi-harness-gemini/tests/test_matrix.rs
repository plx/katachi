//! Test matrix: covers the scenarios called out in the spec's
//! "Acceptance criteria" and "Validation rules" — one end-to-end
//! fixture test per scenario, exercising the full scan → resolve →
//! validate → plan pipeline.

use std::fs;
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use tempfile::TempDir;

use katachi_core::diagnostic::{any_error, Diagnostic};
use katachi_core::error::PlanError;
use katachi_core::harness::{HarnessModule, PlanContext, ScanContext};
use katachi_core::katachi::KatachiDefinition;
use katachi_core::model::{BackendKind, HarnessKind, ItemRef, MaterializationMode};
use katachi_core::paths::{PathSource, ResolvedPath, StoragePaths};
use katachi_core::plan::{ActionRequest, InvocationRequest};
use katachi_core::record::RunId;
use katachi_core::resolve::{resolve, ResolveInputs};
use katachi_core::roster::EdgeKind;
use katachi_core::validate::{default_validators, run_validators, ValidateContext, Validator};

use katachi_harness_gemini::config::GeminiConfig;
use katachi_harness_gemini::item::GeminiItemKind;
use katachi_harness_gemini::policy::ResolvedPolicy;
use katachi_harness_gemini::roster::GeminiRoster;
use katachi_harness_gemini::scan::scan_gemini;
use katachi_harness_gemini::transcript;
use katachi_harness_gemini::validate::gemini_validators;
use katachi_harness_gemini::GeminiHarness;

/// Build a fully-stocked fixture environment and return its config +
/// root cwd.
struct Fixture {
    _tmp: TempDir,
    cfg: GeminiConfig,
    cwd: Utf8PathBuf,
}

impl Fixture {
    fn with<F>(build: F) -> Self
    where
        F: FnOnce(&Utf8Path),
    {
        let tmp = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let home = root.join("home");
        let ext_root = home.join("extensions");
        let project = root.join("project");
        fs::create_dir_all(home.as_std_path()).unwrap();
        fs::create_dir_all(ext_root.as_std_path()).unwrap();
        fs::create_dir_all(project.as_std_path()).unwrap();

        build(&root);

        let cfg = GeminiConfig {
            home: Some(home.clone()),
            user_roots: vec![home],
            project_roots: vec![project.clone()],
            extension_roots: vec![ext_root],
            ..GeminiConfig::default()
        };
        Self {
            _tmp: tmp,
            cfg,
            cwd: project,
        }
    }
}

fn write(path: &Utf8Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent.as_std_path()).unwrap();
    }
    fs::write(path.as_std_path(), body).unwrap();
}

fn default_storage() -> StoragePaths {
    let p = Utf8PathBuf::from("/tmp/katachi-test-matrix");
    StoragePaths {
        data_root: ResolvedPath {
            path: p.clone(),
            source: PathSource::XdgDefault,
        },
        cache_root: ResolvedPath {
            path: p,
            source: PathSource::XdgDefault,
        },
    }
}

fn resolve_roster(
    fx: &Fixture,
    roster_toml: &str,
    action: ActionRequest,
) -> (KatachiDefinition, katachi_core::resolve::ResolveOutput) {
    let harness = GeminiHarness::new();
    let catalog = harness
        .scan(&ScanContext {
            config: &katachi_core::config::KatachiConfig::default(),
            paths: &default_storage(),
            cwd: &fx.cwd,
        })
        .unwrap();
    let _ = catalog; // consumed later via resolver.scan via modules
    let roster = GeminiRoster::from_toml_str(roster_toml).unwrap();
    let def = roster.to_katachi_definition();
    let mut req = InvocationRequest::new(&def.id, action, fx.cwd.clone());
    req.materialization = MaterializationMode::Ambient;

    // Inject the Gemini config into the KatachiConfig so the harness
    // picks up the fixture roots.
    let mut kcfg = katachi_core::config::KatachiConfig::default();
    let mut extra: indexmap::IndexMap<String, toml::Value> = Default::default();
    if let Some(h) = &fx.cfg.home {
        extra.insert("home".into(), toml::Value::String(h.to_string()));
    }
    extra.insert(
        "user_roots".into(),
        toml::Value::Array(
            fx.cfg
                .user_roots
                .iter()
                .map(|p| toml::Value::String(p.to_string()))
                .collect(),
        ),
    );
    extra.insert(
        "project_roots".into(),
        toml::Value::Array(
            fx.cfg
                .project_roots
                .iter()
                .map(|p| toml::Value::String(p.to_string()))
                .collect(),
        ),
    );
    extra.insert(
        "extension_roots".into(),
        toml::Value::Array(
            fx.cfg
                .extension_roots
                .iter()
                .map(|p| toml::Value::String(p.to_string()))
                .collect(),
        ),
    );
    kcfg.harnesses.insert(
        "gemini".into(),
        katachi_core::config::HarnessConfig {
            enabled: true,
            binary: None,
            default_backend: None,
            extra,
        },
    );
    let storage = default_storage();
    let modules: Vec<&dyn HarnessModule> = vec![&harness];
    let inputs = ResolveInputs::new(&req, &def, &modules, &kcfg, &storage, &fx.cwd);
    let out = resolve(inputs).unwrap();
    (def, out)
}

// ---------------- 1. extension containment ----------------

#[test]
fn extension_containment_emits_packaging_edges() {
    let fx = Fixture::with(|root| {
        let ext = root.join("home/extensions/workspace-a11y");
        write(
            &ext.join("gemini-extension.json"),
            r#"{"name": "workspace-a11y", "version": "0.1.0"}"#,
        );
        write(
            &ext.join("skills/a11y-audit.md"),
            "---\ndescription: a11y audit\n---\nbody\n",
        );
        write(
            &ext.join("agents/explorer.md"),
            "---\ndescription: explorer\n---\n",
        );
    });
    let cat = scan_gemini(&fx.cfg, &fx.cwd);
    let ext_ref = ItemRef::new(HarnessKind::Gemini, "extension", "workspace-a11y");
    let skill_ref = ItemRef::new(HarnessKind::Gemini, "skill", "a11y-audit");
    let agent_ref = ItemRef::new(HarnessKind::Gemini, "subagent", "explorer");
    assert!(cat.contains(&ext_ref));
    assert!(cat.contains(&skill_ref));
    assert!(cat.contains(&agent_ref));

    let packaging: Vec<_> = cat
        .edges_from(&ext_ref)
        .filter(|e| e.kind == EdgeKind::Packaging)
        .collect();
    assert!(
        packaging.len() >= 2,
        "expected packaging edges for skill + subagent, got {packaging:?}"
    );
}

// ---------------- 2. settings overrides ----------------

#[test]
fn settings_overrides_between_user_and_project() {
    let fx = Fixture::with(|root| {
        write(
            &root.join("home/settings.json"),
            r#"{"model": "gemini-3-flash", "contextFileName": "GEMINI.md"}"#,
        );
        write(
            &root.join("project/.gemini/settings.json"),
            r#"{"model": "gemini-3-pro"}"#,
        );
    });
    let cat = scan_gemini(&fx.cfg, &fx.cwd);
    let user = cat
        .get(&ItemRef::new(
            HarnessKind::Gemini,
            "settings_layer",
            "settings:user",
        ))
        .unwrap();
    let proj = cat
        .get(&ItemRef::new(
            HarnessKind::Gemini,
            "settings_layer",
            "settings:project",
        ))
        .unwrap();
    assert_eq!(user.raw["model"], "gemini-3-flash");
    assert_eq!(proj.raw["model"], "gemini-3-pro");
}

// ---------------- 3. MCP name conflicts ----------------

#[test]
fn settings_wins_over_extension_mcp_same_name() {
    let fx = Fixture::with(|root| {
        let ext = root.join("home/extensions/browser");
        write(
            &ext.join("gemini-extension.json"),
            r#"{"name": "browser", "mcpServers": {"chrome": {"command": "ext"}}}"#,
        );
        write(
            &root.join("project/.gemini/settings.json"),
            r#"{"mcpServers": {"chrome": {"command": "override"}}}"#,
        );
    });
    let cat = scan_gemini(&fx.cfg, &fx.cwd);
    let projection_edges: Vec<_> = cat
        .iter_edges()
        .filter(|e| e.kind == EdgeKind::Projection)
        .collect();
    assert!(
        !projection_edges.is_empty(),
        "expected a settings-wins projection edge"
    );

    // Also verify both mcp items coexist in the catalog, differentiated
    // by scope-prefixed ids.
    let ext_mcp = ItemRef::new(HarnessKind::Gemini, "mcp_server", "ext:browser:chrome");
    let settings_mcp = ItemRef::new(HarnessKind::Gemini, "mcp_server", "settings:project:chrome");
    assert!(cat.contains(&ext_mcp));
    assert!(cat.contains(&settings_mcp));
}

// ---------------- 4. admin/security restrictions ----------------

#[test]
fn admin_disable_extensions_rejects_loadout() {
    let fx = Fixture::with(|root| {
        let ext = root.join("home/extensions/anything");
        write(
            &ext.join("gemini-extension.json"),
            r#"{"name": "anything"}"#,
        );
        write(
            &root.join("home/settings.json"),
            r#"{"admin": {"disableExtensions": true}}"#,
        );
    });
    let cat = scan_gemini(&fx.cfg, &fx.cwd);
    let policy = ResolvedPolicy::from_catalog(&cat);
    assert!(policy.extensions_disabled);

    let roster = r#"
id = "t"
description = "disabled-ext test"

[selection]
extensions = ["anything"]

[run_profile]
backend = "cli"
"#;
    let (definition, out) =
        resolve_roster(&fx, roster, ActionRequest::Execute { prompt: "x".into() });

    let mut validators: Vec<Arc<dyn Validator>> = default_validators();
    validators.extend(gemini_validators(policy));
    let diags = run_validators(
        &ValidateContext {
            resolved: &out.resolved,
            catalog: &out.catalog,
            definition: &definition,
        },
        &validators,
    );
    assert!(any_error(&diags));
    assert!(diags
        .iter()
        .any(|d| d.code == "gemini.policy.extensions-disabled"));
}

// ---------------- 5. preview feature requirements ----------------

#[test]
fn preview_required_subagent_fails_without_flag() {
    let fx = Fixture::with(|root| {
        let ext = root.join("home/extensions/ships-preview");
        write(
            &ext.join("gemini-extension.json"),
            r#"{"name": "ships-preview"}"#,
        );
        write(
            &ext.join("agents/explorer.md"),
            "---\ndescription: explore\nexperimental: true\n---\n",
        );
    });
    let cat = scan_gemini(&fx.cfg, &fx.cwd);
    let policy = ResolvedPolicy::from_catalog(&cat);
    assert!(!policy.preview_features_enabled);

    let roster = r#"
id = "t"
[selection]
extensions = ["ships-preview"]
subagents = ["explorer"]

[run_profile]
backend = "cli"
"#;
    let (definition, out) =
        resolve_roster(&fx, roster, ActionRequest::Execute { prompt: "x".into() });

    let mut validators: Vec<Arc<dyn Validator>> = default_validators();
    validators.extend(gemini_validators(policy));
    let diags = run_validators(
        &ValidateContext {
            resolved: &out.resolved,
            catalog: &out.catalog,
            definition: &definition,
        },
        &validators,
    );
    assert!(diags.iter().any(|d| d.code == "gemini.preview.required"));
}

#[test]
fn preview_required_subagent_passes_when_flag_enabled() {
    let fx = Fixture::with(|root| {
        let ext = root.join("home/extensions/ships-preview");
        write(
            &ext.join("gemini-extension.json"),
            r#"{"name": "ships-preview"}"#,
        );
        write(
            &ext.join("agents/explorer.md"),
            "---\ndescription: explore\nexperimental: true\n---\n",
        );
        write(
            &root.join("home/settings.json"),
            r#"{"experimental": true}"#,
        );
    });
    let cat = scan_gemini(&fx.cfg, &fx.cwd);
    let policy = ResolvedPolicy::from_catalog(&cat);
    assert!(policy.preview_features_enabled);

    let roster = r#"
id = "t"
[selection]
extensions = ["ships-preview"]
subagents = ["explorer"]

[run_profile]
backend = "cli"
"#;
    let (definition, out) =
        resolve_roster(&fx, roster, ActionRequest::Execute { prompt: "x".into() });
    let mut validators: Vec<Arc<dyn Validator>> = default_validators();
    validators.extend(gemini_validators(policy));
    let diags = run_validators(
        &ValidateContext {
            resolved: &out.resolved,
            catalog: &out.catalog,
            definition: &definition,
        },
        &validators,
    );
    assert!(
        !diags.iter().any(|d| d.code == "gemini.preview.required"),
        "preview flag is enabled, so no preview validation error should fire: {diags:?}"
    );
}

// ---------------- 6. unsupported SDK projection ----------------

#[test]
fn sdk_ts_projection_rejects_extension_selection() {
    let fx = Fixture::with(|root| {
        let ext = root.join("home/extensions/workspace-a11y");
        write(
            &ext.join("gemini-extension.json"),
            r#"{"name": "workspace-a11y"}"#,
        );
    });
    let roster = r#"
id = "t"
[selection]
extensions = ["workspace-a11y"]

[run_profile]
backend = "sdk-ts"
"#;
    let (_def, out) = resolve_roster(&fx, roster, ActionRequest::Execute { prompt: "x".into() });
    let harness = GeminiHarness::new();
    let req = InvocationRequest::new(
        "t",
        ActionRequest::Execute { prompt: "x".into() },
        fx.cwd.clone(),
    );
    let err = harness
        .plan(&PlanContext {
            request: &req,
            resolved: &out.resolved,
            run_id: RunId::new(),
        })
        .unwrap_err();
    match err {
        PlanError::ProjectionLoss { backend, reason } => {
            assert_eq!(backend, "sdk-ts");
            assert!(reason.contains("extensions"));
        }
        other => panic!("unexpected error: {other}"),
    }
}

// ---------------- 7. stream-json transcript parsing ----------------

#[test]
fn stream_json_events_project_to_typed_kinds() {
    let line = r#"{"type": "message", "role": "assistant", "text": "hi"}"#;
    let events = transcript::parse_line(line);
    assert_eq!(events.len(), 1);
    match &events[0] {
        katachi_core::transcript::EventKind::AssistantMessage { text } => {
            assert_eq!(text, "hi");
        }
        other => panic!("wrong event {other:?}"),
    }
}

#[test]
fn stream_json_unknown_event_preserves_raw_payload() {
    let line = r#"{"type": "future-kind", "novel": true}"#;
    let events = transcript::parse_line(line);
    assert!(matches!(
        events[0],
        katachi_core::transcript::EventKind::JsonEvent { .. }
    ));
}

// ---------------- misc: resolver closes packaging to skill ----------------

#[test]
fn resolver_closes_extension_into_skill_via_packaging() {
    let fx = Fixture::with(|root| {
        let ext = root.join("home/extensions/pkg");
        write(&ext.join("gemini-extension.json"), r#"{"name": "pkg"}"#);
        write(
            &ext.join("skills/audit.md"),
            "---\ndescription: audit\n---\n",
        );
    });
    let roster = r#"
id = "t"
[selection]
extensions = ["pkg"]

[run_profile]
backend = "cli"

[resolution]
include_transitive = true
"#;
    let (_def, out) = resolve_roster(&fx, roster, ActionRequest::Execute { prompt: "x".into() });
    let ids: Vec<&str> = out
        .resolved
        .selected_items
        .iter()
        .map(|i| i.item.id.as_str())
        .collect();
    assert!(ids.contains(&"pkg"));
    assert!(ids.contains(&"audit"));
}

// ---------------- GeminiItemKind stable wire format ----------------

#[test]
fn item_kinds_match_expected_wire_names() {
    assert_eq!(GeminiItemKind::Extension.as_str(), "extension");
    assert_eq!(GeminiItemKind::Skill.as_str(), "skill");
    assert_eq!(GeminiItemKind::Subagent.as_str(), "subagent");
    assert_eq!(GeminiItemKind::HookSet.as_str(), "hook_set");
    assert_eq!(GeminiItemKind::McpServer.as_str(), "mcp_server");
    assert_eq!(GeminiItemKind::PolicySet.as_str(), "policy_set");
    assert_eq!(GeminiItemKind::SettingsLayer.as_str(), "settings_layer");
    assert_eq!(GeminiItemKind::ContextSource.as_str(), "context_source");
    assert_eq!(GeminiItemKind::RunProfile.as_str(), "run_profile");
}

// Silence unused-import warnings when adding more cases incrementally.
#[allow(dead_code)]
fn _imports_check() {
    let _: BackendKind = BackendKind::SdkPy;
    let _: Diagnostic = Diagnostic::info("x", "y");
}
