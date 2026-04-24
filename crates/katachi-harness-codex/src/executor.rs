//! Codex CLI executor.
//!
//! Runs the `codex exec` command through the shared
//! [`katachi_core::execute::run`] helper. The executor itself is thin —
//! the planner has already produced a fully-populated
//! [`ExecutionBackendPlan`].

use katachi_core::error::ExecutionError;
use katachi_core::execute;
use katachi_core::harness::ExecuteContext;
use katachi_core::record::ExecutionRecord;

pub fn run(ctx: &ExecuteContext<'_>) -> Result<ExecutionRecord, ExecutionError> {
    execute::run(ctx)
}
