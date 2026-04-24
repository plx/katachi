//! `katachi harness <name> ...` — drive a specific harness directly.
//!
//! Phase 3 starts with Claude; the Codex and Gemini branches stay as
//! `NotImplemented` until their own milestones land.

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use camino::Utf8PathBuf;
use serde::Serialize;

use katachi_core::config;
use katachi_core::diagnostic::{Diagnostic, Severity};
use katachi_core::harness::{ExplainContext, HarnessModule, ScanContext};
use katachi_core::model::{BackendKind, ItemRef, MaterializationMode};
use katachi_core::paths::{resolve_config_file, resolve_storage_paths, PathOverrides};
use katachi_core::persist::RunDirectory;
use katachi_core::plan::{ActionRequest, ExecutionPlan, InvocationRequest};
use katachi_core::record::RunId;

use katachi_harness_claude::config::ClaudeConfig;
use katachi_harness_claude::plan::{
    build_claude_plan, materialize_overlay, ClaudePlanInputs, MaterializedOverlay,
};
use katachi_harness_claude::resolve::{resolve_roster, validate, ResolvedClaudeRoster};
use katachi_harness_claude::roster::ClaudeRosterStore;
use katachi_harness_claude::ClaudeHarness;

use crate::cli::{
    GlobalArgs, GraphFormat, HarnessAction, HarnessCmd, HarnessName, HarnessPlanAction,
    MaterializationArg, SdkTarget,
};
use crate::exit::ExitCode;

use katachi_harness_claude::sdk;

pub fn run(global: &GlobalArgs, cmd: &HarnessCmd) -> Result<ExitCode> {
    match cmd.name {
        HarnessName::Claude => run_claude(global, &cmd.action),
        HarnessName::Codex | HarnessName::Gemini => {
            eprintln!(
                "katachi: `harness {}` is not yet implemented in this phase",
                cmd.name.as_str()
            );
            Ok(ExitCode::NotImplemented)
        }
    }
}

struct ClaudeCtx {
    harness: ClaudeHarness,
    config: config::KatachiConfig,
    claude_config: ClaudeConfig,
    storage: katachi_core::paths::StoragePaths,
    cwd: Utf8PathBuf,
}

fn build_ctx(global: &GlobalArgs) -> Result<ClaudeCtx> {
    let overrides = PathOverrides {
        config_file: global.config.clone(),
        data_root: global.data_root.clone(),
        cache_root: global.cache_root.clone(),
    };
    let config_path = resolve_config_file(&overrides)?;
    let load = config::load(config_path)?;
    let storage = resolve_storage_paths(&overrides, &load.config.storage)?;
    let cwd = resolve_cwd(global)?;
    let claude_config = ClaudeConfig::from_shared(&load.config);
    Ok(ClaudeCtx {
        harness: ClaudeHarness::new(),
        config: load.config,
        claude_config,
        storage,
        cwd,
    })
}

fn run_claude(global: &GlobalArgs, action: &HarnessAction) -> Result<ExitCode> {
    let ctx = build_ctx(global)?;
    match action {
        HarnessAction::Scan => run_scan(global, &ctx),
        HarnessAction::Explain { item_id } => run_explain(global, &ctx, item_id),
        HarnessAction::Graph { format } => run_graph(global, &ctx, *format),
        HarnessAction::Plan { roster_id, what } => match what {
            HarnessPlanAction::Execute { prompt } => {
                run_plan(global, &ctx, roster_id, Some(prompt))
            }
        },
        HarnessAction::Execute { roster_id, prompt } => {
            run_execute(global, &ctx, roster_id, prompt)
        }
        HarnessAction::Doctor => run_doctor(global, &ctx),
        HarnessAction::DumpRoster { roster_id } => run_dump_roster(global, &ctx, roster_id),
        HarnessAction::Project { roster_id, sdk } => run_project(global, &ctx, roster_id, *sdk),
    }
}

