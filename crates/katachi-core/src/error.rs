//! Typed errors for the shared core. Each major phase of the lifecycle
//! (resolve, validate, plan, execute) has its own error so the CLI can
//! map them to stable exit codes.

use thiserror::Error;

use crate::model::ParseKindError;
use crate::persist::PersistError;

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("no katachi with id `{id}` is known")]
    UnknownKatachi { id: String },

    #[error("katachi `{id}` is ambiguous across harnesses: {harnesses:?}")]
    Ambiguous { id: String, harnesses: Vec<String> },

    #[error("no enabled harness can satisfy katachi `{id}`")]
    NoEnabledHarness { id: String },

    #[error("selector references unknown item `{item}`")]
    UnknownItem { item: String },

    #[error(transparent)]
    ParseKind(#[from] ParseKindError),

    #[error("graph cycle detected involving `{item}`")]
    Cycle { item: String },
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("validation failed ({error_count} errors)")]
    Failed { error_count: usize },
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("failed to build execution plan: {message}")]
    BuildFailed { message: String },

    #[error("backend `{backend}` cannot project this katachi: {reason}")]
    ProjectionLoss { backend: String, reason: String },
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("failed to spawn child process `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to read from child process: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },

    #[error("child process `{command}` exited with non-zero status `{status}`")]
    NonZeroExit { command: String, status: String },

    #[error("execution timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },

    #[error("failed to persist run: {0}")]
    Persist(#[from] PersistError),
}
