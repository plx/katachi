//! Tracing/logging setup.

use tracing_subscriber::{fmt, EnvFilter};

/// Initialize logging.
///
/// Verbosity precedence (highest wins):
///
/// - `KATACHI_LOG` env var (env-filter syntax, e.g. `katachi_core=debug`)
/// - `RUST_LOG` env var (same syntax, kept for convention)
/// - `verbose` count from the CLI: 0 => info, 1 => debug, 2+ => trace
///
/// `json` switches the formatter to structured JSON output on stderr.
pub fn init(verbose: u8, json: bool) {
    let filter = EnvFilter::try_from_env("KATACHI_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new(default_filter(verbose)));

    if json {
        fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .json()
            .init();
    } else {
        fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .compact()
            .init();
    }
}

fn default_filter(verbose: u8) -> String {
    match verbose {
        0 => "info".into(),
        1 => "debug".into(),
        _ => "trace".into(),
    }
}
