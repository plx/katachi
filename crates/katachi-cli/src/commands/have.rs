//! `katachi have <id> describe` and `... graph` — resolve + validate +
//! optionally render the dependency subgraph for a named katachi.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Result};
use camino::Utf8PathBuf;
use serde::Serialize;
use time::OffsetDateTime;

use katachi_core::config;
use katachi_core::diagnostic::{any_error, Diagnostic, Severity};
use katachi_core::error::ResolveError;
use katachi_core::harness::{ExecuteContext, RosterCatalog};
use katachi_core::katachi::{KatachiDefinition, KatachiStore, KatachiStoreError, KatachiTarget};
use katachi_core::materialize::KeepPolicy;
use katachi_core::model::{BackendKind, HarnessKind, ItemRef, MaterializationMode};
use katachi_core::paths::{
    resolve_config_file, resolve_storage_paths, PathOverrides, StoragePaths,
};
use katachi_core::persist::{
    finalize_run_directory, CommitPolicy, FinalizedRunState, RunDirectory,
};
use katachi_core::plan::{
    ActionRequest, ExecutionPlan, InvocationRequest, ResolvedItemRef, ResolvedKatachi,
    SelectionReason,
};
use katachi_core::resolve::{resolve, ResolveInputs, ResolveOutput};
use katachi_core::roster::EdgeKind;
use katachi_core::validate::{default_validators, run_validators, ValidateContext};

use katachi_harness_claude::config::ClaudeConfig;
use katachi_harness_claude::plan::{
    build_claude_plan, materialize_overlay as materialize_claude_overlay, ClaudePlanInputs,
    MaterializedOverlay,
};
use katachi_harness_claude::resolve::ResolvedClaudeRoster;
use katachi_harness_claude::roster::{
    ClaudeRoster, ClaudeRosterStore, RosterResolution as ClaudeRosterResolution,
    RosterSelection as ClaudeRosterSelection, RunProfile as ClaudeRunProfile,
};
use katachi_harness_codex::config_layers::discover_config_layers;
use katachi_harness_codex::discovery::resolve_project_roots as resolve_codex_project_roots;
use katachi_harness_codex::effective::{build_effective, BuildEffective};
use katachi_harness_codex::planner::PlannerBundle;
use katachi_harness_codex::roster as codex_roster;
use katachi_harness_codex::roster_file::{
    load_rosters_dir as load_codex_rosters, CodexRosterFile, Resolution as CodexResolution,
    RunProfile as CodexRunProfile, Selection as CodexSelection,
};
use katachi_harness_codex::{
    agents as codex_agents, hooks as codex_hooks, mcp as codex_mcp, rules as codex_rules,
    skills as codex_skills, CodexSettings,
};
use katachi_harness_gemini::roster::GeminiRosterStore;

use crate::cli::{GlobalArgs, GraphFormat, HaveCmd, MaterializationArg};
use crate::exit::ExitCode;
use crate::harness_registry::HarnessRegistry;

pub fn run_describe(global: &GlobalArgs, have: &HaveCmd) -> Result<ExitCode> {
    let prepared = match prepare(global, have, ActionRequest::Describe)? {
        Prepared::Ready(p) => p,
        Prepared::Failed(code) => return Ok(code),
    };

    let report = DescribeReport {
        resolved: &prepared.output.resolved,
        diagnostics: &prepared.validator_diagnostics,
        description: prepared.definition.description.as_deref(),
    };

    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        render_human(&report);
    }

    Ok(prepared.exit_code())
}

pub fn run_plan_execute(global: &GlobalArgs, have: &HaveCmd, prompt: &str) -> Result<ExitCode> {
    let prepared = match prepare(
        global,
        have,
        ActionRequest::Plan {
            prompt: prompt.to_string(),
        },
    )? {
        Prepared::Ready(p) => p,
        Prepared::Failed(code) => return Ok(code),
    };
    if let Some(code) = early_exit(&prepared) {
        emit_prepared_diagnostics(global, &prepared);
        return Ok(code);
    }
    let planned = match build_plan_for_prepared(global, &prepared) {
        Ok(plan) => plan,
        Err(err) => {
            emit_plan_error(global, &prepared, &err.to_string())?;
            return Ok(ExitCode::Plan);
        }
    };
    render_have_plan(global, &prepared, &planned)?;
    Ok(ExitCode::Ok)
}

pub fn run_execute(global: &GlobalArgs, have: &HaveCmd, prompt: &str) -> Result<ExitCode> {
    let prepared = match prepare(
        global,
        have,
        ActionRequest::Execute {
            prompt: prompt.to_string(),
        },
    )? {
        Prepared::Ready(p) => p,
        Prepared::Failed(code) => return Ok(code),
    };
    if let Some(code) = early_exit(&prepared) {
        emit_prepared_diagnostics(global, &prepared);
        return Ok(code);
    }
    let planned = match build_plan_for_prepared(global, &prepared) {
        Ok(plan) => plan,
        Err(err) => {
            emit_plan_error(global, &prepared, &err.to_string())?;
            return Ok(ExitCode::Plan);
        }
    };

    if global.dry_run {
        render_have_plan(global, &prepared, &planned)?;
        return Ok(ExitCode::Ok);
    }

    execute_prepared_plan(global, &prepared, planned)
}

fn early_exit(prepared: &PreparedHave) -> Option<ExitCode> {
    if prepared.has_resolver_errors {
        Some(ExitCode::Resolve)
    } else if prepared.has_validation_errors {
        Some(ExitCode::Validate)
    } else {
        None
    }
}

