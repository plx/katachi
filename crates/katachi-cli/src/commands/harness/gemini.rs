//! Gemini-specific `katachi harness gemini ...` subcommand handlers.

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{anyhow, Result};
use camino::Utf8PathBuf;
use serde::Serialize;

use katachi_core::config;
use katachi_core::diagnostic::{any_error, Diagnostic};
use katachi_core::harness::{
    ExplainContext, ExplainResult, HarnessModule, PlanContext, RosterCatalog, ScanContext,
};
use katachi_core::katachi::KatachiDefinition;
use katachi_core::model::{BackendKind, HarnessKind, ItemRef, MaterializationMode};
use katachi_core::paths::{resolve_config_file, resolve_storage_paths, PathOverrides, StoragePaths};
use katachi_core::plan::{ActionRequest, InvocationRequest, PLAN_SCHEMA_VERSION};
use katachi_core::record::RunId;
use katachi_core::resolve::{resolve, ResolveInputs};
use katachi_core::validate::{default_validators, run_validators, ValidateContext};
use katachi_harness_gemini::roster::{GeminiRoster, GeminiRosterStore};
use katachi_harness_gemini::scan;
use katachi_harness_gemini::GeminiHarness;

use crate::cli::{
    GlobalArgs, GraphFormat, HarnessAction, HarnessPlanAction, MaterializationArg,
};
use crate::exit::ExitCode;

/// Dispatch an action on the gemini harness.
pub fn dispatch(global: &GlobalArgs, action: HarnessAction) -> Result<ExitCode> {
    match action {
        HarnessAction::Scan => run_scan(global),
        HarnessAction::Explain { item_id } => run_explain(global, &item_id),
        HarnessAction::Graph { format } => run_graph(global, format),
        HarnessAction::Plan {
            roster_id,
            what:
                HarnessPlanAction::Execute { prompt },
        } => run_plan(global, &roster_id, &prompt),
        HarnessAction::Execute { roster_id, prompt } => run_execute(global, &roster_id, &prompt),
    }
}

struct Ctx {
    config: config::KatachiConfig,
    storage: StoragePaths,
    cwd: Utf8PathBuf,
}

fn load_ctx(global: &GlobalArgs) -> Result<Ctx> {
    let overrides = PathOverrides {
        config_file: global.config.clone(),
        data_root: global.data_root.clone(),
        cache_root: global.cache_root.clone(),
    };
    let config_path = resolve_config_file(&overrides)?;
    let load = config::load(config_path)?;
    let storage = resolve_storage_paths(&overrides, &load.config.storage)?;
    let cwd = resolve_cwd(global)?;
    Ok(Ctx {
        config: load.config,
        storage,
        cwd,
    })
}

fn resolve_cwd(global: &GlobalArgs) -> Result<Utf8PathBuf> {
    if let Some(cwd) = &global.cwd {
        return Ok(cwd.clone());
    }
    let std_cwd = std::env::current_dir()?;
    Utf8PathBuf::from_path_buf(std_cwd)
        .map_err(|p| anyhow!("cwd `{}` is not valid UTF-8", p.display()))
}

fn catalog_for(ctx: &Ctx) -> Result<RosterCatalog, ExitCode> {
    let harness = GeminiHarness::new();
    let scan_ctx = ScanContext {
        config: &ctx.config,
        paths: &ctx.storage,
        cwd: &ctx.cwd,
    };
    harness.scan(&scan_ctx).map_err(|e| {
        eprintln!("katachi harness gemini: scan failed: {e}");
        ExitCode::Resolve
    })
}

// ---------------- scan ----------------

pub fn run_scan(global: &GlobalArgs) -> Result<ExitCode> {
    let ctx = load_ctx(global)?;
    let catalog = match catalog_for(&ctx) {
        Ok(c) => c,
        Err(code) => return Ok(code),
    };
    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &ScanReport::from(&catalog))?;
        println!();
    } else {
        render_scan_human(&catalog);
    }
    Ok(ExitCode::Ok)
}

