//! `katachi harness codex <subcommand>` — Codex harness operator commands.

use anyhow::{anyhow, Context, Result};
use camino::Utf8PathBuf;
use serde::Serialize;

use katachi_core::config;
use katachi_core::diagnostic::Diagnostic;
use katachi_core::harness::{ExplainContext, HarnessModule, ScanContext};
use katachi_core::model::ItemRef;
use katachi_core::paths::{resolve_config_file, resolve_storage_paths, PathOverrides};
use katachi_core::roster::RosterCatalog;

use katachi_harness_codex::CodexHarness;

use crate::cli::{GlobalArgs, HarnessAction, HarnessCmd};
use crate::exit::ExitCode;

pub fn dispatch(global: &GlobalArgs, cmd: &HarnessCmd) -> Result<ExitCode> {
    match &cmd.action {
        HarnessAction::Scan => run_scan(global),
        HarnessAction::Explain { item_id } => run_explain(global, item_id),
        HarnessAction::Graph { format } => run_graph(global, *format),
        HarnessAction::Plan { .. } | HarnessAction::Execute { .. } => {
            eprintln!("katachi: codex plan/execute not yet implemented");
            Ok(ExitCode::NotImplemented)
        }
    }
}

pub fn run_scan(global: &GlobalArgs) -> Result<ExitCode> {
    let (catalog, _storage) = scan(global)?;
    render_catalog(global, &catalog);
    Ok(ExitCode::Ok)
}

pub fn run_explain(global: &GlobalArgs, item_id: &str) -> Result<ExitCode> {
    let (catalog, _storage) = scan(global)?;
    let harness = CodexHarness::new();
    let item_ref = resolve_item_id(&catalog, item_id)?;
    let cwd = resolve_cwd(global)?;
    let ctx = ExplainContext {
        item: &item_ref,
        catalog: &catalog,
        cwd: &cwd,
    };
    match harness.explain(&ctx) {
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
    let (catalog, _storage) = scan(global)?;
    use crate::cli::GraphFormat;
    match format {
        GraphFormat::Text => render_graph_text(&catalog),
        GraphFormat::Json => {
            serde_json::to_writer_pretty(std::io::stdout(), &catalog)?;
            println!();
        }
        GraphFormat::Dot => render_graph_dot(&catalog),
    }
    Ok(ExitCode::Ok)
}

fn scan(global: &GlobalArgs) -> Result<(RosterCatalog, katachi_core::paths::StoragePaths)> {
    let overrides = PathOverrides {
        config_file: global.config.clone(),
        data_root: global.data_root.clone(),
        cache_root: global.cache_root.clone(),
    };
    let config_path = resolve_config_file(&overrides).context("resolving config path")?;
    let load = config::load(config_path).context("loading config")?;
    let storage = resolve_storage_paths(&overrides, &load.config.storage)
        .context("resolving storage paths")?;

    let cwd = resolve_cwd(global)?;
    let harness = CodexHarness::new();
    let catalog = harness
        .scan(&ScanContext {
            config: &load.config,
            paths: &storage,
            cwd: &cwd,
        })
        .map_err(|e| anyhow!("codex scan failed: {e}"))?;
    Ok((catalog, storage))
}

fn resolve_item_id(catalog: &RosterCatalog, item_id: &str) -> Result<ItemRef> {
    if let Ok(item_ref) = item_id.parse::<ItemRef>() {
        if catalog.contains(&item_ref) {
            return Ok(item_ref);
        }
    }
    // Try to match by (kind, id) when an unqualified id was given.
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

fn resolve_cwd(global: &GlobalArgs) -> Result<Utf8PathBuf> {
    if let Some(cwd) = &global.cwd {
        return Ok(cwd.clone());
    }
    let std_cwd = std::env::current_dir()?;
    Utf8PathBuf::from_path_buf(std_cwd)
        .map_err(|p| anyhow!("cwd `{}` is not valid UTF-8", p.display()))
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

