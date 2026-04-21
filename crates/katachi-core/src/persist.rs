//! Run persistence.
//!
//! A completed run is represented on disk as a directory under
//! `<data_root>/runs/<run-id>/` containing a small set of well-known files:
//!
//! | file              | contents                                      |
//! |-------------------|-----------------------------------------------|
//! | `request.json`    | [`InvocationRequest`]                         |
//! | `plan.json`       | [`ExecutionPlan`]                             |
//! | `record.json`     | [`ExecutionRecord`]                           |
//! | `transcript.jsonl`| line-delimited [`TranscriptEvent`]s           |
//! | `stdout.log`      | raw child-process stdout                      |
//! | `stderr.log`      | raw child-process stderr                      |
//! | `manifest.json`   | [`RunManifest`]: index of files present       |
//!
//! Writers stream into a sibling `<run-id>.partial/` directory while the
//! run is in flight. On success the directory is atomically renamed into
//! its final location via a single `rename` syscall. A partial directory
//! that is never committed is preserved on disk for post-mortem analysis.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::plan::{ExecutionPlan, InvocationRequest};
use crate::record::{ExecutionRecord, RunId};
use crate::transcript::TranscriptEvent;

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const PARTIAL_SUFFIX: &str = ".partial";

pub const FILE_REQUEST: &str = "request.json";
pub const FILE_PLAN: &str = "plan.json";
pub const FILE_RECORD: &str = "record.json";
pub const FILE_TRANSCRIPT: &str = "transcript.jsonl";
pub const FILE_STDOUT: &str = "stdout.log";
pub const FILE_STDERR: &str = "stderr.log";
pub const FILE_MANIFEST: &str = "manifest.json";