#[derive(Serialize)]
struct ScanReport<'a> {
    harness: &'static str,
    item_count: usize,
    edge_count: usize,
    items: Vec<ItemSummary<'a>>,
    edges: Vec<EdgeSummary<'a>>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    diagnostics: &'a [Diagnostic],
}

#[derive(Serialize)]
struct ItemSummary<'a> {
    item_ref: &'a ItemRef,
    display_name: &'a str,
    scope: Option<&'a str>,
    path: Option<String>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    capabilities: &'a [String],
}

#[derive(Serialize)]
struct EdgeSummary<'a> {
    from: &'a ItemRef,
    to: &'a ItemRef,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
}

impl<'a> From<&'a RosterCatalog> for ScanReport<'a> {
    fn from(catalog: &'a RosterCatalog) -> Self {
        let items = catalog
            .iter_items()
            .map(|(_ref, item)| ItemSummary {
                item_ref: &item.item_ref,
                display_name: &item.display_name,
                scope: item.source.scope.as_deref(),
                path: item.source.path.as_ref().map(|p| p.to_string()),
                capabilities: &item.capabilities,
            })
            .collect();
        let edges = catalog
            .iter_edges()
            .map(|e| EdgeSummary {
                from: &e.from,
                to: &e.to,
                kind: match e.kind {
                    katachi_core::roster::EdgeKind::Packaging => "packaging",
                    katachi_core::roster::EdgeKind::Semantic => "semantic",
                    katachi_core::roster::EdgeKind::Projection => "projection",
                },
                note: e.note.as_deref(),
            })
            .collect();
        Self {
            harness: "gemini",
            item_count: catalog.items.len(),
            edge_count: catalog.edges.len(),
            items,
            edges,
            diagnostics: &catalog.diagnostics,
        }
    }
}

fn render_scan_human(catalog: &RosterCatalog) {
    println!("gemini roster scan");
    println!("  items: {}", catalog.items.len());
    println!("  edges: {}", catalog.edges.len());
    println!();
    let mut by_kind: BTreeMap<&str, Vec<&ItemRef>> = BTreeMap::new();
    for (item_ref, _) in catalog.iter_items() {
        by_kind.entry(item_ref.kind.as_str()).or_default().push(item_ref);
    }
    for (kind, refs) in &by_kind {
        println!("[{kind}] ({})", refs.len());
        for r in refs {
            let item = catalog.get(r).unwrap();
            println!("  - {} — {}", r.id, item.display_name);
        }
    }
    if !catalog.diagnostics.is_empty() {
        println!();
        println!("diagnostics:");
        for d in &catalog.diagnostics {
            println!("  [{}] {}: {}", severity_tag(d.severity), d.code, d.message);
        }
    }
}

fn severity_tag(sev: katachi_core::diagnostic::Severity) -> &'static str {
    match sev {
        katachi_core::diagnostic::Severity::Error => "error",
        katachi_core::diagnostic::Severity::Warning => "warn",
        katachi_core::diagnostic::Severity::Info => "info",
    }
}

// ---------------- explain ----------------

pub fn run_explain(global: &GlobalArgs, raw: &str) -> Result<ExitCode> {
    let ctx = load_ctx(global)?;
    let catalog = match catalog_for(&ctx) {
        Ok(c) => c,
        Err(code) => return Ok(code),
    };
    let item_ref = parse_explain_ref(raw, &catalog)?;
    let harness = GeminiHarness::new();
    let explain = harness.explain(&ExplainContext {
        item: &item_ref,
        catalog: &catalog,
        cwd: &ctx.cwd,
    });

    match explain {
        Ok(result) => {
            if global.json {
                serde_json::to_writer_pretty(std::io::stdout(), &result)?;
                println!();
            } else {
                render_explain_human(&result);
            }
            Ok(ExitCode::Ok)
        }
        Err(e) => {
            eprintln!("katachi harness gemini: {e}");
            Ok(ExitCode::Resolve)
        }
    }
}

