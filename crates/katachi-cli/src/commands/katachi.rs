//! `katachi katachi list/show/validate` — admin commands for the
//! katachi-definitions store.

use std::collections::BTreeMap;
use std::fs;

use anyhow::Result;
use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;

use katachi_core::config::{self, KatachiConfig};
use katachi_core::diagnostic::{any_error, Diagnostic};
use katachi_core::katachi::{KatachiDefinition, KatachiStore, KatachiStoreError, KatachiTarget};
use katachi_core::model::{BackendKind, HarnessKind};
use katachi_core::paths::{
    resolve_config_file, resolve_storage_paths, PathOverrides, StoragePaths,
};
use katachi_core::plan::{ActionRequest, InvocationRequest, ResolvedKatachi};
use katachi_core::resolve::{resolve, ResolveInputs, ResolveOutput};
use katachi_core::validate::{default_validators, run_validators, ValidateContext};

use katachi_harness_claude::resolve::{validate as validate_claude_roster, ResolvedClaudeRoster};
use katachi_harness_claude::roster::{
    ClaudeRoster, RosterResolution as ClaudeRosterResolution,
    RosterSelection as ClaudeRosterSelection, RunProfile as ClaudeRunProfile,
    CLAUDE_ROSTER_SCHEMA_VERSION,
};
use katachi_harness_codex::config_layers::discover_config_layers;
use katachi_harness_codex::discovery::resolve_project_roots as resolve_codex_project_roots;
use katachi_harness_codex::effective::{build_effective, BuildEffective};
use katachi_harness_codex::roster as codex_roster;
use katachi_harness_codex::roster_file::{
    load_rosters_dir as load_codex_rosters, CodexRosterFile, Resolution as CodexResolution,
    RunProfile as CodexRunProfile, Selection as CodexSelection,
};
use katachi_harness_codex::{
    agents as codex_agents, hooks as codex_hooks, legality as codex_legality, mcp as codex_mcp,
    rules as codex_rules, skills as codex_skills, CodexSettings,
};
use katachi_harness_gemini::policy::ResolvedPolicy as GeminiResolvedPolicy;
use katachi_harness_gemini::validate::gemini_validators;

use crate::cli::{GlobalArgs, KatachiAction, KatachiCmd};
use crate::commands::have::expand_roster_targets;
use crate::exit::ExitCode;
use crate::harness_registry::HarnessRegistry;

#[derive(Serialize)]
struct KatachiTargetEntry<'a> {
    harness: HarnessKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    roster_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backend: Option<BackendKind>,
    preference: i32,
    selector_count: usize,
    run_profile_overlay_present: bool,
}

pub fn dispatch(global: &GlobalArgs, cmd: KatachiCmd) -> Result<ExitCode> {
    match cmd.action {
        KatachiAction::List => run_list(global),
        KatachiAction::Show { id } => run_show(global, &id),
        KatachiAction::Validate { id } => run_validate(global, &id),
    }
}

fn run_list(global: &GlobalArgs) -> Result<ExitCode> {
    let overrides = PathOverrides {
        config_file: global.config.clone(),
        data_root: global.data_root.clone(),
        cache_root: global.cache_root.clone(),
    };
    let config_path = resolve_config_file(&overrides)?;
    let load = config::load(config_path)?;
    let storage = resolve_storage_paths(&overrides, &load.config.storage)?;

    let store = match KatachiStore::load_from_dir(&storage.katachis_dir()) {
        Ok(s) => s,
        Err(KatachiStoreError::Missing { .. }) => {
            emit_empty_list(global, "katachis directory does not exist; nothing to list");
            return Ok(ExitCode::Ok);
        }
        Err(err) => {
            emit_config_error(global, &format!("{err:#}"));
            return Ok(ExitCode::Config);
        }
    };

    let source_paths = katachi_source_paths(&storage.katachis_dir());

    #[derive(Serialize)]
    struct Entry<'a> {
        id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<&'a str>,
        targets: usize,
        target_harnesses: Vec<HarnessKind>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_path: Option<String>,
        target_details: Vec<KatachiTargetEntry<'a>>,
    }
    let entries: Vec<Entry<'_>> = store
        .all()
        .iter()
        .map(|d| Entry {
            id: &d.id,
            description: d.description.as_deref(),
            targets: d.targets.len(),
            target_harnesses: d.targets.iter().map(|t| t.harness).collect(),
            source_path: source_paths.get(&d.id).cloned(),
            target_details: d.targets.iter().map(target_entry).collect(),
        })
        .collect();
    if global.json {
        let payload = serde_json::json!({ "katachis": entries });
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        if entries.is_empty() {
            println!("(no katachis)");
            return Ok(ExitCode::Ok);
        }
        println!("{:<28} {:<8} {:<36} description", "id", "targets", "source");
        for e in &entries {
            println!(
                "{:<28} {:<8} {:<36} {}",
                e.id,
                e.targets,
                e.source_path.as_deref().unwrap_or("-"),
                e.description.unwrap_or("-")
            );
        }
    }
    Ok(ExitCode::Ok)
}

