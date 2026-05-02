//! `katachi run list/show/transcript` — read-only inspection of
//! persisted runs under `<data_root>/runs/`.

use std::fs;
use std::path::Path;

use anyhow::Result;
use camino::Utf8PathBuf;
use serde::Serialize;
use serde_json::Value;

use katachi_core::config;
use katachi_core::paths::{resolve_config_file, resolve_storage_paths, PathOverrides};
use katachi_core::persist::{
    FILE_MANIFEST, FILE_PLAN, FILE_RECORD, FILE_REQUEST, FILE_TRANSCRIPT, PARTIAL_SUFFIX,
};

use crate::cli::{GlobalArgs, RunAction, RunCmd};
use crate::exit::ExitCode;

pub fn dispatch(global: &GlobalArgs, cmd: RunCmd) -> Result<ExitCode> {
    match cmd.action {
        RunAction::List => run_list(global),
        RunAction::Show { run_id } => run_show(global, &run_id),
        RunAction::Transcript { run_id } => run_transcript(global, &run_id),
    }
}

#[derive(Serialize)]
struct RunSummary {
    run_id: String,
    state: &'static str,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diagnostics: Vec<String>,
}

fn run_list(global: &GlobalArgs) -> Result<ExitCode> {
    let runs_dir = match runs_dir(global)? {
        Some(p) => p,
        None => {
            emit_runs_list(global, &[]);
            return Ok(ExitCode::Ok);
        }
    };

    let entries = match fs::read_dir(&runs_dir) {
        Ok(it) => it,
        Err(_) => {
            emit_runs_list(global, &[]);
            return Ok(ExitCode::Ok);
        }
    };

    let mut summaries: Vec<RunSummary> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let (run_id_str, state) = if let Some(stem) = name.strip_suffix(PARTIAL_SUFFIX) {
            (stem.to_string(), "partial")
        } else {
            (name.to_string(), "committed")
        };
        let summary = build_summary(&path, run_id_str, state);
        summaries.push(summary);
    }
    // UUID v7 sorts time-ordered. Descending so the newest runs surface
    // first.
    summaries.sort_by(|a, b| b.run_id.cmp(&a.run_id));

    emit_runs_list(global, &summaries);
    Ok(ExitCode::Ok)
}

fn build_summary(path: &Path, run_id: String, state: &'static str) -> RunSummary {
    let mut summary = RunSummary {
        run_id,
        state,
        path: path.display().to_string(),
        harness: None,
        backend: None,
        outcome: None,
        started_at: None,
        finished_at: None,
        diagnostics: Vec::new(),
    };
    let record_path = path.join(FILE_RECORD);
    match fs::read_to_string(&record_path) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(v) => {
                summary.outcome = v
                    .pointer("/result/outcome")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned);
                summary.started_at =
                    v.pointer("/started_at").and_then(|s| s.as_str()).map(str::to_owned);
                summary.finished_at =
                    v.pointer("/finished_at").and_then(|s| s.as_str()).map(str::to_owned);
                summary.harness = v
                    .pointer("/plan/harness")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned);
                summary.backend = v
                    .pointer("/plan/backend")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned);
            }
            Err(err) => summary
                .diagnostics
                .push(format!("malformed record.json: {err}")),
        },
        Err(_) => {
            summary
                .diagnostics
                .push(format!("missing {FILE_RECORD}"));
        }
    }
    summary
}

fn emit_runs_list(global: &GlobalArgs, summaries: &[RunSummary]) {
    if global.json {
        let payload = serde_json::json!({ "runs": summaries });
        let _ = serde_json::to_writer_pretty(std::io::stdout(), &payload);
        println!();
    } else {
        if summaries.is_empty() {
            println!("(no runs)");
            return;
        }
        println!(
            "{:<40} {:<10} {:<8} {:<8} {:<8} {}",
            "run_id", "state", "harness", "backend", "outcome", "started_at"
        );
        for s in summaries {
            println!(
                "{:<40} {:<10} {:<8} {:<8} {:<8} {}",
                s.run_id,
                s.state,
                s.harness.as_deref().unwrap_or("-"),
                s.backend.as_deref().unwrap_or("-"),
                s.outcome.as_deref().unwrap_or("-"),
                s.started_at.as_deref().unwrap_or("-"),
            );
        }
    }
}