fn run_doctor(global: &GlobalArgs, ctx: &ClaudeCtx) -> Result<ExitCode> {
    let binary = &ctx.claude_config.binary;
    let (found, resolved) = which::which(binary)
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .map(|p| (true, Some(p)))
        .unwrap_or((false, None));

    let rosters_dir = katachi_harness_claude::roster::roster_dir(&ctx.storage, &ctx.claude_config);
    let plugin_roots: Vec<String> = ctx
        .claude_config
        .plugin_roots
        .iter()
        .map(|p| p.to_string())
        .collect();

    if global.json {
        let payload = serde_json::json!({
            "binary": binary,
            "found": found,
            "resolved": resolved,
            "rosters_dir": rosters_dir,
            "plugin_roots": plugin_roots,
            "user_root": ctx.claude_config.user_root,
            "project_roots": ctx.claude_config.project_roots,
        });
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        println!("claude doctor");
        println!("  binary     : {binary}");
        if found {
            println!(
                "  resolved   : {}",
                resolved.as_deref().unwrap_or("?")
            );
        } else {
            println!("  resolved   : not found on PATH");
        }
        println!("  rosters    : {rosters_dir}");
        println!("  user_root  : {}", ctx.claude_config.user_root);
        println!("  plugin_roots :");
        for p in &plugin_roots {
            println!("    - {p}");
        }
        println!("  project_roots:");
        for p in &ctx.claude_config.project_roots {
            println!("    - {p}");
        }
    }
    if !found {
        Ok(ExitCode::Config)
    } else {
        Ok(ExitCode::Ok)
    }
}

fn run_dump_roster(global: &GlobalArgs, ctx: &ClaudeCtx, roster_id: &str) -> Result<ExitCode> {
    let (resolved, _req) = match resolve_for_roster(global, ctx, roster_id) {
        Ok(x) => x,
        Err(err) => {
            eprintln!("katachi harness claude dump-roster: {err:#}");
            return Ok(ExitCode::Resolve);
        }
    };
    let validator_diags = validate(&resolved);
    let payload = serde_json::json!({
        "roster": &resolved.roster,
        "resolved": &resolved.resolved,
        "projection_diagnostics": &resolved.projection_diagnostics,
        "validator_diagnostics": &validator_diags,
        "catalog": &resolved.catalog,
    });
    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".into())
        );
    }
    let has_errors = validator_diags.iter().any(|d| d.severity == Severity::Error)
        || resolved.resolved.diagnostics.iter().any(|d| d.severity == Severity::Error);
    Ok(if has_errors { ExitCode::Validate } else { ExitCode::Ok })
}

fn run_project(
    global: &GlobalArgs,
    ctx: &ClaudeCtx,
    roster_id: &str,
    sdk_target: SdkTarget,
) -> Result<ExitCode> {
    let (resolved, _req) = match resolve_for_roster(global, ctx, roster_id) {
        Ok(x) => x,
        Err(err) => {
            eprintln!("katachi harness claude project: {err:#}");
            return Ok(ExitCode::Resolve);
        }
    };
    let backend = match sdk_target {
        SdkTarget::Ts => BackendKind::SdkTs,
        SdkTarget::Py => BackendKind::SdkPy,
    };
    let projection = sdk::project(&resolved, backend);
    if global.json {
        let payload = serde_json::json!({
            "backend": projection.backend,
            "code": projection.code,
            "diagnostics": projection.diagnostics,
        });
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        print!("{}", projection.code);
        if !projection.diagnostics.is_empty() {
            eprintln!("--- projection diagnostics ---");
            for d in &projection.diagnostics {
                eprintln!("[{}] {}: {}", severity_tag(d.severity), d.code, d.message);
            }
        }
    }
    let has_errors = projection
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error);
    Ok(if has_errors { ExitCode::Plan } else { ExitCode::Ok })
}

fn run_scan(global: &GlobalArgs, ctx: &ClaudeCtx) -> Result<ExitCode> {
    let catalog = ctx
        .harness
        .scan(&ScanContext {
            config: &ctx.config,
            paths: &ctx.storage,
            cwd: &ctx.cwd,
        })
        .map_err(|err| anyhow!("{err:#}"))?;
    render_scan(global, &catalog)
}

fn run_explain(global: &GlobalArgs, ctx: &ClaudeCtx, item_id: &str) -> Result<ExitCode> {
    let catalog = ctx
        .harness
        .scan(&ScanContext {
            config: &ctx.config,
            paths: &ctx.storage,
            cwd: &ctx.cwd,
        })
        .map_err(|err| anyhow!("{err:#}"))?;
    let item_ref = resolve_item_ref(item_id, &catalog)?;
    match ctx.harness.explain(&ExplainContext {
        item: &item_ref,
        catalog: &catalog,
        cwd: &ctx.cwd,
    }) {
        Ok(res) => {
            render_explain(global, &res);
            Ok(ExitCode::Ok)
        }
        Err(err) => {
            eprintln!("katachi harness claude explain: {err:#}");
            Ok(ExitCode::Resolve)
        }
    }
}

