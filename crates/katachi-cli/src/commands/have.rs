//! `katachi have <id> describe` and `... graph` — resolve + validate +
//! optionally render the dependency subgraph for a named katachi.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Result};
use camino::Utf8PathBuf;
use serde::Serialize;

use katachi_core::config;
use katachi_core::diagnostic::{any_error, Diagnostic, Severity};
use katachi_core::error::ResolveError;
use katachi_core::harness::RosterCatalog;
use katachi_core::katachi::{KatachiDefinition, KatachiStore, KatachiStoreError};
use katachi_core::model::{BackendKind, HarnessKind, ItemRef, MaterializationMode};
use katachi_core::paths::{resolve_config_file, resolve_storage_paths, PathOverrides};
use katachi_core::plan::{
    ActionRequest, InvocationRequest, ResolvedItemRef, ResolvedKatachi, SelectionReason,
};
use katachi_core::resolve::{resolve, ResolveInputs, ResolveOutput};
use katachi_core::roster::EdgeKind;
use katachi_core::validate::{default_validators, run_validators, ValidateContext};

use crate::cli::{GlobalArgs, GraphFormat, HaveCmd, MaterializationArg};
use crate::exit::ExitCode;
use crate::harness_registry::HarnessRegistry;

pub fn run_describe(global: &GlobalArgs, have: &HaveCmd) -> Result<ExitCode> {
    let prepared = match prepare(global, have)? {
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

pub fn run_graph(global: &GlobalArgs, have: &HaveCmd, format: GraphFormat) -> Result<ExitCode> {
    let prepared = match prepare(global, have)? {
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

    let render_json =
        global.json || matches!(format, GraphFormat::Json);
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
    definition: KatachiDefinition,
    output: ResolveOutput,
    validator_diagnostics: Vec<Diagnostic>,
    has_resolver_errors: bool,
    has_validation_errors: bool,
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

fn prepare(global: &GlobalArgs, have: &HaveCmd) -> Result<Prepared> {
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

    let definition = match store.find(&have.id) {
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

    let registry = HarnessRegistry::from_config(&load.config).with_fixtures_from_env();
    let modules = registry.as_refs();

    let cwd = resolve_cwd(global)?;
    let request = build_request(global, &have.id, cwd.clone());

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
        definition,
        output,
        validator_diagnostics,
        has_resolver_errors,
        has_validation_errors,
    }))
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

fn build_request(global: &GlobalArgs, id: &str, cwd: Utf8PathBuf) -> InvocationRequest {
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
    let mut req = InvocationRequest::new(id, ActionRequest::Describe, cwd);
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

fn filter_catalog<'a>(
    catalog: &'a RosterCatalog,
    selected: &BTreeSet<ItemRef>,
) -> Subgraph<'a> {
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
    edges.sort_by(|a, b| {
        a.from
            .id
            .cmp(&b.from.id)
            .then(a.to.id.cmp(&b.to.id))
    });

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
