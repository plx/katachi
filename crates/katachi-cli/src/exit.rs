//! Process exit-code taxonomy for the katachi CLI.
//!
//! Keeping a fixed set of codes lets shell scripts distinguish failure
//! categories. `0` means success; non-zero codes partition by responsibility.

/// Exit codes returned by the `katachi` binary.
#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum ExitCode {
    Ok = 0,
    /// Invalid CLI usage (e.g. bad flag combination, missing arg).
    Usage = 2,
    /// Config file missing, unreadable, or invalid.
    Config = 3,
    /// Failed to resolve a katachi id or selectors to a concrete plan.
    Resolve = 4,
    /// Validation rejected the resolved plan.
    Validate = 5,
    /// Planning failed after resolution.
    Plan = 6,
    /// Execution itself failed.
    Execute = 7,
    /// Feature not yet implemented at the current phase.
    NotImplemented = 64,
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(c: ExitCode) -> Self {
        std::process::ExitCode::from(c as u8)
    }
}