fn parse_explain_ref(raw: &str, catalog: &RosterCatalog) -> Result<ItemRef> {
    // Accept `harness:kind:id`, `kind:id`, or plain id if unambiguous.
    if let Ok(full) = ItemRef::from_str(raw) {
        return Ok(full);
    }
    if let Some((kind, id)) = raw.split_once(':') {
        return Ok(ItemRef::new(HarnessKind::Gemini, kind, id));
    }
    let matches: Vec<&ItemRef> = catalog
        .iter_items()
        .filter_map(|(r, _)| if r.id == raw { Some(r) } else { None })
        .collect();
    match matches.len() {
        0 => Err(anyhow!("no item with id `{raw}`")),
        1 => Ok(matches[0].clone()),
        _ => Err(anyhow!("id `{raw}` is ambiguous: {} matches", matches.len())),
    }
}

fn render_explain_human(result: &ExplainResult) {
    println!("item   : {}", result.item);
    println!("summary: {}", result.summary);
    for section in &result.sections {
        println!();
        println!("{}", section.title);
        for line in section.body.lines() {
            println!("  {line}");
        }
    }
}

// ---------------- graph ----------------

pub fn run_graph(global: &GlobalArgs, format: GraphFormat) -> Result<ExitCode> {
    let ctx = load_ctx(global)?;
    let catalog = match catalog_for(&ctx) {
        Ok(c) => c,
        Err(code) => return Ok(code),
    };
    match format {
        GraphFormat::Json => {
            serde_json::to_writer_pretty(std::io::stdout(), &ScanReport::from(&catalog))?;
            println!();
        }
        GraphFormat::Dot => render_graph_dot(&catalog),
        GraphFormat::Text => render_graph_text(&catalog),
    }
    Ok(ExitCode::Ok)
}

fn render_graph_text(catalog: &RosterCatalog) {
    println!("gemini roster graph (text):");
    for edge in catalog.iter_edges() {
        let arrow = match edge.kind {
            katachi_core::roster::EdgeKind::Packaging => "──contains──>",
            katachi_core::roster::EdgeKind::Semantic => "──semantic──>",
            katachi_core::roster::EdgeKind::Projection => "──projection──>",
        };
        println!("  {} {arrow} {}", edge.from, edge.to);
    }
}

fn render_graph_dot(catalog: &RosterCatalog) {
    println!("digraph gemini_roster {{");
    for (item_ref, item) in catalog.iter_items() {
        println!(
            "  \"{}\" [label=\"{}\\n{}\"];",
            item_ref, item.display_name, item_ref.kind
        );
    }
    for edge in catalog.iter_edges() {
        let kind = match edge.kind {
            katachi_core::roster::EdgeKind::Packaging => "packaging",
            katachi_core::roster::EdgeKind::Semantic => "semantic",
            katachi_core::roster::EdgeKind::Projection => "projection",
        };
        println!(
            "  \"{}\" -> \"{}\" [label=\"{}\"];",
            edge.from, edge.to, kind
        );
    }
    println!("}}");
}

// ---------------- plan ----------------