fn emit_prepared_diagnostics(global: &GlobalArgs, prepared: &PreparedHave) {
    if global.json {
        let payload = serde_json::json!({
            "katachi_id": prepared.definition.id,
            "diagnostics": prepared
                .output
                .resolved
                .diagnostics
                .iter()
                .chain(prepared.validator_diagnostics.iter())
                .collect::<Vec<_>>(),
        });
        let _ = serde_json::to_writer_pretty(std::io::stdout(), &payload);
        println!();
    } else {
        for d in prepared
            .output
            .resolved
            .diagnostics
            .iter()
            .chain(prepared.validator_diagnostics.iter())
        {
            eprintln!("[{}] {}: {}", severity_tag(d.severity), d.code, d.message);
        }
    }
}

fn emit_plan_error(global: &GlobalArgs, prepared: &PreparedHave, message: &str) -> Result<()> {
    if global.json {
        let payload = serde_json::json!({
            "katachi_id": prepared.definition.id,
            "harness": prepared.output.resolved.harness,
            "backend": prepared.output.resolved.backend,
            "error": {
                "kind": "plan",
                "message": message,
            },
            "diagnostics": all_diagnostics(prepared),
        });
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        eprintln!("katachi have plan: {message}");
    }
    Ok(())
}

fn emit_execute_error(
    global: &GlobalArgs,
    prepared: &PreparedHave,
    message: &str,
    partial_path: Option<&camino::Utf8Path>,
) -> Result<()> {
    if global.json {
        let payload = serde_json::json!({
            "katachi_id": prepared.definition.id,
            "harness": prepared.output.resolved.harness,
            "backend": prepared.output.resolved.backend,
            "error": {
                "kind": "execute",
                "message": message,
            },
            "state": "partial",
            "path": partial_path.map(|p| p.to_string()),
            "diagnostics": all_diagnostics(prepared),
        });
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        eprintln!("katachi have execute: {message}");
        if let Some(path) = partial_path {
            eprintln!("katachi have execute: partial preserved at `{path}`");
        }
    }
    Ok(())
}

fn render_have_plan(
    global: &GlobalArgs,
    prepared: &PreparedHave,
    plan: &ExecutionPlan,
) -> Result<()> {
    let report = HavePlanReport {
        katachi_id: &prepared.definition.id,
        harness: plan.harness,
        backend: plan.backend,
        plan,
        selected_items: &prepared.output.resolved.selected_items,
        diagnostics: all_diagnostics(prepared),
    };
    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        println!("katachi: {}", report.katachi_id);
        println!("harness: {} (backend: {})", report.harness, report.backend);
        println!("summary: {}", report.plan.summary);
        println!("selected items: {}", report.selected_items.len());
        println!();
        println!("argv:");
        for arg in &report.plan.execution.argv {
            println!("  {arg}");
        }
        if let Some(cwd) = &report.plan.execution.cwd {
            println!("cwd: {cwd}");
        }
        if !report.diagnostics.is_empty() {
            println!();
            println!("diagnostics:");
            for d in &report.diagnostics {
                println!("  [{}] {}: {}", severity_tag(d.severity), d.code, d.message);
            }
        }
    }
    Ok(())
}

fn render_have_execute(
    global: &GlobalArgs,
    prepared: &PreparedHave,
    record: &katachi_core::record::ExecutionRecord,
    finalized: &katachi_core::persist::FinalizedRun,
) -> Result<()> {
    let state = match finalized.state {
        FinalizedRunState::Committed => "committed",
        FinalizedRunState::Partial => "partial",
    };
    if global.json {
        let payload = serde_json::json!({
            "katachi_id": prepared.definition.id,
            "harness": record.plan.harness,
            "backend": record.plan.backend,
            "record": record,
            "state": state,
            "path": finalized.path,
            "diagnostics": all_diagnostics(prepared),
        });
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        println!(
            "run {} finished with outcome={:?}",
            record.run_id, record.result.outcome
        );
        println!("recorded at {}", finalized.path);
        println!("state: {state}");
    }
    Ok(())
}

fn all_diagnostics(prepared: &PreparedHave) -> Vec<&Diagnostic> {
    prepared
        .output
        .resolved
        .diagnostics
        .iter()
        .chain(prepared.validator_diagnostics.iter())
        .collect()
}

#[derive(Serialize)]
struct HavePlanReport<'a> {
    katachi_id: &'a str,
    harness: HarnessKind,
    backend: BackendKind,
    plan: &'a ExecutionPlan,
    selected_items: &'a [ResolvedItemRef],
    diagnostics: Vec<&'a Diagnostic>,
}

pub fn run_graph(global: &GlobalArgs, have: &HaveCmd, format: GraphFormat) -> Result<ExitCode> {
    let prepared = match prepare(global, have, ActionRequest::Graph)? {
        Prepared::Ready(p) => p,
        Prepared::Failed(code) => return Ok(code),
    };

    let selected: BTreeSet<ItemRef> = prepared
        .output
        .resolved
        .selected_items
        .iter()
        .map(|i| i.item.clone())
        .collect();
    let subgraph = filter_catalog(&prepared.output.catalog, &selected);
    let graph_view = GraphView {
        katachi_id: &prepared.definition.id,
        harness: prepared.output.resolved.harness,
        backend: prepared.output.resolved.backend,
        items: subgraph.items,
        edges: subgraph.edges,
        diagnostics: prepared
            .output
            .resolved
            .diagnostics
            .iter()
            .chain(prepared.validator_diagnostics.iter())
            .collect(),
    };

    let render_json = global.json || matches!(format, GraphFormat::Json);
    if render_json {
        serde_json::to_writer_pretty(std::io::stdout(), &graph_view)?;
        println!();
    } else {
        match format {
            GraphFormat::Json => unreachable!(),
            GraphFormat::Text => render_graph_text(&graph_view),
            GraphFormat::Dot => render_graph_dot(&graph_view),
        }
    }
    Ok(prepared.exit_code())
}

