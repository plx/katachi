//! Gemini-specific execute path.
//!
//! Delegates to the shared [`katachi_core::execute`] runner, then
//! augments the transcript by re-parsing stream-json events with the
//! Gemini-aware adapter in [`crate::transcript`]. The augmentation
//! writes a second sidecar file (`transcript.gemini.jsonl`) so the
//! original raw JSONL captured by the shared runner is preserved
//! alongside the Gemini projection.

use std::fs;
use std::io::{BufRead, BufReader, Write};

use camino::Utf8PathBuf;

use katachi_core::error::ExecutionError;
use katachi_core::execute;
use katachi_core::harness::ExecuteContext;
use katachi_core::persist::{PersistError, FILE_TRANSCRIPT};
use katachi_core::plan::TranscriptMode;
use katachi_core::record::ExecutionRecord;
use katachi_core::transcript::{EventKind, TranscriptEvent};

use crate::transcript as gemini_transcript;

pub fn run(ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
    let normalizer: Box<dyn Fn(&str) -> EventKind + Send + Sync> = Box::new(|line| {
        let mut events = gemini_transcript::parse_line(line);
        if events.is_empty() {
            return EventKind::StdoutText {
                text: line.to_string(),
            };
        }
        events.remove(0)
    });
    let record = execute::run_with_normalizer(ctx, Some(normalizer.as_ref()))?;
    // Only produce the Gemini sidecar if the plan was configured for
    // JSON streaming in the first place. Raw-only runs have nothing
    // gemini-specific to project.
    if ctx.plan.transcript_mode == TranscriptMode::JsonStream {
        if let Err(e) = write_sidecar(ctx) {
            // Treat sidecar failure as non-fatal: log and move on.
            tracing::debug!(error = ?e, "failed to write gemini transcript sidecar");
        }
    }
    Ok(record)
}

const SIDECAR_NAME: &str = "transcript.gemini.jsonl";

fn write_sidecar(ctx: &ExecuteContext<'_>) -> Result<(), SidecarError> {
    let transcript_path: Utf8PathBuf = ctx.run_dir.partial_path().join(FILE_TRANSCRIPT);
    if !transcript_path.exists() {
        return Ok(());
    }
    let sidecar_path = ctx.run_dir.partial_path().join(SIDECAR_NAME);
    let file =
        fs::File::open(transcript_path.as_std_path()).map_err(|source| SidecarError::Open {
            path: transcript_path.clone(),
            source,
        })?;
    let out =
        fs::File::create(sidecar_path.as_std_path()).map_err(|source| SidecarError::Create {
            path: sidecar_path.clone(),
            source,
        })?;
    let mut writer = std::io::BufWriter::new(out);
    let reader = BufReader::new(file);
    let mut next_seq: u64 = 0;
    let mut last_ts = None;

    for line in reader.lines() {
        let line = line.map_err(|source| SidecarError::Read { source })?;
        let event: TranscriptEvent = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };
        last_ts = Some(event.ts);
        let payload = match &event.kind {
            EventKind::JsonEvent { payload } => payload.clone(),
            _ => {
                write_event(&mut writer, &event)?;
                next_seq = event.seq.max(next_seq).saturating_add(1);
                continue;
            }
        };
        for projected in gemini_transcript::project_event(&payload) {
            let ev = TranscriptEvent {
                seq: next_seq,
                ts: event.ts,
                kind: projected,
            };
            next_seq = next_seq.saturating_add(1);
            write_event(&mut writer, &ev)?;
        }
    }
    let _ = last_ts; // silence unused-warning on empty input
    writer.flush().map_err(|source| SidecarError::Write {
        path: sidecar_path,
        source,
    })?;
    Ok(())
}

fn write_event<W: Write>(writer: &mut W, event: &TranscriptEvent) -> Result<(), SidecarError> {
    serde_json::to_writer(&mut *writer, event)
        .map_err(|source| SidecarError::Serialize { source })?;
    writer
        .write_all(b"\n")
        .map_err(|source| SidecarError::Write {
            path: Utf8PathBuf::from(""),
            source,
        })?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum SidecarError {
    #[error("failed to open `{path}`: {source}")]
    Open {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create `{path}`: {source}")]
    Create {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read transcript line: {source}")]
    Read {
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write `{path}`: {source}")]
    Write {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize transcript event: {source}")]
    Serialize {
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Persist(#[from] PersistError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_name_is_stable() {
        assert_eq!(SIDECAR_NAME, "transcript.gemini.jsonl");
    }
}
