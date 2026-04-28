//! `katachi harness codex <subcommand>` — Codex harness operator commands.

use anyhow::{anyhow, Context, Result};
use camino::Utf8PathBuf;
use serde::Serialize;
use time::OffsetDateTime;

use katachi_core::config;
use katachi_core::diagnostic::{any_error, Diagnostic};
use katachi_core::harness::{ExecuteContext, ExplainContext, HarnessModule};
use katachi_core::model::{BackendKind, ItemRef, MaterializationMode};
use katachi_core::paths::{resolve_config_file, resolve_storage_paths, PathOverrides};
use katachi_core::persist::RunDirectory;
use katachi_core::plan::{ActionRequest, InvocationRequest};
use katachi_core::record::RunId;
use katachi_core::roster::RosterCatalog;

use katachi_harness_codex::discovery::{discover, resolve_project_roots, DiscoveryInputs};
use katachi_harness_codex::effective::{build_effective, BuildEffective};
use katachi_harness_codex::harness::CodexHarness;
use katachi_harness_codex::items::CodexItemKind;
use katachi_harness_codex::planner::{plan as codex_plan, CodexPlanInputs, PlannedCodexRun};
use katachi_harness_codex::projection::analyze as analyze_projection;
use katachi_harness_codex::roster_file::{load_rosters_dir, CodexRosterFile};
use katachi_harness_codex::CodexSettings;

use crate::cli::{GlobalArgs, HarnessAction, HarnessCmd, HarnessPlanAction};
use crate::exit::ExitCode;

pub fn dispatch(global: &GlobalArgs, cmd: &HarnessCmd) -> Result<ExitCode> {
    match &cmd.action {
        HarnessAction::Scan => run_scan(global),
        HarnessAction::Explain { item_id } => run_explain(global, item_id),
        HarnessAction::Graph { format } => run_graph(global, *format),
        HarnessAction::EffectiveConfig { roster_id } => run_effective_config(global, roster_id),
        HarnessAction::Doctor => run_doctor(global),
        HarnessAction::Plan { roster_id, what } => run_plan(global, roster_id, what),
        HarnessAction::Execute { roster_id, prompt } => {
            run_execute(global, roster_id, prompt)
        }
    }
}

pub fn run_scan(global: &GlobalArgs) -> Result<ExitCode> {
    let ctx = prepare(global)?;
    render_catalog(global, &ctx.catalog);
    Ok(ExitCode::Ok)
}

pub fn run_explain(global: &GlobalArgs, item_id: &str) -> Result<ExitCode> {
    let ctx = prepare(global)?;
    let harness = CodexHarness::new();
    let item_ref = resolve_item_id(&ctx.catalog, item_id)?;
    let cwd = ctx.cwd.clone();
    let xctx = ExplainContext {
        item: &item_ref,
        catalog: &ctx.catalog,
        cwd: &cwd,
    };
    match harness.explain(&xctx) {
        Ok(result) => {
            if global.json {
                serde_json::to_writer_pretty(std::io::stdout(), &result)?;
                println!();
            } else {
                println!("{}", result.item);
                println!("summary: {}", result.summary);
                for section in &result.sections {
                    println!();
                    println!("== {}", section.title);
                    println!("{}", section.body);
                }
            }
            Ok(ExitCode::Ok)
        }
        Err(err) => {
            eprintln!("katachi harness codex explain: {err}");
            Ok(ExitCode::Resolve)
        }
    }
}

pub fn run_graph(
    global: &GlobalArgs,
    format: crate::cli::GraphFormat,
) -> Result<ExitCode> {
    let ctx = prepare(global)?;
    use crate::cli::GraphFormat;
    match format {
        GraphFormat::Text => render_graph_text(&ctx.catalog),
        GraphFormat::Json => {
            serde_json::to_writer_pretty(std::io::stdout(), &ctx.catalog)?;
            println!();
        }
        GraphFormat::Dot => render_graph_dot(&ctx.catalog),
    }
    Ok(ExitCode::Ok)
}