fn run_graph(global: &GlobalArgs, ctx: &ClaudeCtx, format: GraphFormat) -> Result<ExitCode> {
    let catalog = ctx
        .harness
        .scan(&ScanContext {
            config: &ctx.config,
            paths: &ctx.storage,
            cwd: &ctx.cwd,
        })
        .map_err(|err| anyhow!("{err:#}"))?;
    render_graph(global, &catalog, format);
    Ok(ExitCode::Ok)
}

fn resolve_for_roster(
    global: &GlobalArgs,
    ctx: &ClaudeCtx,
    roster_id: &str,
) -> Result<(ResolvedClaudeRoster, InvocationRequest)> {
    let store = ClaudeRosterStore::load_default(&ctx.storage, &ctx.claude_config)
        .map_err(|err| anyhow!("load rosters: {err:#}"))?;
    let roster = store
        .require(roster_id)
        .map_err(|err| anyhow!("{err:#}"))?
        .clone();
    let catalog = ctx
        .harness
        .scan(&ScanContext {
            config: &ctx.config,
            paths: &ctx.storage,
            cwd: &ctx.cwd,
        })
        .map_err(|err| anyhow!("{err:#}"))?;
    let backend = resolve_backend(global, &roster);
    let resolved = resolve_roster(&roster, catalog, backend);
    let action = ActionRequest::Describe;
    let request = InvocationRequest::new(roster_id, action, ctx.cwd.clone());
    Ok((resolved, request))
}

fn resolve_backend(
    global: &GlobalArgs,
    roster: &katachi_harness_claude::roster::ClaudeRoster,
) -> BackendKind {
    // CLI `--prefer-backend` wins over roster/config defaults.
    for b in &global.prefer_backend {
        if let Ok(parsed) = b.parse::<BackendKind>() {
            return parsed;
        }
    }
    if let Some(b) = roster.backend() {
        return b;
    }
    BackendKind::Cli
}

fn build_plan_from_resolved(
    global: &GlobalArgs,
    ctx: &ClaudeCtx,
    resolved: &ResolvedClaudeRoster,
    prompt: Option<&str>,
) -> Result<ExecutionPlan> {
    let materialization = match global.materialization {
        Some(MaterializationArg::Ambient) => MaterializationMode::Ambient,
        Some(MaterializationArg::TempOverlay) => MaterializationMode::TempOverlay,
        None => resolved.roster.materialization_mode(),
    };
    let run_id = RunId::new();
    let plan = build_claude_plan(ClaudePlanInputs {
        resolved_roster: resolved,
        config: &ctx.claude_config,
        cwd: &ctx.cwd,
        run_id,
        materialization,
        prompt: prompt.map(str::to_string),
    })
    .map_err(|err| anyhow!("{err:#}"))?;
    Ok(plan)
}

fn run_plan(
    global: &GlobalArgs,
    ctx: &ClaudeCtx,
    roster_id: &str,
    prompt: Option<&str>,
) -> Result<ExitCode> {
    let (resolved, _request) = match resolve_for_roster(global, ctx, roster_id) {
        Ok(x) => x,
        Err(err) => {
            eprintln!("katachi harness claude plan: {err:#}");
            return Ok(ExitCode::Resolve);
        }
    };
    let validator_diags = validate(&resolved);
    let resolve_errors = resolved
        .resolved
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error);
    let validation_errors = validator_diags
        .iter()
        .any(|d| d.severity == Severity::Error);
    if resolve_errors {
        render_diagnostics(global, &resolved.resolved.diagnostics, &validator_diags);
        return Ok(ExitCode::Resolve);
    }
    if validation_errors {
        render_diagnostics(global, &resolved.resolved.diagnostics, &validator_diags);
        return Ok(ExitCode::Validate);
    }

    let plan = match build_plan_from_resolved(global, ctx, &resolved, prompt) {
        Ok(plan) => plan,
        Err(err) => {
            eprintln!("katachi harness claude plan: {err:#}");
            return Ok(ExitCode::Plan);
        }
    };
    render_plan(global, &plan, &resolved, &validator_diags);
    Ok(ExitCode::Ok)
}