struct PreparedHave {
    raw_definition: KatachiDefinition,
    definition: KatachiDefinition,
    output: ResolveOutput,
    validator_diagnostics: Vec<Diagnostic>,
    has_resolver_errors: bool,
    has_validation_errors: bool,
    request: InvocationRequest,
    config: katachi_core::config::KatachiConfig,
    storage: StoragePaths,
    cwd: Utf8PathBuf,
    registry: HarnessRegistry,
}

impl PreparedHave {
    fn exit_code(&self) -> ExitCode {
        if self.has_resolver_errors {
            ExitCode::Resolve
        } else if self.has_validation_errors {
            ExitCode::Validate
        } else {
            ExitCode::Ok
        }
    }
}

enum Prepared {
    Ready(PreparedHave),
    Failed(ExitCode),
}

fn prepare(global: &GlobalArgs, have: &HaveCmd, action: ActionRequest) -> Result<Prepared> {
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
        Err(KatachiStoreError::Missing { path }) => {
            emit_resolve_error_message(
                global,
                &format!("katachis directory `{path}` does not exist"),
            )?;
            return Ok(Prepared::Failed(ExitCode::Resolve));
        }
        Err(err @ KatachiStoreError::DuplicateId { .. }) => {
            emit_config_error_message(global, &format!("{err:#}"))?;
            return Ok(Prepared::Failed(ExitCode::Config));
        }
        Err(err) => {
            emit_resolve_error_message(global, &format!("{err:#}"))?;
            return Ok(Prepared::Failed(ExitCode::Resolve));
        }
    };

    let definition_raw = match store.find(&have.id) {
        Some(d) => d.clone(),
        None => {
            emit_resolve_error(
                global,
                &ResolveError::UnknownKatachi {
                    id: have.id.clone(),
                },
            )?;
            return Ok(Prepared::Failed(ExitCode::Resolve));
        }
    };

    let definition = match expand_roster_targets(&definition_raw, &load.config, &storage) {
        Ok(d) => d,
        Err(msg) => {
            emit_resolve_error_message(global, &msg)?;
            return Ok(Prepared::Failed(ExitCode::Resolve));
        }
    };

    let registry = HarnessRegistry::from_config(&load.config).with_fixtures_from_env();
    let modules = registry.as_refs();

    let cwd = resolve_cwd(global)?;
    let request = build_request(global, &have.id, cwd.clone(), action);

    let inputs = ResolveInputs::new(
        &request,
        &definition,
        &modules,
        &load.config,
        &storage,
        &cwd,
    );
    let output = match resolve(inputs) {
        Ok(out) => out,
        Err(err) => {
            emit_resolve_error(global, &err)?;
            return Ok(Prepared::Failed(ExitCode::Resolve));
        }
    };

    let validator_diagnostics = run_validators(
        &ValidateContext {
            resolved: &output.resolved,
            catalog: &output.catalog,
            definition: &definition,
        },
        &default_validators(),
    );

    let has_resolver_errors = any_error(&output.resolved.diagnostics);
    let has_validation_errors = any_error(&validator_diagnostics);
    Ok(Prepared::Ready(PreparedHave {
        raw_definition: definition_raw,
        definition,
        output,
        validator_diagnostics,
        has_resolver_errors,
        has_validation_errors,
        request,
        config: load.config,
        storage,
        cwd,
        registry,
    }))
}

fn build_plan_for_prepared(global: &GlobalArgs, prepared: &PreparedHave) -> Result<ExecutionPlan> {
    let run_id = katachi_core::record::RunId::new();
    let request = effective_request(global, prepared);
    let resolved = resolved_for_plan(prepared);

    match resolved.harness {
        HarnessKind::Claude => {
            let resolved_roster = build_claude_resolved(prepared, &resolved)?;
            let claude_config = ClaudeConfig::from_shared(&prepared.config);
            let plan = build_claude_plan(ClaudePlanInputs {
                resolved_roster: &resolved_roster,
                config: &claude_config,
                cwd: &prepared.cwd,
                run_id,
                materialization: request.materialization,
                prompt: action_prompt(&request.action).map(str::to_owned),
            })
            .map_err(|err| anyhow!("{err}"))?;
            Ok(plan)
        }
        HarnessKind::Codex => {
            let mut resolved = resolved;
            let bundle = build_codex_planner_bundle(prepared, &resolved)?;
            resolved.run_profile.extras = serde_json::to_value(&bundle)?;
            let ctx = katachi_core::harness::PlanContext {
                request: &request,
                resolved: &resolved,
                run_id,
            };
            prepared
                .registry
                .find(HarnessKind::Codex)
                .ok_or_else(|| anyhow!("codex harness is disabled"))?
                .plan(&ctx)
                .map_err(|err| anyhow!("{err}"))
        }
        HarnessKind::Gemini => {
            let ctx = katachi_core::harness::PlanContext {
                request: &request,
                resolved: &resolved,
                run_id,
            };
            prepared
                .registry
                .find(HarnessKind::Gemini)
                .ok_or_else(|| anyhow!("gemini harness is disabled"))?
                .plan(&ctx)
                .map_err(|err| anyhow!("{err}"))
        }
    }
}