pub fn run_effective_config(global: &GlobalArgs, roster_id: &str) -> Result<ExitCode> {
    let bundle = build_bundle(global, roster_id)?;
    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &bundle.effective)?;
        println!();
    } else {
        bundle.print_summary();
    }
    Ok(ExitCode::Ok)
}

pub fn run_doctor(global: &GlobalArgs) -> Result<ExitCode> {
    let ctx = prepare(global)?;
    let settings = ctx.settings.clone();

    let mut report = DoctorReport::default();
    report.binary = settings.binary.clone();
    report.binary_on_path = which::which(&settings.binary).is_ok();
    report.codex_home = settings.codex_home.clone();
    report.codex_home_exists = settings.codex_home.exists();
    report.project_roots = resolve_project_roots(&settings.project_roots, &ctx.cwd);
    report.respect_project_trust = settings.respect_project_trust;
    report.enable_python_sdk = settings.enable_python_sdk;
    report.counts_by_kind = count_by_kind(&ctx.catalog);
    report.diagnostics = ctx.catalog.diagnostics.clone();

    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        report.render_human();
    }
    Ok(ExitCode::Ok)
}

pub fn run_plan(global: &GlobalArgs, roster_id: &str, what: &HarnessPlanAction) -> Result<ExitCode> {
    let HarnessPlanAction::Execute { prompt } = what;
    let bundle = build_bundle(global, roster_id)?;
    let planned = plan_with_bundle(&bundle, prompt.clone(), global)?;

    if global.json {
        let out = PlanReport::from(&planned);
        serde_json::to_writer_pretty(std::io::stdout(), &out)?;
        println!();
    } else {
        planned.render_plan();
    }

    if any_error(&bundle.validation) || any_error(&planned.projection) {
        Ok(ExitCode::Validate)
    } else {
        Ok(ExitCode::Ok)
    }
}

pub fn run_execute(global: &GlobalArgs, roster_id: &str, prompt: &str) -> Result<ExitCode> {
    let bundle = build_bundle(global, roster_id)?;
    let planned = plan_with_bundle(&bundle, prompt.to_string(), global)?;

    if any_error(&bundle.validation) {
        if global.json {
            serde_json::to_writer_pretty(
                std::io::stdout(),
                &serde_json::json!({
                    "error": "validation failed",
                    "diagnostics": &bundle.validation,
                }),
            )?;
            println!();
        } else {
            eprintln!("katachi harness codex execute: validation failed");
            for d in &bundle.validation {
                eprintln!("  [{:?}] {}: {}", d.severity, d.code, d.message);
            }
        }
        return Ok(ExitCode::Validate);
    }
    if any_error(&planned.projection) {
        if global.json {
            serde_json::to_writer_pretty(
                std::io::stdout(),
                &serde_json::json!({
                    "error": "projection failed",
                    "diagnostics": &planned.projection,
                }),
            )?;
            println!();
        } else {
            eprintln!("katachi harness codex execute: projection failed");
            for d in &planned.projection {
                eprintln!("  [{:?}] {}: {}", d.severity, d.code, d.message);
            }
        }
        return Ok(ExitCode::Plan);
    }

    if global.dry_run {
        if global.json {
            let out = PlanReport::from(&planned);
            serde_json::to_writer_pretty(std::io::stdout(), &out)?;
            println!();
        } else {
            println!("dry run: would execute");
            planned.render_plan();
        }
        return Ok(ExitCode::Ok);
    }

    // Build the ExecutionPlan via the shared envelope and run it.
    let execution_plan = planned.to_execution_plan(&bundle.roster)?;

    // Write the run to the runs/ directory.
    let runs_dir = bundle.storage.runs_dir();
    std::fs::create_dir_all(runs_dir.as_std_path())
        .with_context(|| format!("creating runs dir {runs_dir}"))?;
    let run_dir = RunDirectory::create(&runs_dir, execution_plan.run_id)?;
    let request = InvocationRequest {
        action: ActionRequest::Execute {
            prompt: prompt.to_string(),
        },
        ..bundle.build_request(&ctx_cwd(global)?, prompt.to_string())
    };
    run_dir.write_request(&request)?;
    run_dir.write_plan(&execution_plan)?;

    let harness = CodexHarness::new();
    let outcome = harness.execute(&ExecuteContext {
        request: &request,
        plan: &execution_plan,
        run_dir: &run_dir,
        started_at: OffsetDateTime::now_utc(),
    });

    // Always try to finalize the run directory, even when execution failed,
    // so partial transcripts and records are preserved. Surface any
    // finalization failure: downstream tooling treats a missing manifest or
    // un-committed run dir as a lost run.
    let manifest = match run_dir.write_manifest() {
        Ok(m) => Some(m),
        Err(err) => {
            eprintln!("katachi harness codex execute: writing run manifest: {err}");
            None
        }
    };
    let final_path = match run_dir.commit() {
        Ok(p) => Some(p),
        Err(err) => {
            eprintln!("katachi harness codex execute: committing run directory: {err}");
            None
        }
    };
    let persist_failed = manifest.is_none() || final_path.is_none();

    match outcome {
        Ok(record) => {
            if global.json {
                serde_json::to_writer_pretty(std::io::stdout(), &record)?;
                println!();
            } else {
                println!(
                    "run {} finished with outcome={:?}",
                    record.run_id, record.result.outcome
                );
                if let Some(path) = &final_path {
                    println!("recorded at {}", path);
                }
                if manifest.is_some() {
                    println!("manifest written");
                }
            }
            if persist_failed {
                return Ok(ExitCode::Execute);
            }
            Ok(match record.result.outcome {
                katachi_core::record::Outcome::Success => ExitCode::Ok,
                katachi_core::record::Outcome::Planned => ExitCode::Ok,
                katachi_core::record::Outcome::Failure => ExitCode::Execute,
                katachi_core::record::Outcome::Timeout => ExitCode::Execute,
            })
        }
        Err(err) => {
            eprintln!("katachi harness codex execute: {err}");
            Ok(ExitCode::Execute)
        }
    }
}