fn run_show(global: &GlobalArgs, id: &str) -> Result<ExitCode> {
    let overrides = PathOverrides {
        config_file: global.config.clone(),
        data_root: global.data_root.clone(),
        cache_root: global.cache_root.clone(),
    };
    let config_path = resolve_config_file(&overrides)?;
    let load = config::load(config_path)?;
    let storage = resolve_storage_paths(&overrides, &load.config.storage)?;

    let store = match KatachiStore::load_from_dir(&storage.katachis_dir()) {
        Ok(s) => s,
        Err(KatachiStoreError::Missing { .. }) => {
            emit_resolve_error(global, "katachis directory does not exist");
            return Ok(ExitCode::Resolve);
        }
        Err(err) => {
            emit_config_error(global, &format!("{err:#}"));
            return Ok(ExitCode::Config);
        }
    };
    let Some(def) = store.find(id) else {
        emit_resolve_error(global, &format!("no katachi with id `{id}`"));
        return Ok(ExitCode::Resolve);
    };

    if global.json {
        #[derive(Serialize)]
        struct ShowPayload<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            source_path: Option<String>,
            #[serde(flatten)]
            definition: &'a KatachiDefinition,
        }
        let source_paths = katachi_source_paths(&storage.katachis_dir());
        let payload = ShowPayload {
            source_path: source_paths.get(&def.id).cloned(),
            definition: def,
        };
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        println!("id          : {}", def.id);
        if let Some(path) = katachi_source_paths(&storage.katachis_dir()).get(&def.id) {
            println!("source      : {path}");
        }
        if let Some(d) = &def.description {
            println!("description : {d}");
        }
        println!("schema_v    : {}", def.schema_version);
        println!("targets ({}):", def.targets.len());
        for (i, t) in def.targets.iter().enumerate() {
            println!("  [{i}] harness={}", t.harness);
            if let Some(roster) = &t.roster_id {
                println!("       roster_id={roster}");
            }
            if let Some(b) = &t.backend {
                println!("       backend={}", b);
            }
            println!("       preference={}", t.preference);
            if !t.selectors.selectors.is_empty() {
                println!(
                    "       selectors={} entry/entries",
                    t.selectors.selectors.len()
                );
            }
            if !t.run_profile_overlay.is_null() {
                println!("       run_profile_overlay=present");
            }
        }
    }
    Ok(ExitCode::Ok)
}

