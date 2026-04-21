//! Shared child-process executor.
//!
//! Turns an [`ExecutionPlan`] into a running child process, streams its
//! stdout/stderr into the run directory's transcript and log files, and
//! returns an [`ExecutionRecord`] describing the outcome.
//!
//! This module is the single async boundary in `katachi-core`. It creates
//! its own current-thread tokio runtime when invoked synchronously so the
//! rest of the crate can stay sync.

use std::time::Duration;

use serde_json::Value;
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;
use tracing::debug;

use crate::error::ExecutionError;
use crate::harness::ExecuteContext;
use crate::plan::{ActionRequest, TranscriptMode};
use crate::record::{ExecutionRecord, FinalResult, Outcome, RECORD_SCHEMA_VERSION};
use crate::transcript::{EventKind, TranscriptBuilder};

enum StreamLine {
    Stdout(String),
    Stderr(String),
}

enum ExitSentinel {
    Exited(std::process::ExitStatus),
    TimedOut,
}

/// Run an execution plan synchronously on a private tokio runtime.
///
/// Callers that are already inside a tokio runtime should use
/// [`run_async`] directly.
pub fn run(ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| ExecutionError::Io { source })?;
    rt.block_on(run_async(ctx))
}

/// Run an execution plan as an async task in the current tokio runtime.
pub async fn run_async(ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
    let started_at = ctx.started_at;
    let plan = ctx.plan;
    let request = ctx.request;
    let exec = &plan.execution;
    let run_id = ctx.run_dir.run_id();

    let (argv0, args) = exec.argv.split_first().ok_or_else(|| ExecutionError::Spawn {
        command: "<empty>".into(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "argv is empty"),
    })?;

    // Open writers backed by the run directory.
    let mut transcript = ctx.run_dir.transcript_writer()?;
    let mut stdout_log = tokio::fs::File::from_std(ctx.run_dir.stdout_writer()?);
    let mut stderr_log = tokio::fs::File::from_std(ctx.run_dir.stderr_writer()?);

    // Build command.
    let mut cmd = TokioCommand::new(argv0);
    cmd.args(args);
    for (k, v) in &exec.env {
        cmd.env(k, v);
    }
    if let Some(cwd) = &exec.cwd {
        cmd.current_dir(cwd.as_std_path());
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.stdin(if exec.stdin_input.is_some() {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::null()
    });

    let mut child = cmd.spawn().map_err(|source| ExecutionError::Spawn {
        command: argv0.clone(),
        source,
    })?;

    // Feed stdin if provided.
    if let Some(input) = &exec.stdin_input {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input.as_bytes())
                .await
                .map_err(|source| ExecutionError::Io { source })?;
            stdin
                .shutdown()
                .await
                .map_err(|source| ExecutionError::Io { source })?;
        }
    }

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let (tx, mut rx) = mpsc::unbounded_channel::<StreamLine>();
    let tx_out = tx.clone();
    let tx_err = tx;

    let stdout_task = tokio::spawn(read_lines_to_channel(stdout, tx_out, true));
    let stderr_task = tokio::spawn(read_lines_to_channel(stderr, tx_err, false));

    let mut builder = TranscriptBuilder::new();

    // Emit a user_message event so transcripts fully represent the
    // interaction when the action carries a prompt.
    if let Some(prompt) = action_prompt(&request.action) {
        let ev = builder.push(EventKind::UserMessage { text: prompt.to_string() });
        transcript.append(&ev)?;
    }

    let mode = plan.transcript_mode;

    let wait_fut = async {
        if let Some(secs) = exec.timeout_secs {
            match tokio::time::timeout(Duration::from_secs(secs), child.wait()).await {
                Ok(res) => {
                    let status = res.map_err(|source| ExecutionError::Io { source })?;
                    Ok::<ExitSentinel, ExecutionError>(ExitSentinel::Exited(status))
                }
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    Ok(ExitSentinel::TimedOut)
                }
            }
        } else {
            let status = child
                .wait()
                .await
                .map_err(|source| ExecutionError::Io { source })?;
            Ok(ExitSentinel::Exited(status))
        }
    };

    let consume_fut = async {
        while let Some(line) = rx.recv().await {
            match line {
                StreamLine::Stdout(text) => {
                    write_line(&mut stdout_log, &text).await?;
                    let kind = stdout_kind(mode, text);
                    let ev = builder.push(kind);
                    transcript.append(&ev)?;
                }
                StreamLine::Stderr(text) => {
                    write_line(&mut stderr_log, &text).await?;
                    let ev = builder.push(EventKind::StderrText { text });
                    transcript.append(&ev)?;
                }
            }
        }
        Ok::<(), ExecutionError>(())
    };

    let (wait_res, consume_res) = tokio::join!(wait_fut, consume_fut);
    consume_res?;
    let sentinel = wait_res?;

    // Readers should already be done since the streams EOF'd on child exit.
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    stdout_log
        .flush()
        .await
        .map_err(|source| ExecutionError::Io { source })?;
    stderr_log
        .flush()
        .await
        .map_err(|source| ExecutionError::Io { source })?;

    let (outcome, exit_code, summary_text) = match &sentinel {
        ExitSentinel::Exited(status) => {
            let code = status.code();
            if status.success() {
                (Outcome::Success, code, "exited successfully".to_string())
            } else {
                let summary = match code {
                    Some(c) => format!("exited with code {c}"),
                    None => "exited without an exit code".to_string(),
                };
                (Outcome::Failure, code, summary)
            }
        }
        ExitSentinel::TimedOut => (
            Outcome::Timeout,
            None,
            format!("timed out after {}s", exec.timeout_secs.unwrap_or(0)),
        ),
    };

    let result_event = builder.push(EventKind::Result {
        summary: summary_text.clone(),
        outcome: outcome_str(outcome).to_string(),
    });
    transcript.append(&result_event)?;
    transcript.flush()?;

    let record = ExecutionRecord {
        schema_version: RECORD_SCHEMA_VERSION,
        run_id,
        started_at,
        finished_at: OffsetDateTime::now_utc(),
        request: request.clone(),
        plan: plan.clone(),
        events_count: builder.next_seq() as usize,
        result: FinalResult {
            outcome,
            exit_code,
            summary: Some(summary_text),
        },
    };

    ctx.run_dir.write_record(&record)?;

    debug!(run_id = %run_id, outcome = ?outcome, "execution complete");
    Ok(record)
}