pub fn run_plan(global: &GlobalArgs, roster_id: &str, prompt: &str) -> Result<ExitCode> {
    let ctx = load_ctx(global)?;
    let harness = GeminiHarness::new();
    let roster_store = load_rosters(&ctx)?;
    let roster = match roster_store.find(roster_id) {
        Some(r) => r.clone(),
        None => {
            eprintln!(
                "katachi harness gemini: roster `{}` not found under {}",
                roster_id,
                rosters_dir(&ctx).display()
            );
            return Ok(ExitCode::Resolve);
        }
    };
    let definition = roster.to_katachi_definition();
    let req = build_request_for_roster(global, roster_id, ctx.cwd.clone(), prompt);

    let modules: Vec<&dyn HarnessModule> = vec![&harness];
    let inputs = ResolveInputs::new(&req, &definition, &modules, &ctx.config, &ctx.storage, &ctx.cwd);
    let out = match resolve(inputs) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("katachi harness gemini: {e}");
            return Ok(ExitCode::Resolve);
        }
    };

    // Emit resolve diagnostics early.
    let validator_diags = run_validators(
        &ValidateContext {
            resolved: &out.resolved,
            catalog: &out.catalog,
            definition: &definition,
        },
        &default_validators(),
    );

    let plan_ctx = PlanContext {
        request: &req,
        resolved: &out.resolved,
        run_id: RunId::new(),
    };
    let plan = match harness.plan(&plan_ctx) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("katachi harness gemini: {e}");
            return Ok(ExitCode::Plan);
        }
    };

    let report = PlanReport {
        roster: &definition,
        resolved: &out.resolved,
        plan: &plan,
        validator_diagnostics: validator_diags.as_slice(),
    };
    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        render_plan_human(&report);
    }

    if any_error(&out.resolved.diagnostics) {
        return Ok(ExitCode::Resolve);
    }
    if any_error(report.validator_diagnostics) {
        return Ok(ExitCode::Validate);
    }
    Ok(ExitCode::Ok)
}

#[derive(Serialize)]
struct PlanReport<'a> {
    #[serde(skip)]
    roster: &'a KatachiDefinition,
    resolved: &'a katachi_core::plan::ResolvedKatachi,
    plan: &'a katachi_core::plan::ExecutionPlan,
    validator_diagnostics: &'a [Diagnostic],
}

fn render_plan_human(report: &PlanReport<'_>) {
    println!("roster      : {}", report.roster.id);
    println!("harness     : {}", report.resolved.harness);
    println!("backend     : {}", report.resolved.backend);
    println!("summary     : {}", report.plan.summary);
    println!(
        "schema      : plan v{}",
        PLAN_SCHEMA_VERSION
    );
    println!();
    println!("argv:");
    for arg in &report.plan.execution.argv {
        println!("  {arg}");
    }
    if !report.plan.execution.env.is_empty() {
        println!();
        println!("env:");
        for (k, v) in &report.plan.execution.env {
            println!("  {k}={v}");
        }
    }
    if !report.plan.materialization.files.is_empty() {
        println!();
        println!("materialized files: {}", report.plan.materialization.files.len());
    }
    let all_diags: Vec<&Diagnostic> = report
        .resolved
        .diagnostics
        .iter()
        .chain(report.validator_diagnostics.iter())
        .collect();
    if !all_diags.is_empty() {
        println!();
        println!("diagnostics:");
        for d in all_diags {
            println!("  [{}] {}: {}", severity_tag(d.severity), d.code, d.message);
        }
    }
}

// ---------------- execute ----------------