fn run_validate(global: &GlobalArgs, id: &str) -> Result<ExitCode> {
    let overrides = PathOverrides {
        config_file: global.config.clone(),
        data_root: global.data_root.clone(),
        cache_root: global.cache_root.clone(),
    };
    let config_path = resolve_config_file(&overrides)?;
    let load = config::load(config_path)?;
    let storage = resolve_storage_paths(&overrides, &load.config.storage)?;

    let store = match KatachiStore::load_from_dir(&storage.katachis_dir()) {
        Ok(s) => s,
        Err(err) => {
            emit_config_error(global, &format!("{err:#}"));
            return Ok(ExitCode::Config);
        }
    };
    let Some(raw_def) = store.find(id) else {
        emit_resolve_error(global, &format!("no katachi with id `{id}`"));
        return Ok(ExitCode::Resolve);
    };
    let def = match expand_roster_targets(raw_def, &load.config, &storage) {
        Ok(def) => def,
        Err(err) => {
            emit_resolve_error(global, &err);
            return Ok(ExitCode::Resolve);
        }
    };
    let registry = HarnessRegistry::from_config(&load.config).with_fixtures_from_env();
    let modules = registry.as_refs();

    let cwd = resolve_cwd(global)?;
    let mut request = InvocationRequest::new(id, ActionRequest::Describe, cwd.clone());
    request.preferred_harnesses = global
        .prefer_harness
        .iter()
        .filter_map(|h| h.parse::<HarnessKind>().ok())
        .collect();
    request.preferred_backends = global
        .prefer_backend
        .iter()
        .filter_map(|b| b.parse::<BackendKind>().ok())
        .collect();
    if let Some(m) = global.materialization {
        request.materialization = match m {
            crate::cli::MaterializationArg::Ambient => {
                katachi_core::model::MaterializationMode::Ambient
            }
            crate::cli::MaterializationArg::TempOverlay => {
                katachi_core::model::MaterializationMode::TempOverlay
            }
        };
    }

    let inputs = ResolveInputs::new(&request, &def, &modules, &load.config, &storage, &cwd);
    let output = match resolve(inputs) {
        Ok(out) => out,
        Err(err) => {
            emit_resolve_error(global, &err.to_string());
            return Ok(ExitCode::Resolve);
        }
    };
    let mut validators = default_validators();
    if output.resolved.harness == HarnessKind::Gemini {
        let policy = GeminiResolvedPolicy::from_catalog(&output.catalog);
        validators.extend(gemini_validators(policy));
    }
    let mut validator_diags = run_validators(
        &ValidateContext {
            resolved: &output.resolved,
            catalog: &output.catalog,
            definition: &def,
        },
        &validators,
    );
    match harness_specific_diagnostics(&raw_def, &def, &output, &load.config, &storage, &cwd) {
        Ok(mut diags) => validator_diags.append(&mut diags),
        Err(err) => {
            emit_resolve_error(global, &err);
            return Ok(ExitCode::Resolve);
        }
    }
    let payload = serde_json::json!({
        "katachi_id": def.id,
        "harness": output.resolved.harness,
        "backend": output.resolved.backend,
        "resolved_diagnostics": &output.resolved.diagnostics,
        "validator_diagnostics": &validator_diags,
    });
    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        println!("katachi: {}", def.id);
        println!(
            "harness: {} (backend: {})",
            output.resolved.harness, output.resolved.backend
        );
        let total = output.resolved.diagnostics.len() + validator_diags.len();
        if total == 0 {
            println!("ok");
        } else {
            for d in output
                .resolved
                .diagnostics
                .iter()
                .chain(validator_diags.iter())
            {
                println!("  [{:?}] {}: {}", d.severity, d.code, d.message);
            }
        }
    }
    if any_error(&output.resolved.diagnostics) {
        return Ok(ExitCode::Resolve);
    }
    if any_error(&validator_diags) {
        return Ok(ExitCode::Validate);
    }
    Ok(ExitCode::Ok)
}

fn target_entry(target: &KatachiTarget) -> KatachiTargetEntry<'_> {
    KatachiTargetEntry {
        harness: target.harness,
        roster_id: target.roster_id.as_deref(),
        backend: target.backend,
        preference: target.preference,
        selector_count: target.selectors.selectors.len(),
        run_profile_overlay_present: !target.run_profile_overlay.is_null(),
    }
}

fn katachi_source_paths(dir: &Utf8Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(entries) = fs::read_dir(dir.as_std_path()) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        if path.extension() != Some("toml") || !path.is_file() {
            continue;
        }
        let id = KatachiDefinition::from_toml_file(&path)
            .map(|d| d.id)
            .ok()
            .or_else(|| path.file_stem().map(str::to_owned));
        if let Some(id) = id {
            out.entry(id).or_insert_with(|| path.to_string());
        }
    }
    out
}