struct PreparedCtx {
    cwd: Utf8PathBuf,
    catalog: RosterCatalog,
    settings: CodexSettings,
    storage: katachi_core::paths::StoragePaths,
}

fn prepare(global: &GlobalArgs) -> Result<PreparedCtx> {
    let overrides = PathOverrides {
        config_file: global.config.clone(),
        data_root: global.data_root.clone(),
        cache_root: global.cache_root.clone(),
    };
    let config_path = resolve_config_file(&overrides).context("resolving config path")?;
    let load = config::load(config_path).context("loading config")?;
    let storage = resolve_storage_paths(&overrides, &load.config.storage)
        .context("resolving storage paths")?;

    let cwd = ctx_cwd(global)?;
    let settings = CodexSettings::load(&load.config);
    let inputs = DiscoveryInputs {
        settings: settings.clone(),
        cwd: cwd.clone(),
    };
    let catalog = discover(&inputs).map_err(|e| anyhow!("codex scan failed: {e}"))?;
    Ok(PreparedCtx {
        cwd,
        catalog,
        settings,
        storage,
    })
}

fn ctx_cwd(global: &GlobalArgs) -> Result<Utf8PathBuf> {
    if let Some(cwd) = &global.cwd {
        return Ok(cwd.clone());
    }
    let std_cwd = std::env::current_dir()?;
    Utf8PathBuf::from_path_buf(std_cwd)
        .map_err(|p| anyhow!("cwd `{}` is not valid UTF-8", p.display()))
}

fn resolve_item_id(catalog: &RosterCatalog, item_id: &str) -> Result<ItemRef> {
    if let Ok(item_ref) = item_id.parse::<ItemRef>() {
        if catalog.contains(&item_ref) {
            return Ok(item_ref);
        }
    }
    let matches: Vec<_> = catalog
        .iter_items()
        .filter(|(item_ref, _)| item_ref.id == item_id || full_id_matches(item_ref, item_id))
        .map(|(item_ref, _)| item_ref.clone())
        .collect();
    match matches.len() {
        0 => Err(anyhow!("no codex item matches `{item_id}`")),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => Err(anyhow!(
            "{n} codex items match `{item_id}`; qualify with `<kind>:<id>`"
        )),
    }
}