pub type PersistResult<T> = Result<T, PersistError>;

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("run directory `{path}` already exists")]
    AlreadyExists { path: Utf8PathBuf },

    #[error("failed to create `{path}`: {source}")]
    CreateDir {
        path: Utf8PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to open `{path}`: {source}")]
    Open {
        path: Utf8PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to write `{path}`: {source}")]
    Write {
        path: Utf8PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to serialize {what}: {source}")]
    Serialize {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to read directory `{path}`: {source}")]
    ReadDir {
        path: Utf8PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to rename `{from}` -> `{to}`: {source}")]
    Rename {
        from: Utf8PathBuf,
        to: Utf8PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("path `{0}` is not valid UTF-8")]
    NonUtf8(String),
}

/// A run-scoped directory. Writers stream into the partial path; `commit`
/// atomically renames it to the final `runs/<run-id>/` location.
#[derive(Debug)]
pub struct RunDirectory {
    run_id: RunId,
    partial_path: Utf8PathBuf,
    final_path: Utf8PathBuf,
}

impl RunDirectory {
    /// Create a fresh partial directory under `runs_root` for `run_id`.
    ///
    /// Fails with [`PersistError::AlreadyExists`] if either the partial or
    /// final path is already occupied — duplicate run ids are treated as
    /// programmer errors rather than silently overwritten.
    pub fn create(runs_root: &Utf8Path, run_id: RunId) -> PersistResult<Self> {
        let id_str = run_id.to_string();
        let partial_path = runs_root.join(format!("{id_str}{PARTIAL_SUFFIX}"));
        let final_path = runs_root.join(&id_str);

        if final_path.exists() {
            return Err(PersistError::AlreadyExists { path: final_path });
        }

        fs::create_dir_all(runs_root.as_std_path()).map_err(|source| PersistError::CreateDir {
            path: runs_root.to_owned(),
            source,
        })?;
        match fs::create_dir(partial_path.as_std_path()) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(PersistError::AlreadyExists { path: partial_path });
            }
            Err(source) => {
                return Err(PersistError::CreateDir {
                    path: partial_path,
                    source,
                });
            }
        }

        Ok(Self {
            run_id,
            partial_path,
            final_path,
        })
    }

    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    /// Current on-disk path (the partial directory).
    pub fn partial_path(&self) -> &Utf8Path {
        &self.partial_path
    }

    /// Where the directory will live after [`RunDirectory::commit`].
    pub fn final_path(&self) -> &Utf8Path {
        &self.final_path
    }

    pub fn write_request(&self, request: &InvocationRequest) -> PersistResult<Utf8PathBuf> {
        self.write_json_pretty(FILE_REQUEST, request, "request")
    }

    pub fn write_plan(&self, plan: &ExecutionPlan) -> PersistResult<Utf8PathBuf> {
        self.write_json_pretty(FILE_PLAN, plan, "plan")
    }

    pub fn write_record(&self, record: &ExecutionRecord) -> PersistResult<Utf8PathBuf> {
        self.write_json_pretty(FILE_RECORD, record, "record")
    }

    /// Open a buffered appender for `transcript.jsonl`. Callers must call
    /// [`TranscriptWriter::flush`] before committing.
    pub fn transcript_writer(&self) -> PersistResult<TranscriptWriter> {
        let path = self.partial_path.join(FILE_TRANSCRIPT);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_std_path())
            .map_err(|source| PersistError::Open {
                path: path.clone(),
                source,
            })?;
        Ok(TranscriptWriter {
            path,
            writer: BufWriter::new(file),
        })
    }

    pub fn stdout_writer(&self) -> PersistResult<File> {
        self.open_log(FILE_STDOUT)
    }

    pub fn stderr_writer(&self) -> PersistResult<File> {
        self.open_log(FILE_STDERR)
    }

    /// Scan the partial directory, classify each regular file, and write
    /// `manifest.json`. The manifest does not list itself.
    pub fn write_manifest(&self) -> PersistResult<RunManifest> {
        self.write_manifest_at(OffsetDateTime::now_utc())
    }

    /// Like [`RunDirectory::write_manifest`] but with an explicit timestamp
    /// (useful for deterministic snapshot tests).
    pub fn write_manifest_at(&self, created_at: OffsetDateTime) -> PersistResult<RunManifest> {
        let mut files: Vec<ManifestEntry> = Vec::new();
        let read = fs::read_dir(self.partial_path.as_std_path()).map_err(|source| {
            PersistError::ReadDir {
                path: self.partial_path.clone(),
                source,
            }
        })?;
        for entry in read {
            let entry = entry.map_err(|source| PersistError::ReadDir {
                path: self.partial_path.clone(),
                source,
            })?;
            let os_name = entry.file_name();
            let name = os_name
                .to_str()
                .ok_or_else(|| PersistError::NonUtf8(os_name.to_string_lossy().into_owned()))?
                .to_owned();
            if name == FILE_MANIFEST {
                continue;
            }
            let meta = entry.metadata().map_err(|source| PersistError::ReadDir {
                path: self.partial_path.join(&name),
                source,
            })?;
            if !meta.is_file() {
                continue;
            }
            files.push(ManifestEntry {
                kind: classify_file(&name),
                path: name,
                bytes: meta.len(),
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));

        let manifest = RunManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            run_id: self.run_id,
            created_at,
            files,
        };
        self.write_json_pretty(FILE_MANIFEST, &manifest, "manifest")?;
        Ok(manifest)
    }

    /// Atomically rename the partial directory to its final location.
    pub fn commit(self) -> PersistResult<Utf8PathBuf> {
        fs::rename(
            self.partial_path.as_std_path(),
            self.final_path.as_std_path(),
        )
        .map_err(|source| PersistError::Rename {
            from: self.partial_path.clone(),
            to: self.final_path.clone(),
            source,
        })?;
        Ok(self.final_path)
    }

    fn open_log(&self, name: &str) -> PersistResult<File> {
        let path = self.partial_path.join(name);
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_std_path())
            .map_err(|source| PersistError::Open { path, source })
    }

    fn write_json_pretty<T: Serialize>(
        &self,
        name: &str,
        value: &T,
        what: &'static str,
    ) -> PersistResult<Utf8PathBuf> {
        let path = self.partial_path.join(name);
        let mut bytes = serde_json::to_vec_pretty(value)
            .map_err(|source| PersistError::Serialize { what, source })?;
        bytes.push(b'\n');
        fs::write(path.as_std_path(), &bytes).map_err(|source| PersistError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }
}

/// A buffered JSONL appender for [`TranscriptEvent`]s.
pub struct TranscriptWriter {
    path: Utf8PathBuf,
    writer: BufWriter<File>,
}

impl TranscriptWriter {
    pub fn append(&mut self, event: &TranscriptEvent) -> PersistResult<()> {
        serde_json::to_writer(&mut self.writer, event).map_err(|source| {
            PersistError::Serialize {
                what: "transcript event",
                source,
            }
        })?;
        self.writer
            .write_all(b"\n")
            .map_err(|source| PersistError::Write {
                path: self.path.clone(),
                source,
            })?;
        Ok(())
    }

    pub fn flush(&mut self) -> PersistResult<()> {
        self.writer.flush().map_err(|source| PersistError::Write {
            path: self.path.clone(),
            source,
        })
    }

    pub fn path(&self) -> &Utf8Path {
        &self.path
    }
}

/// Index of files present in a run directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunManifest {
    #[serde(default = "default_manifest_schema")]
    pub schema_version: u32,
    pub run_id: RunId,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub files: Vec<ManifestEntry>,
}