fn execute_prepared_plan(
    global: &GlobalArgs,
    prepared: &PreparedHave,
    plan: ExecutionPlan,
) -> Result<ExitCode> {
    let request = effective_request(global, prepared);
    let runs_dir = prepared.storage.runs_dir();
    let run_dir = match RunDirectory::create(&runs_dir, plan.run_id) {
        Ok(dir) => dir,
        Err(err) => {
            emit_execute_error(global, prepared, &format!("{err:#}"), None)?;
            return Ok(ExitCode::Execute);
        }
    };

    if let Err(err) = run_dir.write_request(&request) {
        let partial = run_dir.partial_path().to_owned();
        emit_execute_error(global, prepared, &format!("{err:#}"), Some(&partial))?;
        return Ok(ExitCode::Execute);
    }
    if let Err(err) = run_dir.write_plan(&plan) {
        let partial = run_dir.partial_path().to_owned();
        emit_execute_error(global, prepared, &format!("{err:#}"), Some(&partial))?;
        return Ok(ExitCode::Execute);
    }

    let mut claude_overlay = None;
    if plan.harness == HarnessKind::Claude
        && matches!(plan.materialization.mode, MaterializationMode::TempOverlay)
        && !plan.materialization.files.is_empty()
    {
        match materialize_claude_overlay(&plan.materialization) {
            Ok(overlay) => claude_overlay = Some(overlay),
            Err(err) => {
                let partial = run_dir.partial_path().to_owned();
                emit_execute_error(
                    global,
                    prepared,
                    &format!("failed to materialize claude overlay: {err}"),
                    Some(&partial),
                )?;
                return Ok(ExitCode::Execute);
            }
        }
    }

    let module = prepared
        .registry
        .find(plan.harness)
        .ok_or_else(|| anyhow!("{} harness is disabled", plan.harness))?;
    let exec_ctx = ExecuteContext {
        request: &request,
        plan: &plan,
        run_dir: &run_dir,
        started_at: OffsetDateTime::now_utc(),
    };
    let record_result = module.execute(&exec_ctx);
    let outcome = record_result
        .as_ref()
        .ok()
        .map(|record| record.result.outcome);
    let finalized = finalize_run_directory(run_dir, outcome, CommitPolicy::SuccessOnly);

    cleanup_claude_overlay(
        claude_overlay,
        ClaudeConfig::from_shared(&prepared.config).preserve_failed_overlays,
        finalized.is_committed(),
    );

    match record_result {
        Ok(record) => {
            render_have_execute(global, prepared, &record, &finalized)?;
            if record.result.outcome == katachi_core::record::Outcome::Success
                && finalized.is_committed()
            {
                Ok(ExitCode::Ok)
            } else {
                Ok(ExitCode::Execute)
            }
        }
        Err(err) => {
            emit_execute_error(
                global,
                prepared,
                &format!("{err:#}"),
                Some(&finalized.partial_path),
            )?;
            Ok(ExitCode::Execute)
        }
    }
}

fn cleanup_claude_overlay(
    overlay: Option<MaterializedOverlay>,
    preserve_on_failure: bool,
    success: bool,
) {
    let Some(overlay) = overlay else { return };
    if !success && preserve_on_failure {
        if let MaterializedOverlay::Temp(mut temp) = overlay {
            temp.set_keep(KeepPolicy::Keep);
        }
        return;
    }
    let _ = overlay.cleanup();
}

fn effective_request(global: &GlobalArgs, prepared: &PreparedHave) -> InvocationRequest {
    let mut request = prepared.request.clone();
    if global.materialization.is_none() {
        request.materialization = selected_target_materialization(prepared);
    }
    request
}

fn selected_target_materialization(prepared: &PreparedHave) -> MaterializationMode {
    let raw = selected_raw_target(prepared);
    let expanded = selected_expanded_target(prepared);
    if raw.roster_id.is_none() {
        return MaterializationMode::TempOverlay;
    }
    match expanded.harness {
        HarnessKind::Claude => {
            let claude_config = ClaudeConfig::from_shared(&prepared.config);
            let Ok(store) = ClaudeRosterStore::load_default(&prepared.storage, &claude_config)
            else {
                return MaterializationMode::TempOverlay;
            };
            store
                .find(raw.roster_id.as_deref().unwrap_or_default())
                .map(|r| r.materialization_mode())
                .unwrap_or(MaterializationMode::TempOverlay)
        }
        HarnessKind::Codex => {
            let dir = prepared.storage.rosters_dir().join("codex");
            let Ok(rosters) = load_codex_rosters(&dir) else {
                return MaterializationMode::TempOverlay;
            };
            rosters
                .into_iter()
                .find(|r| raw.roster_id.as_deref() == Some(r.id.as_str()))
                .map(|r| codex_materialization(&r.resolution.materialization))
                .unwrap_or(MaterializationMode::TempOverlay)
        }
        HarnessKind::Gemini => {
            let dir = prepared.storage.rosters_dir().join("gemini");
            let Ok(store) = GeminiRosterStore::load_dir(&dir) else {
                return MaterializationMode::TempOverlay;
            };
            store
                .find(raw.roster_id.as_deref().unwrap_or_default())
                .map(|r| codex_materialization(&r.resolution.materialization))
                .unwrap_or(MaterializationMode::TempOverlay)
        }
    }
}

