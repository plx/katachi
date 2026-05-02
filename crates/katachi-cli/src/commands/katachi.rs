//! `katachi katachi list/show/validate` — admin commands for the
//! katachi-definitions store.

use anyhow::Result;
use serde::Serialize;

use katachi_core::config;
use katachi_core::diagnostic::any_error;
use katachi_core::katachi::{KatachiStore, KatachiStoreError};
use katachi_core::model::{BackendKind, HarnessKind};
use katachi_core::paths::{resolve_config_file, resolve_storage_paths, PathOverrides};
use katachi_core::plan::{ActionRequest, InvocationRequest};
use katachi_core::resolve::{resolve, ResolveInputs};
use katachi_core::validate::{default_validators, run_validators, ValidateContext};

use crate::cli::{GlobalArgs, KatachiAction, KatachiCmd};
use crate::exit::ExitCode;
use crate::harness_registry::HarnessRegistry;

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

    #[derive(Serialize)]
    struct Entry<'a> {
        id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<&'a str>,
        targets: usize,
        target_harnesses: Vec<HarnessKind>,
    }
    let entries: Vec<Entry<'_>> = store
        .all()
        .iter()
        .map(|d| Entry {
            id: &d.id,
            description: d.description.as_deref(),
            targets: d.targets.len(),
            target_harnesses: d.targets.iter().map(|t| t.harness).collect(),
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
        println!("{:<28} {:<8} {}", "id", "targets", "description");
        for e in &entries {
            println!(
                "{:<28} {:<8} {}",
                e.id,
                e.targets,
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
        serde_json::to_writer_pretty(std::io::stdout(), def)?;
        println!();
    } else {
        println!("id          : {}", def.id);
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
    let Some(def) = store.find(id) else {
        emit_resolve_error(global, &format!("no katachi with id `{id}`"));
        return Ok(ExitCode::Resolve);
    };
    let registry = HarnessRegistry::from_config(&load.config).with_fixtures_from_env();
    let modules = registry.as_refs();

    let std_cwd = std::env::current_dir()?;
    let cwd = camino::Utf8PathBuf::from_path_buf(std_cwd)
        .map_err(|p| anyhow::anyhow!("cwd `{}` is not valid UTF-8", p.display()))?;
    let mut request = InvocationRequest::new(id, ActionRequest::Describe, cwd.clone());
    request.preferred_backends = global
        .prefer_backend
        .iter()
        .filter_map(|b| b.parse::<BackendKind>().ok())
        .collect();

    let inputs = ResolveInputs::new(&request, def, &modules, &load.config, &storage, &cwd);
    let output = match resolve(inputs) {
        Ok(out) => out,
        Err(err) => {
            emit_resolve_error(global, &err.to_string());
            return Ok(ExitCode::Resolve);
        }
    };
    let validator_diags = run_validators(
        &ValidateContext {
            resolved: &output.resolved,
            catalog: &output.catalog,
            definition: def,
        },
        &default_validators(),
    );
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