fn full_id_matches(item: &ItemRef, needle: &str) -> bool {
    if let Some((kind, id)) = needle.split_once(':') {
        item.kind == kind && item.id == id
    } else {
        false
    }
}

fn render_catalog(global: &GlobalArgs, catalog: &RosterCatalog) {
    if global.json {
        if serde_json::to_writer_pretty(std::io::stdout(), catalog).is_ok() {
            println!();
        }
        return;
    }
    let summary = ScanSummary::from_catalog(catalog);
    summary.render();
}

fn render_graph_text(catalog: &RosterCatalog) {
    for (item_ref, _) in catalog.iter_items() {
        println!("{}", item_ref);
    }
    println!();
    for edge in catalog.iter_edges() {
        let note = edge.note.as_deref().unwrap_or("");
        println!(
            "{}  --({note})-->  {}  [{}]",
            edge.from,
            edge.to,
            match edge.kind {
                katachi_core::roster::EdgeKind::Packaging => "packaging",
                katachi_core::roster::EdgeKind::Semantic => "semantic",
                katachi_core::roster::EdgeKind::Projection => "projection",
            }
        );
    }
}

fn render_graph_dot(catalog: &RosterCatalog) {
    println!("digraph codex {{");
    for (item_ref, item) in catalog.iter_items() {
        println!(
            "  \"{}\" [label=\"{}\\n{}\"];",
            item_ref, item_ref.kind, item.display_name
        );
    }
    for edge in catalog.iter_edges() {
        let note = edge.note.as_deref().unwrap_or("");
        println!(
            "  \"{}\" -> \"{}\" [label=\"{}\"];",
            edge.from, edge.to, note
        );
    }
    println!("}}");
}

fn count_by_kind(catalog: &RosterCatalog) -> std::collections::BTreeMap<String, usize> {
    let mut out: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (item_ref, _) in catalog.iter_items() {
        *out.entry(item_ref.kind.clone()).or_insert(0) += 1;
    }
    out
}

#[derive(Serialize)]
struct ScanSummary<'a> {
    items_by_kind: std::collections::BTreeMap<String, Vec<String>>,
    edge_count: usize,
    diagnostics: &'a [Diagnostic],
}

impl<'a> ScanSummary<'a> {
    fn from_catalog(catalog: &'a RosterCatalog) -> Self {
        let mut items_by_kind: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for (item_ref, _) in catalog.iter_items() {
            items_by_kind
                .entry(item_ref.kind.clone())
                .or_default()
                .push(item_ref.id.clone());
        }
        for ids in items_by_kind.values_mut() {
            ids.sort();
        }
        Self {
            items_by_kind,
            edge_count: catalog.edges.len(),
            diagnostics: &catalog.diagnostics,
        }
    }

    fn render(&self) {
        println!("katachi harness codex scan");
        println!();
        if self.items_by_kind.is_empty() {
            println!("(no codex artifacts discovered)");
        } else {
            for (kind, ids) in &self.items_by_kind {
                println!("[{kind}] ({})", ids.len());
                for id in ids {
                    println!("  - {id}");
                }
            }
        }
        println!();
        println!("edges: {}", self.edge_count);
        if !self.diagnostics.is_empty() {
            println!();
            println!("diagnostics:");
            for d in self.diagnostics {
                println!("  [{:?}] {}: {}", d.severity, d.code, d.message);
            }
        }
    }
}

struct BuiltBundle {
    roster: CodexRosterFile,
    effective: katachi_harness_codex::effective::EffectiveCodexConfig,
    settings: CodexSettings,
    storage: katachi_core::paths::StoragePaths,
    validation: Vec<Diagnostic>,
    resolved_items: Vec<ItemRef>,
}