fn codex_materialization(raw: &str) -> MaterializationMode {
    if raw == "ambient" {
        MaterializationMode::Ambient
    } else {
        MaterializationMode::TempOverlay
    }
}

fn resolved_for_plan(prepared: &PreparedHave) -> ResolvedKatachi {
    prepared.output.resolved.clone()
}

fn selected_raw_target(prepared: &PreparedHave) -> &KatachiTarget {
    &prepared.raw_definition.targets[prepared.output.chosen_target_index]
}

fn selected_expanded_target(prepared: &PreparedHave) -> &KatachiTarget {
    &prepared.definition.targets[prepared.output.chosen_target_index]
}

fn action_prompt(action: &ActionRequest) -> Option<&str> {
    match action {
        ActionRequest::Plan { prompt } | ActionRequest::Execute { prompt } => Some(prompt),
        _ => None,
    }
}

fn build_claude_resolved(
    prepared: &PreparedHave,
    resolved: &ResolvedKatachi,
) -> Result<ResolvedClaudeRoster> {
    let raw_target = selected_raw_target(prepared);
    let mut roster = if let Some(roster_id) = raw_target.roster_id.as_deref() {
        let claude_config = ClaudeConfig::from_shared(&prepared.config);
        let store = ClaudeRosterStore::load_default(&prepared.storage, &claude_config)
            .map_err(|err| anyhow!("loading claude rosters: {err}"))?;
        store
            .find(roster_id)
            .ok_or_else(|| anyhow!("claude roster `{roster_id}` not found"))?
            .clone()
    } else {
        synthetic_claude_roster(prepared)
    };
    apply_claude_overlay(&mut roster.run_profile, &raw_target.run_profile_overlay);
    if raw_target.roster_id.is_none() {
        roster.run_profile = claude_profile_from_overlay(&raw_target.run_profile_overlay);
    }
    Ok(ResolvedClaudeRoster {
        roster,
        resolved: resolved.clone(),
        catalog: prepared.output.catalog.clone(),
        projection_diagnostics: Vec::new(),
    })
}

fn synthetic_claude_roster(prepared: &PreparedHave) -> ClaudeRoster {
    let mut selection = ClaudeRosterSelection::default();
    for item in &prepared.output.resolved.selected_items {
        match item.item.kind.as_str() {
            "plugin" => selection.plugins.push(item.item.id.clone()),
            "skill" => selection.skills.push(item.item.id.clone()),
            "agent" => selection.agents.push(item.item.id.clone()),
            "hook_set" => selection.hooks.push(item.item.id.clone()),
            "mcp_server" => selection.mcp_servers.push(item.item.id.clone()),
            "instruction_source" => selection.instructions.push(item.item.id.clone()),
            "output_style" => selection.output_styles.push(item.item.id.clone()),
            _ => {}
        }
    }
    ClaudeRoster {
        version: katachi_harness_claude::roster::CLAUDE_ROSTER_SCHEMA_VERSION,
        id: prepared.definition.id.clone(),
        description: prepared.definition.description.clone(),
        selection,
        run_profile: claude_profile_from_overlay(
            &selected_raw_target(prepared).run_profile_overlay,
        ),
        resolution: ClaudeRosterResolution::default(),
    }
}

fn claude_profile_from_overlay(overlay: &serde_json::Value) -> ClaudeRunProfile {
    let mut profile = ClaudeRunProfile::default();
    apply_claude_overlay(&mut profile, overlay);
    profile
}

fn apply_claude_overlay(profile: &mut ClaudeRunProfile, overlay: &serde_json::Value) {
    let Some(obj) = overlay.as_object() else {
        return;
    };
    if let Some(v) = obj.get("model").and_then(|v| v.as_str()) {
        profile.model = Some(v.to_string());
    }
    if let Some(v) = obj.get("permission_mode").and_then(|v| v.as_str()) {
        profile.permission_mode = Some(v.to_string());
    }
    if let Some(v) = obj.get("output_format").and_then(|v| v.as_str()) {
        profile.output_format = Some(v.to_string());
    }
    if let Some(v) = obj.get("system_prompt").and_then(|v| v.as_str()) {
        profile.system_prompt = Some(v.to_string());
    }
    if let Some(v) = obj.get("append_system_prompt").and_then(|v| v.as_str()) {
        profile.append_system_prompt = Some(v.to_string());
    }
    if let Some(v) = obj.get("timeout_secs").and_then(|v| v.as_u64()) {
        profile.timeout_secs = Some(v);
    }
    if let Some(v) = obj
        .get("include_partial_messages")
        .and_then(|v| v.as_bool())
    {
        profile.include_partial_messages = Some(v);
    }
    apply_string_array(&mut profile.setting_sources, obj.get("setting_sources"));
    apply_string_array(&mut profile.allowed_tools, obj.get("allowed_tools"));
    apply_string_array(&mut profile.disallowed_tools, obj.get("disallowed_tools"));
}