fn run_execute(
    global: &GlobalArgs,
    ctx: &ClaudeCtx,
    roster_id: &str,
    prompt: &str,
) -> Result<ExitCode> {
    let (resolved, _request_template) = match resolve_for_roster(global, ctx, roster_id) {
        Ok(x) => x,
        Err(err) => {
            eprintln!("katachi harness claude execute: {err:#}");
            return Ok(ExitCode::Resolve);
        }
    };
    let validator_diags = validate(&resolved);
    if resolved
        .resolved
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        render_diagnostics(global, &resolved.resolved.diagnostics, &validator_diags);
        return Ok(ExitCode::Resolve);
    }
    if validator_diags.iter().any(|d| d.severity == Severity::Error) {
        render_diagnostics(global, &resolved.resolved.diagnostics, &validator_diags);
        return Ok(ExitCode::Validate);
    }

    let plan = match build_plan_from_resolved(global, ctx, &resolved, Some(prompt)) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("katachi harness claude execute: {err:#}");
            return Ok(ExitCode::Plan);
        }
    };

    if global.dry_run {
        render_plan(global, &plan, &resolved, &validator_diags);
        return Ok(ExitCode::Ok);
    }

    // Materialize overlay (if any) and spawn the CLI.
    let overlay = if matches!(plan.materialization.mode, MaterializationMode::TempOverlay)
        && !plan.materialization.files.is_empty()
    {
        match materialize_overlay(&plan.materialization) {
            Ok(o) => Some(o),
            Err(err) => {
                eprintln!("katachi harness claude execute: overlay failed: {err:#}");
                return Ok(ExitCode::Execute);
            }
        }
    } else {
        None
    };

    // Build run directory.
    let run_id = plan.run_id;
    let runs_dir = ctx.storage.runs_dir();
    let run_dir = match RunDirectory::create(&runs_dir, run_id) {
        Ok(d) => d,
        Err(err) => {
            eprintln!("katachi harness claude execute: {err:#}");
            cleanup_overlay(overlay, ctx.claude_config.preserve_failed_overlays, false);
            return Ok(ExitCode::Execute);
        }
    };
    let request = build_execute_request(global, roster_id, prompt, &ctx.cwd);
    if let Err(err) = run_dir.write_request(&request) {
        eprintln!("katachi harness claude execute: {err:#}");
        cleanup_overlay(overlay, ctx.claude_config.preserve_failed_overlays, false);
        return Ok(ExitCode::Execute);
    }
    if let Err(err) = run_dir.write_plan(&plan) {
        eprintln!("katachi harness claude execute: {err:#}");
        cleanup_overlay(overlay, ctx.claude_config.preserve_failed_overlays, false);
        return Ok(ExitCode::Execute);
    }

    let exec_ctx = katachi_core::harness::ExecuteContext {
        request: &request,
        plan: &plan,
        run_dir: &run_dir,
        started_at: time::OffsetDateTime::now_utc(),
    };
    let record_result = ctx.harness.execute(&exec_ctx);
    // Always write manifest before committing.
    let _ = run_dir.write_manifest();

    let exit = match &record_result {
        Ok(rec) => match rec.result.outcome {
            katachi_core::record::Outcome::Success => ExitCode::Ok,
            katachi_core::record::Outcome::Failure
            | katachi_core::record::Outcome::Timeout
            | katachi_core::record::Outcome::Planned => ExitCode::Execute,
        },
        Err(err) => {
            eprintln!("katachi harness claude execute: {err:#}");
            ExitCode::Execute
        }
    };

    // Commit on success, leave partial on failure (preserves diagnostics).
    if matches!(exit, ExitCode::Ok) {
        match run_dir.commit() {
            Ok(_) => {}
            Err(err) => eprintln!("katachi harness claude execute: commit failed: {err:#}"),
        }
    }

    cleanup_overlay(
        overlay,
        ctx.claude_config.preserve_failed_overlays,
        matches!(exit, ExitCode::Ok),
    );
    Ok(exit)
}