impl BuiltBundle {
    fn print_summary(&self) {
        println!("katachi harness codex effective-config: {}", self.roster.id);
        println!();
        println!("layer order: {:?}", self.effective.layer_order);
        println!("active profile: {:?}", self.effective.active_profile);
        println!("policy:");
        println!(
            "  approval_policy: {:?}",
            self.effective.policy.approval_policy
        );
        println!("  sandbox_mode:    {:?}", self.effective.policy.sandbox_mode);
        println!("  model:           {:?}", self.effective.policy.model);
        println!("  profile:         {:?}", self.effective.policy.profile);
        println!(
            "  output_mode:     {:?}",
            self.effective.policy.output_mode
        );
        println!(
            "  output_schema:   {:?}",
            self.effective.policy.output_schema_file
        );
        println!("mcp servers:");
        for (name, _) in &self.effective.mcp_servers {
            println!("  - {name}");
        }
        println!("instruction chain ({}):", self.effective.instruction_chain.len());
        for doc in &self.effective.instruction_chain {
            println!("  {} {} ({})", doc.order, doc.id, doc.scope);
        }
        println!("hooks:    {}", self.effective.hooks.len());
        println!("rules:    {}", self.effective.rules.len());
        println!("skills:   {}", self.effective.skills.len());
        println!("agents:   {}", self.effective.agents.len());
        if !self.validation.is_empty() {
            println!();
            println!("validation:");
            for d in &self.validation {
                println!("  [{:?}] {}: {}", d.severity, d.code, d.message);
            }
        }
    }

    fn build_request(&self, cwd: &Utf8PathBuf, _prompt: String) -> InvocationRequest {
        let mut req = InvocationRequest::new(
            self.roster.id.clone(),
            ActionRequest::Describe, // overridden by caller
            cwd.clone(),
        );
        req.materialization = roster_materialization_mode(&self.roster);
        req
    }
}

fn roster_materialization_mode(roster: &CodexRosterFile) -> MaterializationMode {
    match roster.resolution.materialization.as_str() {
        "ambient" => MaterializationMode::Ambient,
        _ => MaterializationMode::TempOverlay,
    }
}

fn build_bundle(global: &GlobalArgs, roster_id: &str) -> Result<BuiltBundle> {
    let ctx = prepare(global)?;

    // 1. Load the roster file.
    let rosters_dir = ctx.storage.rosters_dir().join("codex");
    let rosters = load_rosters_dir(&rosters_dir)
        .with_context(|| format!("loading codex rosters from {rosters_dir}"))?;
    let roster = rosters
        .into_iter()
        .find(|r| r.id == roster_id)
        .ok_or_else(|| anyhow!("no codex roster with id `{roster_id}` in {rosters_dir}"))?;

    // 2. Expand roster selection to ItemRefs present in the catalog.
    //    Unknown selections produce validation diagnostics.
    let (resolved_items, mut selection_diags) = expand_selection(&ctx.catalog, &roster);

    // 3. Filter discovery outputs to the selected set (keeps effective
    //    config focused rather than merging everything).
    let layers: Vec<_> = collect_layers(&ctx, &roster, &resolved_items);
    let instructions: Vec<_> = collect_instructions(&ctx, &roster, &resolved_items);
    let hooks: Vec<_> = collect_hooks(&ctx, &roster, &resolved_items);
    let rules: Vec<_> = collect_rules(&ctx, &roster, &resolved_items);
    let mcps: Vec<_> = collect_mcps(&ctx, &roster, &resolved_items);
    let skills: Vec<_> = collect_skills(&ctx, &roster, &resolved_items);
    let agents: Vec<_> = collect_agents(&ctx, &roster, &resolved_items);

    let active_profile = roster
        .selection
        .profiles
        .first()
        .cloned()
        .or_else(|| roster.run_profile.profile.clone());
    let only_active = roster.resolution.respect_project_trust;

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
        only_active,
    });

    // 4. Run legality validators.
    let mut validation = Vec::new();
    validation.extend(katachi_harness_codex::legality::validate(
        &ctx.settings,
        &roster,
        &effective,
    ));
    validation.append(&mut selection_diags);

    Ok(BuiltBundle {
        roster,
        effective,
        settings: ctx.settings,
        storage: ctx.storage,
        validation,
        resolved_items,
    })
}