fn default_manifest_schema() -> u32 {
    MANIFEST_SCHEMA_VERSION
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// File name relative to the run directory.
    pub path: String,
    pub kind: RunFileKind,
    pub bytes: u64,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFileKind {
    Request,
    Plan,
    Record,
    Transcript,
    Stdout,
    Stderr,
    Other,
}

fn classify_file(name: &str) -> RunFileKind {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BackendKind, HarnessKind};
    use crate::plan::{
        ActionRequest, ExecutionBackendPlan, ExecutionPlan, InvocationRequest, MaterializationPlan,
        TranscriptMode, PLAN_SCHEMA_VERSION,
    };
    use crate::record::{ExecutionRecord, FinalResult, Outcome, RECORD_SCHEMA_VERSION};
    use crate::transcript::{EventKind, TranscriptBuilder};
    use std::collections::BTreeMap;
    use std::io::Write;
    use tempfile::TempDir;

    fn sample_request() -> InvocationRequest {
        InvocationRequest::new(
            "demo",
            ActionRequest::Execute {
                prompt: "hi".into(),
            },
            Utf8PathBuf::from("/tmp"),
        )
    }

    fn sample_plan(run_id: RunId) -> ExecutionPlan {
        ExecutionPlan {
            schema_version: PLAN_SCHEMA_VERSION,
            run_id,
            summary: "echo hi".into(),
            harness: HarnessKind::Claude,
            backend: BackendKind::Cli,
            materialization: MaterializationPlan::ambient(),
            execution: ExecutionBackendPlan {
                backend: BackendKind::Cli,
                argv: vec!["echo".into(), "hi".into()],
                stdin_input: None,
                env: BTreeMap::new(),
                cwd: None,
                timeout_secs: None,
            },
            transcript_mode: TranscriptMode::default(),
        }
    }

    fn sample_record(
        run_id: RunId,
        request: InvocationRequest,
        plan: ExecutionPlan,
    ) -> ExecutionRecord {
        ExecutionRecord {
            schema_version: RECORD_SCHEMA_VERSION,
            run_id,
            started_at: OffsetDateTime::UNIX_EPOCH,
            finished_at: OffsetDateTime::UNIX_EPOCH,
            request,
            plan,
            events_count: 1,
            result: FinalResult {
                outcome: Outcome::Success,
                exit_code: Some(0),
                summary: Some("done".into()),
            },
        }
    }

    fn runs_root() -> (TempDir, Utf8PathBuf) {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        (td, root)
    }

    #[test]
    fn create_partial_directory_layout() {
        let (_td, root) = runs_root();
        let run_id = RunId::new();
        let dir = RunDirectory::create(&root, run_id).unwrap();

        assert!(dir.partial_path().exists());
        assert!(dir.partial_path().as_str().ends_with(PARTIAL_SUFFIX));
        assert_eq!(dir.final_path(), &root.join(run_id.to_string()));
        assert_eq!(dir.run_id(), run_id);
    }

    #[test]
    fn create_errors_if_final_exists() {
        let (_td, root) = runs_root();
        let run_id = RunId::new();
        fs::create_dir_all(root.join(run_id.to_string()).as_std_path()).unwrap();
        let err = RunDirectory::create(&root, run_id).unwrap_err();
        assert!(matches!(err, PersistError::AlreadyExists { .. }));
    }

    #[test]
    fn create_errors_if_partial_exists() {
        let (_td, root) = runs_root();
        let run_id = RunId::new();
        fs::create_dir_all(root.join(format!("{run_id}{PARTIAL_SUFFIX}")).as_std_path()).unwrap();
        let err = RunDirectory::create(&root, run_id).unwrap_err();
        assert!(matches!(err, PersistError::AlreadyExists { .. }));
    }

    #[test]
    fn write_request_plan_record_roundtrip() {
        let (_td, root) = runs_root();
        let run_id = RunId::new();
        let dir = RunDirectory::create(&root, run_id).unwrap();
        let req = sample_request();
        let plan = sample_plan(run_id);
        let rec = sample_record(run_id, req.clone(), plan.clone());

        dir.write_request(&req).unwrap();
        dir.write_plan(&plan).unwrap();
        dir.write_record(&rec).unwrap();

        let req_json = fs::read_to_string(dir.partial_path().join(FILE_REQUEST)).unwrap();
        assert!(
            req_json.contains('\n'),
            "JSON output should be pretty-printed"
        );
        assert!(
            req_json.ends_with('\n'),
            "JSON output should end in newline"
        );

        let parsed: InvocationRequest = serde_json::from_str(&req_json).unwrap();
        assert_eq!(parsed.katachi_id, req.katachi_id);

        let plan_back: ExecutionPlan =
            serde_json::from_str(&fs::read_to_string(dir.partial_path().join(FILE_PLAN)).unwrap())
                .unwrap();
        assert_eq!(plan_back.run_id, run_id);
        assert_eq!(plan_back.execution.argv, vec!["echo", "hi"]);

        let rec_back: ExecutionRecord = serde_json::from_str(
            &fs::read_to_string(dir.partial_path().join(FILE_RECORD)).unwrap(),
        )
        .unwrap();
        assert_eq!(rec_back.result.outcome, Outcome::Success);
    }

    #[test]
    fn transcript_writer_appends_jsonl() {
        let (_td, root) = runs_root();
        let dir = RunDirectory::create(&root, RunId::new()).unwrap();

        {
            let mut w = dir.transcript_writer().unwrap();
            let mut b = TranscriptBuilder::new();
            w.append(&b.push_at(
                OffsetDateTime::UNIX_EPOCH,
                EventKind::StdoutText { text: "one".into() },
            ))
            .unwrap();
            w.append(&b.push_at(
                OffsetDateTime::UNIX_EPOCH,
                EventKind::StderrText { text: "two".into() },
            ))
            .unwrap();
            w.flush().unwrap();
        }

        let content = fs::read_to_string(dir.partial_path().join(FILE_TRANSCRIPT)).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let _: TranscriptEvent = serde_json::from_str(line).unwrap();
        }
    }

    #[test]
    fn stdout_stderr_writers_append() {
        let (_td, root) = runs_root();
        let dir = RunDirectory::create(&root, RunId::new()).unwrap();

        {
            let mut out = dir.stdout_writer().unwrap();
            out.write_all(b"hello\n").unwrap();
        }
        {
            let mut err = dir.stderr_writer().unwrap();
            err.write_all(b"boom\n").unwrap();
        }

        assert_eq!(
            fs::read_to_string(dir.partial_path().join(FILE_STDOUT)).unwrap(),
            "hello\n"
        );
        assert_eq!(
            fs::read_to_string(dir.partial_path().join(FILE_STDERR)).unwrap(),
            "boom\n"
        );
    }

    #[test]
    fn manifest_lists_present_files_sorted_and_excludes_self() {
        let (_td, root) = runs_root();
        let run_id = RunId::new();
        let dir = RunDirectory::create(&root, run_id).unwrap();
        let req = sample_request();
        let plan = sample_plan(run_id);
        let rec = sample_record(run_id, req.clone(), plan.clone());

        dir.write_request(&req).unwrap();
        dir.write_plan(&plan).unwrap();
        dir.write_record(&rec).unwrap();
        {
            let mut w = dir.transcript_writer().unwrap();
            w.append(&TranscriptBuilder::new().push_at(
                OffsetDateTime::UNIX_EPOCH,
                EventKind::StdoutText { text: "hi".into() },
            ))
            .unwrap();
            w.flush().unwrap();
        }
        {
            let mut out = dir.stdout_writer().unwrap();
            out.write_all(b"hi\n").unwrap();
        }

        let manifest = dir.write_manifest_at(OffsetDateTime::UNIX_EPOCH).unwrap();
        let names: Vec<_> = manifest.files.iter().map(|e| e.path.clone()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "manifest entries should be sorted");
        for expected in &[
            FILE_REQUEST,
            FILE_PLAN,
            FILE_RECORD,
            FILE_TRANSCRIPT,
            FILE_STDOUT,
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "manifest missing {expected}: {names:?}"
            );
        }
        assert!(!names.iter().any(|n| n == FILE_MANIFEST));

        let on_disk: RunManifest = serde_json::from_str(
            &fs::read_to_string(dir.partial_path().join(FILE_MANIFEST)).unwrap(),
        )
        .unwrap();
        assert_eq!(on_disk.schema_version, MANIFEST_SCHEMA_VERSION);
        assert_eq!(on_disk.run_id, run_id);
        assert_eq!(on_disk.created_at, OffsetDateTime::UNIX_EPOCH);
    }

    #[test]
    fn commit_renames_partial_to_final() {
        let (_td, root) = runs_root();
        let run_id = RunId::new();
        let dir = RunDirectory::create(&root, run_id).unwrap();
        let partial = dir.partial_path().to_owned();
        let final_path = dir.final_path().to_owned();
        dir.write_request(&sample_request()).unwrap();

        let committed = dir.commit().unwrap();
        assert_eq!(committed, final_path);
        assert!(!partial.exists(), "partial should be gone after commit");
        assert!(final_path.join(FILE_REQUEST).exists());
    }

    #[test]
    fn partial_preserved_when_not_committed() {
        let (_td, root) = runs_root();
        let dir = RunDirectory::create(&root, RunId::new()).unwrap();
        let partial = dir.partial_path().to_owned();
        dir.write_request(&sample_request()).unwrap();
        drop(dir);
        assert!(
            partial.exists(),
            "partial dir should persist on drop for diagnosis"
        );
    }

    #[test]
    fn classify_file_recognizes_known_names() {
        assert_eq!(classify_file(FILE_REQUEST), RunFileKind::Request);
        assert_eq!(classify_file(FILE_PLAN), RunFileKind::Plan);
        assert_eq!(classify_file(FILE_RECORD), RunFileKind::Record);
        assert_eq!(classify_file(FILE_TRANSCRIPT), RunFileKind::Transcript);
        assert_eq!(classify_file(FILE_STDOUT), RunFileKind::Stdout);
        assert_eq!(classify_file(FILE_STDERR), RunFileKind::Stderr);
        assert_eq!(classify_file("random.blob"), RunFileKind::Other);
    }
}