fn build_codex_planner_bundle(
    prepared: &PreparedHave,
    resolved: &ResolvedKatachi,
) -> Result<PlannerBundle> {
    let raw_target = selected_raw_target(prepared);
    let settings = CodexSettings::load(&prepared.config);
    let mut roster = if let Some(roster_id) = raw_target.roster_id.as_deref() {
        let dir = prepared.storage.rosters_dir().join("codex");
        load_codex_rosters(&dir)
            .map_err(|err| anyhow!("loading codex rosters: {err}"))?
            .into_iter()
            .find(|r| r.id == roster_id)
            .ok_or_else(|| anyhow!("codex roster `{roster_id}` not found"))?
    } else {
        synthetic_codex_roster(prepared)
    };
    apply_codex_overlay(&mut roster.run_profile, &raw_target.run_profile_overlay);
    roster.run_profile.backend = Some(resolved.backend.to_string());

    let cwd = &prepared.cwd;
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

    Ok(PlannerBundle {
        roster,
        effective,
        settings,
    })
}

fn synthetic_codex_roster(prepared: &PreparedHave) -> CodexRosterFile {
    let mut selection = CodexSelection::default();
    for item in &prepared.output.resolved.selected_items {
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
        id: prepared.definition.id.clone(),
        description: prepared.definition.description.clone(),
        selection,
        run_profile: codex_profile_from_overlay(&selected_raw_target(prepared).run_profile_overlay),
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

fn apply_string_array(out: &mut Vec<String>, value: Option<&serde_json::Value>) {
    let Some(values) = value.and_then(|v| v.as_array()) else {
        return;
    };
    *out = values
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
}

/// Expand any target with a `roster_id` into projected selectors and a
/// run-profile overlay. Mixing `roster_id` with explicit selectors is a
/// deliberate resolve-time error because roster expansion already supplies
/// the target selector set.
pub(crate) fn expand_roster_targets(
    raw: &KatachiDefinition,
    config: &katachi_core::config::KatachiConfig,
    storage: &StoragePaths,
) -> std::result::Result<KatachiDefinition, String> {
    let mut targets: Vec<KatachiTarget> = Vec::with_capacity(raw.targets.len());
    for target in &raw.targets {
        let Some(roster_id) = target.roster_id.as_ref() else {
            targets.push(target.clone());
            continue;
        };
        if roster_id.trim().is_empty() {
            return Err(format!(
                "katachi `{}` target {} has empty `roster_id`",
                raw.id, target.harness
            ));
        }
        if !target.selectors.selectors.is_empty() {
            return Err(format!(
                "katachi `{}` target {} mixes `roster_id` with explicit selectors; \
                 roster_id targets must not also declare explicit selectors",
                raw.id, target.harness
            ));
        }
        let projected = match target.harness {
            HarnessKind::Claude => project_claude_target(target, roster_id, config, storage)?,
            HarnessKind::Codex => project_codex_target(target, roster_id, storage)?,
            HarnessKind::Gemini => project_gemini_target(target, roster_id, storage)?,
        };
        targets.push(projected);
    }
    Ok(KatachiDefinition {
        schema_version: raw.schema_version,
        id: raw.id.clone(),
        description: raw.description.clone(),
        targets,
    })
}

fn project_claude_target(
    base: &KatachiTarget,
    roster_id: &str,
    config: &katachi_core::config::KatachiConfig,
    storage: &StoragePaths,
) -> std::result::Result<KatachiTarget, String> {
    let claude_config = ClaudeConfig::from_shared(config);
    let store = ClaudeRosterStore::load_default(storage, &claude_config)
        .map_err(|err| format!("loading claude rosters: {err}"))?;
    let roster = store
        .find(roster_id)
        .ok_or_else(|| format!("claude roster `{roster_id}` not found"))?;
    let roster_backend = match roster.run_profile.backend.as_deref() {
        Some(raw) => Some(
            raw.parse::<BackendKind>()
                .map_err(|_| format!("unknown claude roster backend `{raw}`"))?,
        ),
        None => None,
    };
    let backend = base.backend.or(roster_backend);
    Ok(KatachiTarget {
        harness: HarnessKind::Claude,
        roster_id: Some(roster_id.to_string()),
        backend,
        preference: base.preference,
        selectors: roster.to_selector_set(roster.resolution.include_transitive),
        run_profile_overlay: merge_overlay(&base.run_profile_overlay, roster.run_profile_overlay()),
    })
}

fn project_codex_target(
    base: &KatachiTarget,
    roster_id: &str,
    storage: &StoragePaths,
) -> std::result::Result<KatachiTarget, String> {
    let dir = storage.rosters_dir().join("codex");
    let rosters =
        load_codex_rosters(&dir).map_err(|err| format!("loading codex rosters: {err}"))?;
    let roster = rosters
        .into_iter()
        .find(|r| r.id == roster_id)
        .ok_or_else(|| format!("codex roster `{roster_id}` not found"))?;
    let roster_backend = match roster.run_profile.backend.as_deref() {
        Some(raw) => Some(
            raw.parse::<BackendKind>()
                .map_err(|_| format!("unknown codex roster backend `{raw}`"))?,
        ),
        None => None,
    };
    let backend = base.backend.or(roster_backend);
    let include_closure = roster.resolution.include_transitive;
    let overlay = roster.run_profile_overlay();
    let selectors = codex_roster_selector_set(&roster, include_closure);
    Ok(KatachiTarget {
        harness: HarnessKind::Codex,
        roster_id: Some(roster_id.to_string()),
        backend,
        preference: base.preference,
        selectors,
        run_profile_overlay: merge_overlay(&base.run_profile_overlay, overlay),
    })
}

fn codex_roster_selector_set(
    roster: &CodexRosterFile,
    include_closure: bool,
) -> katachi_core::selector::SelectorSet {
    use katachi_core::selector::{Selector, SelectorSet};
    let mut selectors = Vec::new();
    for (kind, ids) in roster.selection.by_kind() {
        for id in ids {
            selectors.push(Selector::Glob {
                kind: Some(kind.as_str().to_string()),
                pattern: format!("*{id}"),
            });
        }
    }
    SelectorSet {
        selectors,
        include_packaging_closure: include_closure,
        include_semantic_closure: include_closure,
    }
}

fn project_gemini_target(
    base: &KatachiTarget,
    roster_id: &str,
    storage: &StoragePaths,
) -> std::result::Result<KatachiTarget, String> {
    let dir = storage.rosters_dir().join("gemini");
    let store = GeminiRosterStore::load_dir(&dir)
        .map_err(|err| format!("loading gemini rosters: {err}"))?;
    let roster = store
        .find(roster_id)
        .ok_or_else(|| format!("gemini roster `{roster_id}` not found"))?
        .clone();
    let projected = roster.to_katachi_definition();
    let projected_target = projected
        .targets
        .into_iter()
        .next()
        .ok_or_else(|| "gemini roster projection missing target".to_string())?;
    let roster_backend = match roster.run_profile.backend.as_deref() {
        Some(raw) => Some(
            raw.parse::<BackendKind>()
                .map_err(|_| format!("unknown gemini roster backend `{raw}`"))?,
        ),
        None => projected_target.backend,
    };
    Ok(KatachiTarget {
        harness: HarnessKind::Gemini,
        roster_id: Some(roster_id.to_string()),
        backend: base.backend.or(roster_backend),
        preference: base.preference,
        selectors: projected_target.selectors,
        run_profile_overlay: merge_overlay(
            &base.run_profile_overlay,
            projected_target.run_profile_overlay,
        ),
    })
}

/// Merge a target-level overlay on top of the roster overlay. Target
/// fields win field-by-field per policy §6.
fn merge_overlay(target: &serde_json::Value, mut roster: serde_json::Value) -> serde_json::Value {
    if target.is_null() {
        return roster;
    }
    if let (Some(target_obj), Some(roster_obj)) = (target.as_object(), roster.as_object_mut()) {
        for (k, v) in target_obj {
            roster_obj.insert(k.clone(), v.clone());
        }
        return roster;
    }
    target.clone()
}

fn emit_config_error_message(global: &GlobalArgs, msg: &str) -> Result<()> {
    if global.json {
        let obj = serde_json::json!({
            "error": {
                "kind": "config",
                "message": msg,
            }
        });
        serde_json::to_writer_pretty(std::io::stdout(), &obj)?;
        println!();
    } else {
        eprintln!("katachi config error: {msg}");
    }
    Ok(())
}

fn resolve_cwd(global: &GlobalArgs) -> Result<Utf8PathBuf> {
    if let Some(cwd) = &global.cwd {
        return Ok(cwd.clone());
    }
    let std_cwd = std::env::current_dir()?;
    Utf8PathBuf::from_path_buf(std_cwd)
        .map_err(|p| anyhow!("cwd `{}` is not valid UTF-8", p.display()))
}

fn build_request(
    global: &GlobalArgs,
    id: &str,
    cwd: Utf8PathBuf,
    action: ActionRequest,
) -> InvocationRequest {
    let preferred_harnesses: Vec<HarnessKind> = global
        .prefer_harness
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    let preferred_backends: Vec<BackendKind> = global
        .prefer_backend
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    let mut req = InvocationRequest::new(id, action, cwd);
    req.preferred_harnesses = preferred_harnesses;
    req.preferred_backends = preferred_backends;
    if let Some(m) = global.materialization {
        req.materialization = match m {
            MaterializationArg::Ambient => MaterializationMode::Ambient,
            MaterializationArg::TempOverlay => MaterializationMode::TempOverlay,
        };
    }
    req.dry_run = global.dry_run;
    req
}

fn emit_resolve_error(global: &GlobalArgs, err: &ResolveError) -> Result<()> {
    emit_resolve_error_message(global, &err.to_string())
}

fn emit_resolve_error_message(global: &GlobalArgs, msg: &str) -> Result<()> {
    if global.json {
        let obj = serde_json::json!({
            "error": {
                "kind": "resolve",
                "message": msg,
            }
        });
        serde_json::to_writer_pretty(std::io::stdout(), &obj)?;
        println!();
    } else {
        eprintln!("katachi resolve error: {msg}");
    }
    Ok(())
}

/// Report payload shared by the human and JSON renderers.
///
/// `resolved.diagnostics` carries resolver warnings (e.g. cycles), while the
/// sibling `diagnostics` field holds validator output. Keeping them split
/// makes provenance obvious downstream.
#[derive(Serialize)]
struct DescribeReport<'a> {
    resolved: &'a ResolvedKatachi,
    diagnostics: &'a [Diagnostic],
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
}

fn render_human(report: &DescribeReport<'_>) {
    let r = report.resolved;
    println!("katachi: {}", r.katachi_id);
    if let Some(desc) = report.description {
        println!("description: {desc}");
    }
    println!("harness: {} (backend: {})", r.harness, r.backend);
    println!();

    if r.selected_items.is_empty() {
        println!("selected items: (none)");
    } else {
        println!("selected items ({}):", r.selected_items.len());
        let mut by_kind: BTreeMap<&str, Vec<&ResolvedItemRef>> = BTreeMap::new();
        for item in &r.selected_items {
            by_kind
                .entry(item.item.kind.as_str())
                .or_default()
                .push(item);
        }
        for (kind, items) in &by_kind {
            println!("  [{kind}]");
            for item in items {
                let reason = reason_label(item);
                println!("    - {}  ({reason})", item.item.id);
            }
        }
    }

    let has_any_diag = !r.diagnostics.is_empty() || !report.diagnostics.is_empty();
    if has_any_diag {
        println!();
        println!("diagnostics:");
        for d in r.diagnostics.iter().chain(report.diagnostics.iter()) {
            let sev = severity_tag(d.severity);
            println!("  [{sev}] {}: {}", d.code, d.message);
        }
    }
}

fn reason_label(item: &ResolvedItemRef) -> String {
    match item.reason {
        SelectionReason::Direct => "direct".to_string(),
        SelectionReason::PackagingClosure => match &item.pulled_in_by {
            Some(p) => format!("packaging-closure via {p}"),
            None => "packaging-closure".to_string(),
        },
        SelectionReason::SemanticClosure => match &item.pulled_in_by {
            Some(p) => format!("semantic-closure via {p}"),
            None => "semantic-closure".to_string(),
        },
    }
}

fn severity_tag(sev: Severity) -> &'static str {
    match sev {
        Severity::Error => "error",
        Severity::Warning => "warn ",
        Severity::Info => "info ",
    }
}