fn expand_selection(
    catalog: &RosterCatalog,
    roster: &CodexRosterFile,
) -> (Vec<ItemRef>, Vec<Diagnostic>) {
    let mut out = Vec::new();
    let mut diagnostics = Vec::new();
    for (kind, ids) in roster.selection.by_kind() {
        for id in &ids {
            let item_ref = ItemRef::new(
                katachi_core::model::HarnessKind::Codex,
                kind.as_str(),
                id.clone(),
            );
            if catalog.contains(&item_ref) {
                out.push(item_ref);
                continue;
            }
            // Allow bare ids to match any discovered id with the same kind.
            let matches: Vec<_> = catalog
                .iter_items()
                .filter(|(ir, _)| ir.kind == kind.as_str() && ir.id.ends_with(id))
                .map(|(ir, _)| ir.clone())
                .collect();
            if matches.is_empty() {
                diagnostics.push(Diagnostic::error(
                    "codex.selection.unknown-item",
                    format!("roster selects unknown codex {} `{}`", kind.as_str(), id),
                ));
                continue;
            }
            out.extend(matches);
        }
    }
    (out, diagnostics)
}

fn collect_layers(
    ctx: &PreparedCtx,
    roster: &CodexRosterFile,
    _resolved: &[ItemRef],
) -> Vec<katachi_harness_codex::config_layers::ConfigLayer> {
    // Simplest correct behaviour for v1: re-discover layers using the
    // same settings. We can't reconstruct `ConfigLayer` from the catalog
    // alone because the catalog strips the typed TOML body.
    let _ = (roster, _resolved);
    let mut diags = Vec::new();
    let roots = resolve_project_roots(&ctx.settings.project_roots, &ctx.cwd);
    katachi_harness_codex::config_layers::discover_config_layers(
        &ctx.settings,
        &roots,
        &ctx.cwd,
        &mut diags,
    )
}

fn collect_instructions(
    ctx: &PreparedCtx,
    _roster: &CodexRosterFile,
    _resolved: &[ItemRef],
) -> Vec<katachi_harness_codex::roster::InstructionDoc> {
    let mut diags = Vec::new();
    let roots = resolve_project_roots(&ctx.settings.project_roots, &ctx.cwd);
    katachi_harness_codex::roster::discover_instruction_chain(
        &ctx.settings,
        &roots,
        &ctx.cwd,
        &mut diags,
    )
}

fn collect_hooks(
    ctx: &PreparedCtx,
    _roster: &CodexRosterFile,
    _resolved: &[ItemRef],
) -> Vec<katachi_harness_codex::hooks::HookSet> {
    let mut diags = Vec::new();
    let layers = collect_layers(ctx, _roster, _resolved);
    katachi_harness_codex::hooks::discover_hooks(&layers, &mut diags)
}

fn collect_rules(
    ctx: &PreparedCtx,
    _roster: &CodexRosterFile,
    _resolved: &[ItemRef],
) -> Vec<katachi_harness_codex::rules::RuleSet> {
    let mut diags = Vec::new();
    let layers = collect_layers(ctx, _roster, _resolved);
    katachi_harness_codex::rules::discover_rules(&layers, &mut diags)
}

fn collect_mcps(
    ctx: &PreparedCtx,
    _roster: &CodexRosterFile,
    _resolved: &[ItemRef],
) -> Vec<katachi_harness_codex::mcp::McpServer> {
    let mut diags = Vec::new();
    let layers = collect_layers(ctx, _roster, _resolved);
    katachi_harness_codex::mcp::discover_mcp_servers(&layers, &mut diags)
}

fn collect_skills(
    ctx: &PreparedCtx,
    roster: &CodexRosterFile,
    _resolved: &[ItemRef],
) -> Vec<katachi_harness_codex::skills::Skill> {
    let mut diags = Vec::new();
    let roots = resolve_project_roots(&ctx.settings.project_roots, &ctx.cwd);
    let all = katachi_harness_codex::skills::discover_skills(&ctx.settings, &roots, &mut diags);
    if roster.selection.skills.is_empty() {
        all
    } else {
        let want: std::collections::HashSet<&String> = roster.selection.skills.iter().collect();
        all.into_iter()
            .filter(|s| want.contains(&s.id) || want.iter().any(|w| s.id.ends_with(w.as_str())))
            .collect()
    }
}