pub fn run_execute(global: &GlobalArgs, roster_id: &str, prompt: &str) -> Result<ExitCode> {
    // Same resolution/planning as run_plan, but also runs the plan.
    let ctx = load_ctx(global)?;
    let harness = GeminiHarness::new();
    let roster_store = load_rosters(&ctx)?;
    let roster = match roster_store.find(roster_id) {
        Some(r) => r.clone(),
        None => {
            eprintln!(
                "katachi harness gemini: roster `{}` not found under {}",
                roster_id,
                rosters_dir(&ctx).display()
            );
            return Ok(ExitCode::Resolve);
        }
    };
    let definition = roster.to_katachi_definition();
    let req = build_request_for_roster(global, roster_id, ctx.cwd.clone(), prompt);

    let modules: Vec<&dyn HarnessModule> = vec![&harness];
    let inputs = ResolveInputs::new(&req, &definition, &modules, &ctx.config, &ctx.storage, &ctx.cwd);
    let out = match resolve(inputs) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("katachi harness gemini: {e}");
            return Ok(ExitCode::Resolve);
        }
    };

    let plan_ctx = PlanContext {
        request: &req,
        resolved: &out.resolved,
        run_id: RunId::new(),
    };
    let plan = match harness.plan(&plan_ctx) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("katachi harness gemini: {e}");
            return Ok(ExitCode::Plan);
        }
    };

    let runs_root_utf8 = ctx.storage.runs_dir();
    if let Err(e) = std::fs::create_dir_all(runs_root_utf8.as_std_path()) {
        eprintln!("katachi harness gemini: failed to create runs directory: {e}");
        return Ok(ExitCode::Config);
    }
    let run_dir =
        match katachi_core::persist::RunDirectory::create(&runs_root_utf8, plan.run_id) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("katachi harness gemini: {e}");
                return Ok(ExitCode::Execute);
            }
        };
    if let Err(e) = run_dir.write_request(&req) {
        eprintln!("katachi harness gemini: {e}");
        return Ok(ExitCode::Execute);
    }
    if let Err(e) = run_dir.write_plan(&plan) {
        eprintln!("katachi harness gemini: {e}");
        return Ok(ExitCode::Execute);
    }

    let exec_ctx = katachi_core::harness::ExecuteContext {
        request: &req,
        plan: &plan,
        run_dir: &run_dir,
        started_at: time::OffsetDateTime::now_utc(),
    };

    let record = match harness.execute(&exec_ctx) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("katachi harness gemini: execute failed: {e}");
            let _ = run_dir.write_manifest();
            return Ok(ExitCode::Execute);
        }
    };
    let _ = run_dir.write_manifest();
    let committed = match run_dir.commit() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("katachi harness gemini: failed to commit run: {e}");
            return Ok(ExitCode::Execute);
        }
    };
    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &record)?;
        println!();
    } else {
        println!("run complete");
        println!("  run_id : {}", record.run_id);
        println!("  dir    : {}", committed);
        println!("  outcome: {:?}", record.result.outcome);
        if let Some(code) = record.result.exit_code {
            println!("  exit   : {code}");
        }
    }
    match record.result.outcome {
        katachi_core::record::Outcome::Success | katachi_core::record::Outcome::Planned => {
            Ok(ExitCode::Ok)
        }
        _ => Ok(ExitCode::Execute),
    }
}

fn load_rosters(ctx: &Ctx) -> Result<GeminiRosterStore> {
    let dir = Utf8PathBuf::from_path_buf(rosters_dir(ctx)).map_err(|p| {
        anyhow!(
            "rosters directory `{}` is not valid UTF-8",
            p.display()
        )
    })?;
    GeminiRosterStore::load_dir(&dir).map_err(|e| anyhow!("{e}"))
}

fn rosters_dir(ctx: &Ctx) -> std::path::PathBuf {
    ctx.storage
        .rosters_dir()
        .as_std_path()
        .to_path_buf()
        .join("gemini")
}

fn build_request_for_roster(
    global: &GlobalArgs,
    roster_id: &str,
    cwd: Utf8PathBuf,
    prompt: &str,
) -> InvocationRequest {
    let mut req = InvocationRequest::new(
        roster_id,
        ActionRequest::Execute {
            prompt: prompt.to_owned(),
        },
        cwd,
    );
    req.preferred_harnesses = vec![HarnessKind::Gemini];
    req.preferred_backends = global
        .prefer_backend
        .iter()
        .filter_map(|s| BackendKind::from_str(s).ok())
        .collect();
    if let Some(m) = global.materialization {
        req.materialization = match m {
            MaterializationArg::Ambient => MaterializationMode::Ambient,
            MaterializationArg::TempOverlay => MaterializationMode::TempOverlay,
        };
    }
    req.dry_run = global.dry_run;
    req
}

// Keep `scan` reachable even when consumers only import this module.
#[allow(dead_code)]
pub(crate) fn _keep_alive() {
    let _ = scan::build_catalog;
    let _: fn() -> GeminiRoster = || unreachable!();
}