#[derive(Serialize)]
struct GraphView<'a> {
    katachi_id: &'a str,
    harness: HarnessKind,
    backend: BackendKind,
    items: Vec<GraphItem<'a>>,
    edges: Vec<GraphEdge<'a>>,
    diagnostics: Vec<&'a Diagnostic>,
}

#[derive(Serialize)]
struct GraphItem<'a> {
    item_ref: &'a ItemRef,
    display_name: &'a str,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

#[derive(Serialize)]
struct GraphEdge<'a> {
    from: &'a ItemRef,
    to: &'a ItemRef,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
}

struct Subgraph<'a> {
    items: Vec<GraphItem<'a>>,
    edges: Vec<GraphEdge<'a>>,
}

fn filter_catalog<'a>(catalog: &'a RosterCatalog, selected: &BTreeSet<ItemRef>) -> Subgraph<'a> {
    let mut items: Vec<GraphItem<'a>> = catalog
        .iter_items()
        .filter(|(item_ref, _)| selected.contains(*item_ref))
        .map(|(item_ref, item)| GraphItem {
            item_ref,
            display_name: &item.display_name,
            kind: item_ref.kind.as_str(),
            scope: item.source.scope.as_deref(),
            path: item.source.path.as_ref().map(|p| p.to_string()),
        })
        .collect();
    items.sort_by(|a, b| a.item_ref.id.cmp(&b.item_ref.id));

    let mut edges: Vec<GraphEdge<'a>> = catalog
        .iter_edges()
        .filter(|e| selected.contains(&e.from) && selected.contains(&e.to))
        .map(|e| GraphEdge {
            from: &e.from,
            to: &e.to,
            kind: edge_kind_str(e.kind),
            note: e.note.as_deref(),
        })
        .collect();
    edges.sort_by(|a, b| a.from.id.cmp(&b.from.id).then(a.to.id.cmp(&b.to.id)));

    Subgraph { items, edges }
}