fn collect_agents(
    ctx: &PreparedCtx,
    roster: &CodexRosterFile,
    _resolved: &[ItemRef],
) -> Vec<katachi_harness_codex::agents::CustomAgent> {
    let mut diags = Vec::new();
    let roots = resolve_project_roots(&ctx.settings.project_roots, &ctx.cwd);
    let all = katachi_harness_codex::agents::discover_agents(&ctx.settings, &roots, &mut diags);
    if roster.selection.agents.is_empty() {
        all
    } else {
        let want: std::collections::HashSet<&String> = roster.selection.agents.iter().collect();
        all.into_iter()
            .filter(|a| want.contains(&a.id))
            .collect()
    }
}

struct PlannedWithProjection {
    planned: PlannedCodexRun,
    projection: Vec<Diagnostic>,
    run_id: RunId,
    bundle_snapshot: serde_json::Value,
}

impl PlannedWithProjection {
    fn render_plan(&self) {
        println!("backend:       {}", self.planned.backend);
        println!("command:       {}", self.planned.command.join(" "));
        println!("cwd:           {}", self.planned.cwd);
        println!("transcript:    {:?}", self.planned.transcript_mode);
        println!("materialization files: {}", self.planned.materialization.files.len());
        for (k, v) in &self.planned.env {
            println!("  env {k} = {v}");
        }
        if !self.projection.is_empty() {
            println!();
            println!("projection diagnostics:");
            for d in &self.projection {
                println!("  [{:?}] {}: {}", d.severity, d.code, d.message);
            }
        }
    }

    fn to_execution_plan(
        &self,
        roster: &CodexRosterFile,
    ) -> Result<katachi_core::plan::ExecutionPlan> {
        use katachi_core::plan::{
            ExecutionBackendPlan, ExecutionPlan, MaterializationPlan, PLAN_SCHEMA_VERSION,
        };
        Ok(ExecutionPlan {
            schema_version: PLAN_SCHEMA_VERSION,
            run_id: self.run_id,
            summary: format!("codex exec roster={}", roster.id),
            harness: katachi_core::model::HarnessKind::Codex,
            backend: self.planned.backend,
            materialization: MaterializationPlan {
                mode: roster_materialization_mode(roster),
                overlay_root: None,
                files: self.planned.materialization.files.clone(),
                env: self.planned.materialization.env.clone(),
            },
            execution: ExecutionBackendPlan {
                backend: self.planned.backend,
                argv: self.planned.command.clone(),
                stdin_input: None,
                env: self.planned.env.clone(),
                cwd: Some(self.planned.cwd.clone()),
                timeout_secs: roster.run_profile.timeout_secs,
            },
            transcript_mode: self.planned.transcript_mode,
        })
    }
}

fn plan_with_bundle(
    bundle: &BuiltBundle,
    prompt: String,
    global: &GlobalArgs,
) -> Result<PlannedWithProjection> {
    // Build a request-sized InvocationRequest to hand to the planner.
    let cwd = ctx_cwd(global)?;
    let mut req = bundle.build_request(&cwd, prompt.clone());
    req.action = ActionRequest::Execute { prompt };
    let resolved = katachi_core::plan::ResolvedKatachi {
        katachi_id: bundle.roster.id.clone(),
        harness: katachi_core::model::HarnessKind::Codex,
        backend: parse_backend(&bundle),
        selected_items: bundle
            .resolved_items
            .iter()
            .map(|i| katachi_core::plan::ResolvedItemRef {
                item: i.clone(),
                reason: katachi_core::plan::SelectionReason::Direct,
                pulled_in_by: None,
            })
            .collect(),
        run_profile: katachi_core::plan::RunProfile::default(),
        diagnostics: Vec::new(),
    };
    let run_id = RunId::new();
    let ctx = katachi_core::harness::PlanContext {
        request: &req,
        resolved: &resolved,
        run_id,
    };
    let inputs = CodexPlanInputs {
        ctx: &ctx,
        roster: &bundle.roster,
        effective: &bundle.effective,
        settings: &bundle.settings,
    };
    let planned = codex_plan(&inputs).with_context(|| "building codex plan")?;

    let projection = analyze_projection(
        planned.backend,
        &bundle.roster,
        &bundle.effective,
        &bundle.settings,
    );
    let bundle_snapshot = serde_json::json!({
        "roster_id": bundle.roster.id,
        "selected": bundle.resolved_items.iter().map(ToString::to_string).collect::<Vec<_>>(),
    });
    Ok(PlannedWithProjection {
        planned,
        projection,
        run_id,
        bundle_snapshot,
    })
}