fn harness_specific_diagnostics(
    raw_definition: &KatachiDefinition,
    definition: &KatachiDefinition,
    output: &ResolveOutput,
    config: &KatachiConfig,
    storage: &StoragePaths,
    cwd: &Utf8Path,
) -> std::result::Result<Vec<Diagnostic>, String> {
    match output.resolved.harness {
        HarnessKind::Claude => Ok(claude_validation_diagnostics(
            definition,
            &output.resolved,
            output,
        )),
        HarnessKind::Codex => {
            codex_validation_diagnostics(raw_definition, output, config, storage, cwd)
        }
        HarnessKind::Gemini => Ok(Vec::new()),
    }
}

fn claude_validation_diagnostics(
    definition: &KatachiDefinition,
    resolved: &ResolvedKatachi,
    output: &ResolveOutput,
) -> Vec<Diagnostic> {
    let resolved_roster = ResolvedClaudeRoster {
        roster: ClaudeRoster {
            version: CLAUDE_ROSTER_SCHEMA_VERSION,
            id: definition.id.clone(),
            description: definition.description.clone(),
            selection: ClaudeRosterSelection::default(),
            run_profile: ClaudeRunProfile::default(),
            resolution: ClaudeRosterResolution::default(),
        },
        resolved: resolved.clone(),
        catalog: output.catalog.clone(),
        projection_diagnostics: Vec::new(),
    };
    validate_claude_roster(&resolved_roster)
        .into_iter()
        .filter(|d| !d.code.starts_with("resolve.") && !d.code.starts_with("selector."))
        .collect()
}

fn codex_validation_diagnostics(
    raw_definition: &KatachiDefinition,
    output: &ResolveOutput,
    config: &KatachiConfig,
    storage: &StoragePaths,
    cwd: &Utf8Path,
) -> std::result::Result<Vec<Diagnostic>, String> {
    let raw_target = raw_definition
        .targets
        .get(output.chosen_target_index)
        .ok_or_else(|| "resolved target index is out of bounds".to_string())?;
    let settings = CodexSettings::load(config);
    let mut roster = if let Some(roster_id) = raw_target.roster_id.as_deref() {
        let dir = storage.rosters_dir().join("codex");
        load_codex_rosters(&dir)
            .map_err(|err| format!("loading codex rosters: {err}"))?
            .into_iter()
            .find(|r| r.id == roster_id)
            .ok_or_else(|| format!("codex roster `{roster_id}` not found"))?
    } else {
        synthetic_codex_roster(raw_definition, raw_target, &output.resolved)
    };
    apply_codex_overlay(&mut roster.run_profile, &raw_target.run_profile_overlay);
    roster.run_profile.backend = Some(output.resolved.backend.to_string());

    let roots = resolve_codex_project_roots(&settings.project_roots, cwd);
    let mut diagnostics = Vec::new();
    let layers = discover_config_layers(&settings, &roots, cwd, &mut diagnostics);
    let instructions =
        codex_roster::discover_instruction_chain(&settings, &roots, cwd, &mut diagnostics);
    let hooks = codex_hooks::discover_hooks(&layers, &mut diagnostics);
    let rules = codex_rules::discover_rules(&layers, &mut diagnostics);
    let mcps = codex_mcp::discover_mcp_servers(&layers, &mut diagnostics);
    let skills = codex_skills::discover_skills(&settings, &roots, &mut diagnostics);
    let agents = codex_agents::discover_agents(&settings, &roots, &mut diagnostics);

    let active_profile = roster
        .selection
        .profiles
        .first()
        .cloned()
        .or_else(|| roster.run_profile.profile.clone());
    let effective = build_effective(BuildEffective {
        layers: &layers,
        instructions: &instructions,
        hooks: &hooks,
        rules: &rules,
        mcps: &mcps,
        skills: &skills,
        agents: &agents,
        run_profile: roster.run_profile.clone(),
        active_profile,
        only_active: roster.resolution.respect_project_trust,
    });

    let mut out = diagnostics;
    out.extend(codex_legality::validate(&settings, &roster, &effective));
    Ok(out)
}

