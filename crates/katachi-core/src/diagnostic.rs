//! Shared diagnostic type used by the resolver, validator, planner, and
//! anywhere else that needs to surface non-fatal or fatal notes about
//! a katachi invocation.

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// A single diagnostic.
///
/// `code` is a stable, machine-readable identifier (e.g. `resolve.missing-dep`).
/// Tests can assert on codes without depending on human-readable wording.
/// `pointer` is an optional JSON-pointer-style string indicating which part
/// of the input produced the diagnostic; `data` carries structured details.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
}

impl Diagnostic {
    pub fn info(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Info, code, message)
    }
    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, message)
    }
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, message)
    }

    fn new(severity: Severity, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity,
            code: code.into(),
            message: message.into(),
            pointer: None,
            data: serde_json::Value::Null,
        }
    }

    pub fn with_pointer(mut self, pointer: impl Into<String>) -> Self {
        self.pointer = Some(pointer.into());
        self
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = data;
        self
    }
}

/// Quick predicate: does this slice contain any `Error`-severity diagnostics?
pub fn any_error(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| d.severity == Severity::Error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_severity_and_fields() {
        let d = Diagnostic::warning("resolve.missing-dep", "missing item");
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.code, "resolve.missing-dep");
        assert!(d.pointer.is_none());
        assert_eq!(d.data, serde_json::Value::Null);
    }

    #[test]
    fn serialization_skips_empty_optional_fields() {
        let d = Diagnostic::info("doctor.no-config", "using defaults");
        let j = serde_json::to_value(&d).unwrap();
        assert!(j.get("pointer").is_none());
        assert!(j.get("data").is_none());
    }

    #[test]
    fn any_error_detection() {
        let ds = vec![
            Diagnostic::info("x", "y"),
            Diagnostic::warning("x", "y"),
        ];
        assert!(!any_error(&ds));
        let mut ds = ds;
        ds.push(Diagnostic::error("x", "y"));
        assert!(any_error(&ds));
    }
}