fn run_show(global: &GlobalArgs, run_id_arg: &str) -> Result<ExitCode> {
    let runs_dir = match runs_dir(global)? {
        Some(p) => p,
        None => {
            emit_run_not_found(global, run_id_arg);
            return Ok(ExitCode::Resolve);
        }
    };
    let id = run_id_arg
        .strip_suffix(PARTIAL_SUFFIX)
        .unwrap_or(run_id_arg)
        .to_string();
    let committed = runs_dir.join(&id);
    let partial = runs_dir.join(format!("{id}{PARTIAL_SUFFIX}"));

    let (state, path) = match (committed.is_dir(), partial.is_dir()) {
        (true, true) => {
            emit_inconsistent(global, &id, &committed, &partial);
            return Ok(ExitCode::Resolve);
        }
        (true, false) => ("committed", committed),
        (false, true) => ("partial", partial),
        (false, false) => {
            emit_run_not_found(global, &id);
            return Ok(ExitCode::Resolve);
        }
    };

    let request = read_optional_json(&path.join(FILE_REQUEST));
    let plan = read_optional_json(&path.join(FILE_PLAN));
    let record = read_optional_json(&path.join(FILE_RECORD));
    let manifest = read_optional_json(&path.join(FILE_MANIFEST));

    let payload = serde_json::json!({
        "run_id": id,
        "state": state,
        "path": path.display().to_string(),
        "request": request,
        "plan": plan,
        "record": record,
        "manifest": manifest,
    });
    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        println!("run_id: {id}");
        println!("state : {state}");
        println!("path  : {}", path.display());
        if let Some(record) = &record {
            if let Some(outcome) = record.pointer("/result/outcome").and_then(|v| v.as_str()) {
                println!("outcome: {outcome}");
            }
            if let Some(exit_code) =
                record.pointer("/result/exit_code").and_then(|v| v.as_i64())
            {
                println!("exit  : {exit_code}");
            }
        }
        if let Some(plan) = &plan {
            if let Some(s) = plan.pointer("/summary").and_then(|v| v.as_str()) {
                println!("plan  : {s}");
            }
        }
        if let Some(manifest) = &manifest {
            if let Some(files) = manifest.pointer("/files").and_then(|v| v.as_array()) {
                println!("files ({}):", files.len());
                for f in files {
                    if let Some(p) = f.get("path").and_then(|v| v.as_str()) {
                        println!("  - {p}");
                    }
                }
            }
        }
    }
    Ok(ExitCode::Ok)
}

fn run_transcript(global: &GlobalArgs, run_id_arg: &str) -> Result<ExitCode> {
    let runs_dir = match runs_dir(global)? {
        Some(p) => p,
        None => {
            emit_run_not_found(global, run_id_arg);
            return Ok(ExitCode::Resolve);
        }
    };
    let id = run_id_arg
        .strip_suffix(PARTIAL_SUFFIX)
        .unwrap_or(run_id_arg)
        .to_string();
    let committed = runs_dir.join(&id);
    let partial = runs_dir.join(format!("{id}{PARTIAL_SUFFIX}"));
    let path = if committed.is_dir() {
        committed
    } else if partial.is_dir() {
        partial
    } else {
        emit_run_not_found(global, &id);
        return Ok(ExitCode::Resolve);
    };
    let transcript = path.join(FILE_TRANSCRIPT);
    let raw = match fs::read_to_string(&transcript) {
        Ok(s) => s,
        Err(_) => {
            if global.json {
                let payload = serde_json::json!({
                    "error": {
                        "kind": "resolve",
                        "message": format!("transcript not found at `{}`", transcript.display()),
                    }
                });
                let _ = serde_json::to_writer_pretty(std::io::stdout(), &payload);
                println!();
            } else {
                eprintln!("katachi run transcript: no transcript at `{}`", transcript.display());
            }
            return Ok(ExitCode::Resolve);
        }
    };

    let mut events: Vec<Value> = Vec::new();
    let mut diagnostics: Vec<String> = Vec::new();
    for (lineno, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => events.push(v),
            Err(err) => diagnostics.push(format!("line {}: {}", lineno + 1, err)),
        }
    }

    if global.json {
        let payload = serde_json::json!({
            "run_id": id,
            "events": events,
            "diagnostics": diagnostics,
        });
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        for event in &events {
            let seq = event.get("seq").and_then(|v| v.as_i64()).unwrap_or(-1);
            let ts = event
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let kind = event
                .get("kind")
                .and_then(|k| {
                    k.as_object().and_then(|obj| obj.keys().next().cloned())
                })
                .unwrap_or_else(|| "unknown".into());
            println!("{seq:>4} {ts} {kind}");
        }
        if !diagnostics.is_empty() {
            println!();
            println!("diagnostics:");
            for d in &diagnostics {
                println!("  {d}");
            }
        }
    }
    Ok(ExitCode::Ok)
}

fn read_optional_json(path: &Path) -> Option<Value> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn runs_dir(global: &GlobalArgs) -> Result<Option<std::path::PathBuf>> {
    let overrides = PathOverrides {
        config_file: global.config.clone(),
        data_root: global.data_root.clone(),
        cache_root: global.cache_root.clone(),
    };
    let config_path = resolve_config_file(&overrides)?;
    let load = config::load(config_path)?;
    let storage = resolve_storage_paths(&overrides, &load.config.storage)?;
    let runs: Utf8PathBuf = storage.runs_dir();
    let std_path = runs.as_std_path().to_path_buf();
    if std_path.exists() {
        Ok(Some(std_path))
    } else {
        Ok(None)
    }
}

fn emit_run_not_found(global: &GlobalArgs, run_id: &str) {
    if global.json {
        let payload = serde_json::json!({
            "error": {
                "kind": "resolve",
                "message": format!("run `{run_id}` not found"),
            }
        });
        let _ = serde_json::to_writer_pretty(std::io::stdout(), &payload);
        println!();
    } else {
        eprintln!("katachi run: no run with id `{run_id}`");
    }
}

fn emit_inconsistent(global: &GlobalArgs, id: &str, committed: &Path, partial: &Path) {
    let msg = format!(
        "run `{id}` is inconsistent: both committed (`{}`) and partial (`{}`) exist",
        committed.display(),
        partial.display(),
    );
    if global.json {
        let payload = serde_json::json!({
            "error": {
                "kind": "resolve",
                "message": msg,
            }
        });
        let _ = serde_json::to_writer_pretty(std::io::stdout(), &payload);
        println!();
    } else {
        eprintln!("katachi run: {msg}");
    }
}