fn synthetic_codex_roster(
    definition: &KatachiDefinition,
    target: &KatachiTarget,
    resolved: &ResolvedKatachi,
) -> CodexRosterFile {
    let mut selection = CodexSelection::default();
    for item in &resolved.selected_items {
        match item.item.kind.as_str() {
            "config_layer" => selection.config_layers.push(item.item.id.clone()),
            "profile" => selection.profiles.push(item.item.id.clone()),
            "instruction_doc" => selection.instructions.push(item.item.id.clone()),
            "skill" => selection.skills.push(item.item.id.clone()),
            "custom_agent" => selection.agents.push(item.item.id.clone()),
            "hook_set" => selection.hooks.push(item.item.id.clone()),
            "mcp_server" => selection.mcp_servers.push(item.item.id.clone()),
            "rule_set" => selection.rules.push(item.item.id.clone()),
            "plugin" => selection.plugins.push(item.item.id.clone()),
            _ => {}
        }
    }
    CodexRosterFile {
        version: katachi_harness_codex::roster_file::ROSTER_SCHEMA_VERSION,
        id: definition.id.clone(),
        description: definition.description.clone(),
        selection,
        run_profile: codex_profile_from_overlay(&target.run_profile_overlay),
        resolution: CodexResolution::default(),
    }
}

fn codex_profile_from_overlay(overlay: &serde_json::Value) -> CodexRunProfile {
    let mut profile = CodexRunProfile::default();
    apply_codex_overlay(&mut profile, overlay);
    profile
}

fn apply_codex_overlay(profile: &mut CodexRunProfile, overlay: &serde_json::Value) {
    let Some(obj) = overlay.as_object() else {
        return;
    };
    if let Some(v) = obj.get("approval_policy").and_then(|v| v.as_str()) {
        profile.approval_policy = Some(v.to_string());
    }
    if let Some(v) = obj.get("sandbox_mode").and_then(|v| v.as_str()) {
        profile.sandbox_mode = Some(v.to_string());
    }
    if let Some(v) = obj.get("model").and_then(|v| v.as_str()) {
        profile.model = Some(v.to_string());
    }
    if let Some(v) = obj.get("profile").and_then(|v| v.as_str()) {
        profile.profile = Some(v.to_string());
    }
    if let Some(v) = obj.get("output_mode").and_then(|v| v.as_str()) {
        profile.output_mode = Some(v.to_string());
    }
    if let Some(v) = obj.get("timeout_secs").and_then(|v| v.as_u64()) {
        profile.timeout_secs = Some(v);
    }
    if let Some(v) = obj.get("backend").and_then(|v| v.as_str()) {
        profile.backend = Some(v.to_string());
    }
}

fn resolve_cwd(global: &GlobalArgs) -> Result<Utf8PathBuf> {
    if let Some(cwd) = &global.cwd {
        return Ok(cwd.clone());
    }
    let std_cwd = std::env::current_dir()?;
    Utf8PathBuf::from_path_buf(std_cwd)
        .map_err(|p| anyhow::anyhow!("cwd `{}` is not valid UTF-8", p.display()))
}

fn emit_empty_list(global: &GlobalArgs, msg: &str) {
    if global.json {
        let payload = serde_json::json!({ "katachis": [], "diagnostics": [msg] });
        let _ = serde_json::to_writer_pretty(std::io::stdout(), &payload);
        println!();
    } else {
        println!("(no katachis: {msg})");
    }
}

fn emit_config_error(global: &GlobalArgs, msg: &str) {
    if global.json {
        let payload = serde_json::json!({
            "error": { "kind": "config", "message": msg }
        });
        let _ = serde_json::to_writer_pretty(std::io::stdout(), &payload);
        println!();
    } else {
        eprintln!("katachi katachi: {msg}");
    }
}

fn emit_resolve_error(global: &GlobalArgs, msg: &str) {
    if global.json {
        let payload = serde_json::json!({
            "error": { "kind": "resolve", "message": msg }
        });
        let _ = serde_json::to_writer_pretty(std::io::stdout(), &payload);
        println!();
    } else {
        eprintln!("katachi katachi: {msg}");
    }
}