fn cleanup_overlay(
    overlay: Option<MaterializedOverlay>,
    preserve_on_failure: bool,
    success: bool,
) {
    let Some(overlay) = overlay else { return };
    if !success && preserve_on_failure {
        if let MaterializedOverlay::Fixed { root } = &overlay {
            eprintln!("katachi: preserved overlay for post-mortem at `{root}`");
        }
        // TempOverlay already lives in a system tempdir; leak it.
        if let MaterializedOverlay::Temp(mut t) = overlay {
            t.set_keep(katachi_core::materialize::KeepPolicy::Keep);
        }
        return;
    }
    if let Err(err) = overlay.cleanup() {
        eprintln!("katachi: failed to clean up overlay: {err:#}");
    }
}

fn build_execute_request(
    global: &GlobalArgs,
    roster_id: &str,
    prompt: &str,
    cwd: &Utf8PathBuf,
) -> InvocationRequest {
    let preferred_harnesses: Vec<_> = global
        .prefer_harness
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    let preferred_backends: Vec<_> = global
        .prefer_backend
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    let mut req = InvocationRequest::new(
        roster_id,
        ActionRequest::Execute {
            prompt: prompt.to_string(),
        },
        cwd.clone(),
    );
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

fn resolve_cwd(global: &GlobalArgs) -> Result<Utf8PathBuf> {
    if let Some(cwd) = &global.cwd {
        return Ok(cwd.clone());
    }
    let std_cwd = std::env::current_dir()?;
    Utf8PathBuf::from_path_buf(std_cwd)
        .map_err(|p| anyhow!("cwd `{}` is not valid UTF-8", p.display()))
}

fn resolve_item_ref(raw: &str, catalog: &katachi_core::harness::RosterCatalog) -> Result<ItemRef> {
    if let Ok(parsed) = raw.parse::<ItemRef>() {
        if catalog.contains(&parsed) {
            return Ok(parsed);
        }
    }
    if let Some((kind, id)) = raw.split_once(':') {
        for (ir, _) in catalog.iter_items() {
            if ir.kind == kind && ir.id == id {
                return Ok(ir.clone());
            }
        }
    }
    let matches: Vec<&ItemRef> = catalog
        .iter_items()
        .filter(|(ir, _)| ir.id == raw)
        .map(|(ir, _)| ir)
        .collect();
    if matches.len() == 1 {
        return Ok(matches[0].clone());
    }
    if matches.len() > 1 {
        return Err(anyhow!(
            "ambiguous item id `{raw}`; qualify with kind (kind:id) or a full item-ref"
        ));
    }
    Err(anyhow!("no discovered item matches `{raw}`"))
}

fn render_scan(
    global: &GlobalArgs,
    catalog: &katachi_core::harness::RosterCatalog,
) -> Result<ExitCode> {
    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), catalog)?;
        println!();
    } else {
        human_render_scan(catalog);
    }
    Ok(ExitCode::Ok)
}

fn human_render_scan(catalog: &katachi_core::harness::RosterCatalog) {
    if catalog.items.is_empty() {
        println!("claude scan: no items discovered");
    } else {
        let mut by_kind: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (item_ref, item) in catalog.iter_items() {
            let line = if item.display_name == item_ref.id {
                item_ref.id.clone()
            } else {
                format!("{} — {}", item_ref.id, item.display_name)
            };
            by_kind
                .entry(item_ref.kind.as_str())
                .or_default()
                .push(line);
        }
        println!(
            "claude scan: {} item(s) across {} kind(s)",
            catalog.items.len(),
            by_kind.len()
        );
        for (kind, rows) in &by_kind {
            println!("  [{kind}] ({})", rows.len());
            for r in rows {
                println!("    - {r}");
            }
        }
    }
    if !catalog.edges.is_empty() {
        println!("edges: {}", catalog.edges.len());
    }
    if !catalog.diagnostics.is_empty() {
        println!("diagnostics:");
        for d in &catalog.diagnostics {
            println!("  [{}] {}: {}", severity_tag(d.severity), d.code, d.message);
        }
    }
}

fn render_explain(global: &GlobalArgs, res: &katachi_core::harness::ExplainResult) {
    if global.json {
        let _ = serde_json::to_writer_pretty(std::io::stdout(), res);
        println!();
    } else {
        println!("item: {}", res.item);
        println!("summary: {}", res.summary);
        for section in &res.sections {
            println!();
            println!("[{}]", section.title);
            println!("{}", section.body);
        }
    }
}

