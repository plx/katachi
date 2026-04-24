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
use katachi_core::model::{BackendKind, ItemRef};
use katachi_core::paths::{resolve_config_file, resolve_storage_paths, PathOverrides};

use katachi_harness_claude::ClaudeHarness;

use crate::cli::{
    GlobalArgs, GraphFormat, HarnessAction, HarnessCmd, HarnessName, HarnessPlanAction,
};
use crate::exit::ExitCode;

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

fn run_claude(global: &GlobalArgs, action: &HarnessAction) -> Result<ExitCode> {
    let harness = ClaudeHarness::new();
    let overrides = PathOverrides {
        config_file: global.config.clone(),
        data_root: global.data_root.clone(),
        cache_root: global.cache_root.clone(),
    };
    let config_path = resolve_config_file(&overrides)?;
    let load = config::load(config_path)?;
    let storage = resolve_storage_paths(&overrides, &load.config.storage)?;
    let cwd = resolve_cwd(global)?;

    match action {
        HarnessAction::Scan => {
            let ctx = ScanContext {
                config: &load.config,
                paths: &storage,
                cwd: &cwd,
            };
            let catalog = harness.scan(&ctx).map_err(|err| anyhow!("{err:#}"))?;
            render_scan(global, &catalog)
        }
        HarnessAction::Explain { item_id } => {
            let ctx = ScanContext {
                config: &load.config,
                paths: &storage,
                cwd: &cwd,
            };
            let catalog = harness.scan(&ctx).map_err(|err| anyhow!("{err:#}"))?;
            let item_ref = resolve_item_ref(item_id, &catalog)?;
            let explain_ctx = ExplainContext {
                item: &item_ref,
                catalog: &catalog,
                cwd: &cwd,
            };
            match harness.explain(&explain_ctx) {
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
        HarnessAction::Graph { format } => {
            let ctx = ScanContext {
                config: &load.config,
                paths: &storage,
                cwd: &cwd,
            };
            let catalog = harness.scan(&ctx).map_err(|err| anyhow!("{err:#}"))?;
            render_graph(global, &catalog, *format);
            Ok(ExitCode::Ok)
        }
        HarnessAction::Plan { roster_id, what } => match what {
            HarnessPlanAction::Execute { prompt } => run_plan(
                global,
                &harness,
                &load.config,
                &storage,
                &cwd,
                roster_id,
                prompt,
            ),
        },
        HarnessAction::Execute { roster_id, prompt } => run_execute(
            global,
            &harness,
            &load.config,
            &storage,
            &cwd,
            roster_id,
            prompt,
        ),
    }
}

fn run_plan(
    _global: &GlobalArgs,
    _harness: &ClaudeHarness,
    _config: &config::KatachiConfig,
    _storage: &katachi_core::paths::StoragePaths,
    _cwd: &Utf8PathBuf,
    _roster_id: &str,
    _prompt: &str,
) -> Result<ExitCode> {
    // Planner wiring lands in Step 10.
    eprintln!("katachi: `harness claude plan` is not yet fully wired");
    Ok(ExitCode::NotImplemented)
}

fn run_execute(
    _global: &GlobalArgs,
    _harness: &ClaudeHarness,
    _config: &config::KatachiConfig,
    _storage: &katachi_core::paths::StoragePaths,
    _cwd: &Utf8PathBuf,
    _roster_id: &str,
    _prompt: &str,
) -> Result<ExitCode> {
    // Executor wiring lands in Step 11.
    eprintln!("katachi: `harness claude execute` is not yet fully wired");
    Ok(ExitCode::NotImplemented)
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
    // Short forms: `kind:id` or plain `id` (scanning all kinds).
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

fn render_scan(global: &GlobalArgs, catalog: &katachi_core::harness::RosterCatalog) -> Result<ExitCode> {
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

#[allow(dead_code)]
fn backend_or_default(kind: Option<BackendKind>) -> BackendKind {
    kind.unwrap_or(BackendKind::Cli)
}
