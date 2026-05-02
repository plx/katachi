//! Error types specific to the Claude harness module.
//!
//! Callers typically convert these into the shared [`katachi_core::error`]
//! variants before crossing the harness boundary, but the harness-local
//! types let operators see a precise failure mode inside the module.

use camino::Utf8PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClaudeDiscoveryError {
    #[error("failed to read `{path}`: {source}")]
    Io {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse frontmatter in `{path}`: {message}")]
    Frontmatter {
        path: Utf8PathBuf,
        message: String,
    },
    #[error("failed to parse `{path}` as JSON: {source}")]
    Json {
        path: Utf8PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("non-UTF-8 path encountered under `{path}`")]
    NonUtf8 { path: Utf8PathBuf },
}

#[derive(Debug, Error)]
pub enum ClaudeRosterError {
    #[error("failed to read roster `{path}`: {source}")]
    Io {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse roster `{path}`: {source}")]
    Parse {
        path: Utf8PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("unsupported roster schema version `{found}` in `{path}` (expected `{expected}`)")]
    UnsupportedVersion {
        path: Utf8PathBuf,
        found: u32,
        expected: u32,
    },
    #[error("roster `{path}` has empty id")]
    EmptyId { path: Utf8PathBuf },
    #[error("roster `{id}` not found in `{dir}`")]
    NotFound { id: String, dir: Utf8PathBuf },
    #[error("failed to read roster directory `{path}`: {source}")]
    ReadDir {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("duplicate claude roster id `{id}` in `{first}` and `{second}`")]
    DuplicateId {
        id: String,
        first: Utf8PathBuf,
        second: Utf8PathBuf,
    },
}

#[derive(Debug, Error)]
pub enum ClaudeResolveError {
    #[error(transparent)]
    Roster(#[from] ClaudeRosterError),
    #[error(transparent)]
    Discovery(#[from] ClaudeDiscoveryError),
    #[error("roster `{roster_id}` references unknown {kind} `{id}`")]
    UnknownSelection {
        roster_id: String,
        kind: String,
        id: String,
    },
}

#[derive(Debug, Error)]
pub enum ClaudeProjectionError {
    #[error("backend `{backend}` cannot faithfully project this roster: {reason}")]
    Unsupported { backend: String, reason: String },
    #[error("failed to materialize overlay at `{path}`: {source}")]
    Materialize {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },
}