fn render_graph(
    global: &GlobalArgs,
    catalog: &katachi_core::harness::RosterCatalog,
    format: GraphFormat,
) {
    match (global.json, format) {
        (true, _) | (_, GraphFormat::Json) => {
            let wire = GraphWire::from(catalog);
            let _ = serde_json::to_writer_pretty(std::io::stdout(), &wire);
            println!();
        }
        (_, GraphFormat::Dot) => {
            print_dot(catalog);
        }
        (_, GraphFormat::Text) => {
            for (item_ref, item) in catalog.iter_items() {
                println!("{} ({})", item_ref, item.display_name);
            }
            for e in catalog.iter_edges() {
                let label = e.note.as_deref().unwrap_or("");
                println!("  {} -[{}]-> {}", e.from, label, e.to);
            }
        }
    }
}

fn render_plan(
    global: &GlobalArgs,
    plan: &ExecutionPlan,
    resolved: &ResolvedClaudeRoster,
    validator_diags: &[Diagnostic],
) {
    if global.json {
        let payload = serde_json::json!({
            "plan": plan,
            "resolved": resolved.resolved,
            "validator_diagnostics": validator_diags,
            "projection_diagnostics": resolved.projection_diagnostics,
        });
        let _ = serde_json::to_writer_pretty(std::io::stdout(), &payload);
        println!();
    } else {
        println!("claude plan: {}", plan.summary);
        println!("backend: {} ({})", plan.backend, plan.harness);
        println!(
            "materialization: {:?} ({} file(s))",
            plan.materialization.mode,
            plan.materialization.files.len()
        );
        if let Some(root) = &plan.materialization.overlay_root {
            println!("overlay_root: {root}");
        }
        println!("argv:");
        for (i, a) in plan.execution.argv.iter().enumerate() {
            println!("  [{i}] {a}");
        }
        if let Some(cwd) = &plan.execution.cwd {
            println!("cwd: {cwd}");
        }
        if !plan.execution.env.is_empty() {
            println!("env:");
            for (k, v) in &plan.execution.env {
                println!("  {k}={v}");
            }
        }
        render_diagnostics(global, &resolved.resolved.diagnostics, validator_diags);
    }
}

fn render_diagnostics(
    _global: &GlobalArgs,
    resolver: &[Diagnostic],
    validator: &[Diagnostic],
) {
    if resolver.is_empty() && validator.is_empty() {
        return;
    }
    println!("diagnostics:");
    for d in resolver.iter().chain(validator.iter()) {
        println!("  [{}] {}: {}", severity_tag(d.severity), d.code, d.message);
    }
}

#[derive(Serialize)]
struct GraphWire<'a> {
    items: Vec<&'a ItemRef>,
    edges: Vec<EdgeWire<'a>>,
    diagnostics: &'a [Diagnostic],
}

#[derive(Serialize)]
struct EdgeWire<'a> {
    from: &'a ItemRef,
    to: &'a ItemRef,
    kind: katachi_core::harness::EdgeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
}

impl<'a> From<&'a katachi_core::harness::RosterCatalog> for GraphWire<'a> {
    fn from(c: &'a katachi_core::harness::RosterCatalog) -> Self {
        Self {
            items: c.iter_items().map(|(ir, _)| ir).collect(),
            edges: c
                .iter_edges()
                .map(|e| EdgeWire {
                    from: &e.from,
                    to: &e.to,
                    kind: e.kind,
                    note: e.note.as_deref(),
                })
                .collect(),
            diagnostics: &c.diagnostics,
        }
    }
}

fn print_dot(catalog: &katachi_core::harness::RosterCatalog) {
    println!("digraph claude {{");
    println!("  rankdir=LR;");
    for (item_ref, _) in catalog.iter_items() {
        println!("  \"{item_ref}\";");
    }
    for e in catalog.iter_edges() {
        let label = e.note.as_deref().unwrap_or("");
        println!("  \"{}\" -> \"{}\" [label=\"{}\"];", e.from, e.to, label);
    }
    println!("}}");
}

fn severity_tag(sev: Severity) -> &'static str {
    match sev {
        Severity::Error => "error",
        Severity::Warning => "warn ",
        Severity::Info => "info ",
    }
}
