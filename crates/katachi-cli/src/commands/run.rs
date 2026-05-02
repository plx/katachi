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
    RunFileKind, FILE_MANIFEST, FILE_PLAN, FILE_RECORD, FILE_REQUEST, FILE_STDERR, FILE_STDOUT,
    FILE_TRANSCRIPT, MANIFEST_SCHEMA_VERSION, PARTIAL_SUFFIX,
};
use katachi_core::transcript::{EventKind, TranscriptEvent};

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
        Err(err) => {
            emit_run_config_error(
                global,
                &format!(
                    "failed to read runs directory `{}`: {err}",
                    runs_dir.display()
                ),
            );
            return Ok(ExitCode::Config);
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
                summary.started_at = v
                    .pointer("/started_at")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned);
                summary.finished_at = v
                    .pointer("/finished_at")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned);
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
            summary.diagnostics.push(format!("missing {FILE_RECORD}"));
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

    let mut diagnostics = Vec::new();
    let request = read_json_artifact(&path.join(FILE_REQUEST), FILE_REQUEST, &mut diagnostics);
    let plan = read_json_artifact(&path.join(FILE_PLAN), FILE_PLAN, &mut diagnostics);
    let record = read_json_artifact(&path.join(FILE_RECORD), FILE_RECORD, &mut diagnostics);
    let manifest_path = path.join(FILE_MANIFEST);
    let manifest = if manifest_path.exists() {
        read_json_artifact(&manifest_path, FILE_MANIFEST, &mut diagnostics)
    } else {
        diagnostics.push(format!(
            "missing {FILE_MANIFEST}; discovered artifacts from directory"
        ));
        discover_manifest(&path, &id, &mut diagnostics)
    };

    let payload = serde_json::json!({
        "run_id": id,
        "state": state,
        "path": path.display().to_string(),
        "request": request,
        "plan": plan,
        "record": record,
        "manifest": manifest,
        "diagnostics": diagnostics,
    });
    if global.json {
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        println!("run_id: {id}");
        println!("state : {state}");
        println!("path  : {}", path.display());
        if let Some(request) = &request {
            if let Some(katachi_id) = request.pointer("/katachi_id").and_then(|v| v.as_str()) {
                println!("request: katachi={katachi_id}");
            }
            if let Some(cwd) = request.pointer("/cwd").and_then(|v| v.as_str()) {
                println!("cwd   : {cwd}");
            }
        }
        if let Some(record) = &record {
            if let Some(outcome) = record.pointer("/result/outcome").and_then(|v| v.as_str()) {
                println!("outcome: {outcome}");
            }
            if let Some(exit_code) = record.pointer("/result/exit_code").and_then(|v| v.as_i64()) {
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
        if !diagnostics.is_empty() {
            println!("diagnostics:");
            for d in &diagnostics {
                println!("  {d}");
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
                eprintln!(
                    "katachi run transcript: no transcript at `{}`",
                    transcript.display()
                );
            }
            return Ok(ExitCode::Resolve);
        }
    };

    let mut events: Vec<TranscriptLine> = Vec::new();
    let mut diagnostics: Vec<String> = Vec::new();
    for (lineno, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TranscriptEvent>(line) {
            Ok(event) => events.push(TranscriptLine::from_typed(lineno, event)?),
            Err(typed_err) => match serde_json::from_str::<Value>(line) {
                Ok(value) => {
                    diagnostics.push(format!(
                        "line {}: forward-compatible event shape: {}",
                        lineno + 1,
                        typed_err
                    ));
                    events.push(TranscriptLine::from_value(lineno, value));
                }
                Err(err) => diagnostics.push(format!("line {}: {}", lineno + 1, err)),
            },
        }
    }
    events.sort_by(|a, b| {
        a.seq
            .unwrap_or(u64::MAX)
            .cmp(&b.seq.unwrap_or(u64::MAX))
            .then(a.lineno.cmp(&b.lineno))
    });

    if global.json {
        let event_values: Vec<Value> = events.iter().map(|e| e.value.clone()).collect();
        let payload = serde_json::json!({
            "run_id": id,
            "state": state,
            "path": path.display().to_string(),
            "events": event_values,
            "diagnostics": diagnostics,
        });
        serde_json::to_writer_pretty(std::io::stdout(), &payload)?;
        println!();
    } else {
        for event in &events {
            let seq = event
                .seq
                .map(|s| s.to_string())
                .unwrap_or_else(|| "-".into());
            println!(
                "{seq:>4} {} {} {}",
                event.ts.as_deref().unwrap_or("-"),
                event.kind,
                event.summary
            );
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

struct TranscriptLine {
    lineno: usize,
    seq: Option<u64>,
    ts: Option<String>,
    kind: String,
    summary: String,
    value: Value,
}

impl TranscriptLine {
    fn from_typed(lineno: usize, event: TranscriptEvent) -> Result<Self> {
        let value = serde_json::to_value(&event)?;
        let kind = value
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let ts = value.get("ts").and_then(Value::as_str).map(str::to_owned);
        let summary = summarize_event(&event.kind);
        Ok(Self {
            lineno,
            seq: Some(event.seq),
            ts,
            kind,
            summary,
            value,
        })
    }

    fn from_value(lineno: usize, value: Value) -> Self {
        let seq = value.get("seq").and_then(Value::as_u64);
        let ts = value
            .get("ts")
            .or_else(|| value.get("timestamp"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let kind = value
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("json_event")
            .to_owned();
        Self {
            lineno,
            seq,
            ts,
            kind,
            summary: "raw event".into(),
            value,
        }
    }
}

fn summarize_event(kind: &EventKind) -> String {
    match kind {
        EventKind::StdoutText { text } => clipped(text),
        EventKind::StderrText { text } => clipped(text),
        EventKind::JsonEvent { payload } => payload
            .get("type")
            .and_then(Value::as_str)
            .map(|ty| format!("type={ty}"))
            .unwrap_or_else(|| "json payload".into()),
        EventKind::ToolUse { name, .. } => format!("name={name}"),
        EventKind::ToolResult { name, is_error, .. } => {
            if *is_error {
                format!("name={name} error=true")
            } else {
                format!("name={name}")
            }
        }
        EventKind::AssistantMessage { text } => clipped(text),
        EventKind::UserMessage { text } => clipped(text),
        EventKind::Warning { code, message } => format!("{code}: {}", clipped(message)),
        EventKind::Result { summary, outcome } => format!("{outcome}: {}", clipped(summary)),
    }
}

fn clipped(text: &str) -> String {
    let normalized = text.replace(['\r', '\n'], " ");
    let mut chars = normalized.chars();
    let clipped: String = chars.by_ref().take(96).collect();
    if chars.next().is_some() {
        format!("{clipped}...")
    } else {
        clipped
    }
}

fn read_json_artifact(path: &Path, name: &str, diagnostics: &mut Vec<String>) -> Option<Value> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            diagnostics.push(format!("missing {name}"));
            return None;
        }
        Err(err) => {
            diagnostics.push(format!("failed to read {name}: {err}"));
            return None;
        }
    };
    match serde_json::from_str(&raw) {
        Ok(value) => Some(value),
        Err(err) => {
            diagnostics.push(format!("malformed {name}: {err}"));
            None
        }
    }
}

fn discover_manifest(path: &Path, run_id: &str, diagnostics: &mut Vec<String>) -> Option<Value> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) => {
            diagnostics.push(format!("failed to discover artifacts: {err}"));
            return None;
        }
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }
        let Some(name) = entry_path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name == FILE_MANIFEST {
            continue;
        }
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        files.push(serde_json::json!({
            "path": name,
            "kind": classify_artifact(name),
            "bytes": bytes,
        }));
    }
    files.sort_by(|a, b| {
        a.get("path")
            .and_then(Value::as_str)
            .cmp(&b.get("path").and_then(Value::as_str))
    });
    Some(serde_json::json!({
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "run_id": run_id,
        "files": files,
    }))
}

fn classify_artifact(name: &str) -> RunFileKind {
    match name {
        FILE_REQUEST => RunFileKind::Request,
        FILE_PLAN => RunFileKind::Plan,
        FILE_RECORD => RunFileKind::Record,
        FILE_TRANSCRIPT => RunFileKind::Transcript,
        FILE_STDOUT => RunFileKind::Stdout,
        FILE_STDERR => RunFileKind::Stderr,
        _ => RunFileKind::Other,
    }
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

fn emit_run_config_error(global: &GlobalArgs, msg: &str) {
    if global.json {
        let payload = serde_json::json!({
            "error": {
                "kind": "config",
                "message": msg,
            }
        });
        let _ = serde_json::to_writer_pretty(std::io::stdout(), &payload);
        println!();
    } else {
        eprintln!("katachi run: {msg}");
    }
}