async fn read_lines_to_channel<R>(stream: R, tx: mpsc::UnboundedSender<StreamLine>, is_stdout: bool)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let msg = if is_stdout {
            StreamLine::Stdout(line)
        } else {
            StreamLine::Stderr(line)
        };
        if tx.send(msg).is_err() {
            break;
        }
    }
}

async fn write_line(file: &mut tokio::fs::File, line: &str) -> Result<(), ExecutionError> {
    file.write_all(line.as_bytes())
        .await
        .map_err(|source| ExecutionError::Io { source })?;
    file.write_all(b"\n")
        .await
        .map_err(|source| ExecutionError::Io { source })?;
    Ok(())
}

fn stdout_kind(mode: TranscriptMode, text: String) -> EventKind {
    match mode {
        TranscriptMode::JsonStream => match serde_json::from_str::<Value>(&text) {
            Ok(payload) => EventKind::JsonEvent { payload },
            Err(_) => EventKind::StdoutText { text },
        },
        TranscriptMode::RawOnly => EventKind::StdoutText { text },
    }
}

fn action_prompt(action: &ActionRequest) -> Option<&str> {
    match action {
        ActionRequest::Execute { prompt } | ActionRequest::Plan { prompt } => Some(prompt.as_str()),
        _ => None,
    }
}

fn outcome_str(o: Outcome) -> &'static str {
    match o {
        Outcome::Success => "success",
        Outcome::Failure => "failure",
        Outcome::Timeout => "timeout",
        Outcome::Planned => "planned",
    }
}