fn edge_kind_str(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Packaging => "packaging",
        EdgeKind::Semantic => "semantic",
        EdgeKind::Projection => "projection",
    }
}

fn render_graph_text(view: &GraphView<'_>) {
    println!("katachi: {}", view.katachi_id);
    println!("harness: {} (backend: {})", view.harness, view.backend);
    println!();
    if view.items.is_empty() {
        println!("selected items: (none)");
    } else {
        println!("selected items ({}):", view.items.len());
        for it in &view.items {
            println!("  - {}  [{}]", it.item_ref, it.display_name);
        }
    }
    println!();
    if view.edges.is_empty() {
        println!("edges: (none)");
    } else {
        println!("edges ({}):", view.edges.len());
        for e in &view.edges {
            let note = e.note.map(|n| format!(" ({n})")).unwrap_or_default();
            println!("  {} --[{}]--> {}{}", e.from, e.kind, e.to, note);
        }
    }
    if !view.diagnostics.is_empty() {
        println!();
        println!("diagnostics:");
        for d in &view.diagnostics {
            println!("  [{}] {}: {}", severity_tag(d.severity), d.code, d.message);
        }
    }
}

fn render_graph_dot(view: &GraphView<'_>) {
    println!("digraph katachi_{} {{", sanitize_dot_id(view.katachi_id));
    println!("  rankdir=LR;");
    for it in &view.items {
        println!(
            "  \"{}\" [label=\"{}\\n{}\"];",
            it.item_ref, it.item_ref.id, it.kind
        );
    }
    for e in &view.edges {
        let note = e.note.unwrap_or("");
        println!(
            "  \"{}\" -> \"{}\" [label=\"{}{}{}\"];",
            e.from,
            e.to,
            e.kind,
            if note.is_empty() { "" } else { ": " },
            note
        );
    }
    println!("}}");
}

fn sanitize_dot_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}
