//! `katachi have <id> describe` — resolve + validate a named katachi.

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use camino::Utf8PathBuf;
use serde::Serialize;

use katachi_core::config;
use katachi_core::diagnostic::{any_error, Diagnostic, Severity};
use katachi_core::error::ResolveError;
use katachi_core::harness::HarnessModule;
use katachi_core::katachi::{KatachiStore, KatachiStoreError};
use katachi_core::model::{BackendKind, HarnessKind, MaterializationMode};
use katachi_core::paths::{resolve_config_file, resolve_storage_paths, PathOverrides};
use katachi_core::plan::{
    ActionRequest, InvocationRequest, ResolvedItemRef, ResolvedKatachi, SelectionReason,
};
use katachi_core::resolve::{resolve, ResolveInputs};
use katachi_core::validate::{default_validators, run_validators, ValidateContext};

use crate::cli::{GlobalArgs, HaveCmd, MaterializationArg};
use crate::exit::ExitCode;
use crate::fixtures;

pub fn run_describe(global: &GlobalArgs, have: &HaveCmd) -> Result<ExitCode> {
    let overrides = PathOverrides {
        config_file: global.config.clone(),
        data_root: global.data_root.clone(),
        cache_root: global.cache_root.clone(),
    };
    let config_path = resolve_config_file(&overrides)?;
    let load = config::load(config_path)?;
    let storage = resolve_storage_paths(&overrides, &load.config.storage)?;

    let katachis_dir = storage.katachis_dir();
    let store = match KatachiStore::load_from_dir(&katachis_dir) {
        Ok(s) => s,
        Err(KatachiStoreError::Missing { path }) => {
            return emit_resolve_error_message(
                global,
                &format!("katachis directory `{path}` does not exist"),
            );
        }
        Err(err) => {
            return emit_resolve_error_message(global, &format!("{err:#}"));
        }
    };

    let definition = match store.find(&have.id) {
        Some(d) => d.clone(),
        None => {
            return emit_resolve_error(
                global,
                &ResolveError::UnknownKatachi {
                    id: have.id.clone(),
                },
            );
        }
    };

    let fixture_modules = fixtures::load_from_env();
    let modules: Vec<&dyn HarnessModule> = fixture_modules.iter().map(|h| h.as_ref()).collect();

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
        Err(err) => return emit_resolve_error(global, &err),
    };

    let validator_diagnostics = run_validators(
        &ValidateContext {
            resolved: &output.resolved,
            catalog: &output.catalog,
            definition: &definition,
        },
        &default_validators(),
    );

    let has_validation_errors = any_error(&validator_diagnostics);

    let report = DescribeReport {
        resolved: &output.resolved,
        diagnostics: &validator_diagnostics,
        description: definition.description.as_deref(),
    };

    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        render_human(&report);
    }

    if has_validation_errors {
        Ok(ExitCode::Validate)
    } else {
        Ok(ExitCode::Ok)
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

fn emit_resolve_error(global: &GlobalArgs, err: &ResolveError) -> Result<ExitCode> {
    emit_resolve_error_message(global, &err.to_string())
}

fn emit_resolve_error_message(global: &GlobalArgs, msg: &str) -> Result<ExitCode> {
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
    Ok(ExitCode::Resolve)
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