fn parse_backend(bundle: &BuiltBundle) -> BackendKind {
    let raw = bundle
        .roster
        .run_profile
        .backend
        .as_deref()
        .unwrap_or(bundle.settings.default_backend.as_str());
    raw.parse().unwrap_or(BackendKind::Cli)
}

#[derive(Serialize, Default)]
struct DoctorReport {
    binary: String,
    binary_on_path: bool,
    codex_home: Utf8PathBuf,
    codex_home_exists: bool,
    project_roots: Vec<Utf8PathBuf>,
    respect_project_trust: bool,
    enable_python_sdk: bool,
    counts_by_kind: std::collections::BTreeMap<String, usize>,
    diagnostics: Vec<Diagnostic>,
}

impl DoctorReport {
    fn render_human(&self) {
        println!("katachi harness codex doctor");
        println!();
        println!("Binary:");
        println!("  name : {}", self.binary);
        println!(
            "  on PATH: {}",
            if self.binary_on_path { "yes" } else { "no" }
        );
        println!();
        println!("Codex home:");
        println!("  path  : {}", self.codex_home);
        println!(
            "  exists: {}",
            if self.codex_home_exists { "yes" } else { "no" }
        );
        println!();
        println!("Project roots:");
        for root in &self.project_roots {
            println!("  - {root}");
        }
        println!();
        println!("Flags:");
        println!(
            "  respect_project_trust: {}",
            self.respect_project_trust
        );
        println!("  enable_python_sdk:     {}", self.enable_python_sdk);
        println!();
        println!("Discovered:");
        for (k, n) in &self.counts_by_kind {
            println!("  {k}: {n}");
        }
        if !self.diagnostics.is_empty() {
            println!();
            println!("Discovery diagnostics:");
            for d in &self.diagnostics {
                println!("  [{:?}] {}: {}", d.severity, d.code, d.message);
            }
        }
    }
}

#[derive(Serialize)]
struct PlanReport<'a> {
    backend: BackendKind,
    argv: &'a [String],
    env: &'a std::collections::BTreeMap<String, String>,
    cwd: &'a Utf8PathBuf,
    transcript_mode: &'static str,
    materialization_files: usize,
    projection: &'a [Diagnostic],
    bundle: &'a serde_json::Value,
}

impl<'a> From<&'a PlannedWithProjection> for PlanReport<'a> {
    fn from(p: &'a PlannedWithProjection) -> Self {
        Self {
            backend: p.planned.backend,
            argv: &p.planned.command,
            env: &p.planned.env,
            cwd: &p.planned.cwd,
            transcript_mode: match p.planned.transcript_mode {
                katachi_core::plan::TranscriptMode::RawOnly => "raw_only",
                katachi_core::plan::TranscriptMode::JsonStream => "json_stream",
            },
            materialization_files: p.planned.materialization.files.len(),
            projection: &p.projection,
            bundle: &p.bundle_snapshot,
        }
    }
}

/// Internal: translate a CodexItemKind to its string form for selection
/// bookkeeping.
#[allow(dead_code)]
fn kind_as_str(k: CodexItemKind) -> &'static str {
    k.as_str()
}
