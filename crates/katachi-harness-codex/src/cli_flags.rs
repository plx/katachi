//! Codex CLI flag vocabulary.
//!
//! Centralizing every exact flag string here means that if the Codex CLI
//! renames a flag or introduces a new supported value, only this module
//! has to be updated. Downstream code composes flags via
//! [`CliFlags`] helpers instead of hard-coding strings.

/// Enumerates the Codex CLI flags this module knows how to emit.
pub mod flags {
    pub const EXEC: &str = "exec";
    pub const CD: &str = "--cd";
    pub const SKIP_GIT_REPO_CHECK: &str = "--skip-git-repo-check";
    pub const PROFILE: &str = "--profile";
    pub const MODEL: &str = "-m";
    pub const MODEL_LONG: &str = "--model";
    pub const APPROVAL_POLICY: &str = "--ask-for-approval";
    pub const SANDBOX: &str = "--sandbox";
    pub const CONFIG_OVERRIDE: &str = "-c";
    pub const OUTPUT_SCHEMA: &str = "--output-schema";
    pub const OUTPUT_LAST_MESSAGE: &str = "--output-last-message";
    pub const JSON: &str = "--json";
    pub const EXPERIMENTAL_JSON: &str = "--experimental-json";
    pub const FULL_AUTO: &str = "--full-auto";
    pub const DANGEROUSLY_BYPASS: &str = "--dangerously-bypass-approvals-and-sandbox";
    pub const INCLUDE_PLAN_TOOL: &str = "--include-plan-tool";
    pub const SEARCH: &str = "--search";
    pub const COLOR: &str = "--color";
    pub const IMAGE: &str = "--image";
    pub const OSS: &str = "--oss";
    pub const CONFIG_PROFILE_FLAG: &str = "--profile";
}

/// Translate a katachi approval policy string into the codex flag value.
/// Codex currently accepts `"untrusted"`, `"on-failure"`, `"on-request"`,
/// and `"never"`.
pub fn approval_policy(value: &str) -> Option<&'static str> {
    match value {
        "never" => Some("never"),
        "on-request" | "on_request" => Some("on-request"),
        "on-failure" | "on_failure" => Some("on-failure"),
        "untrusted" => Some("untrusted"),
        _ => None,
    }
}

/// Translate a katachi sandbox mode string into the codex flag value.
pub fn sandbox_mode(value: &str) -> Option<&'static str> {
    match value {
        "read-only" | "read_only" => Some("read-only"),
        "workspace-write" | "workspace_write" => Some("workspace-write"),
        "danger-full-access" | "danger_full_access" => Some("danger-full-access"),
        _ => None,
    }
}

/// Map the roster `output_mode` string onto a CLI flag. `"machine-readable"`
/// selects JSON; `"text"` leaves the CLI's default behaviour.
pub fn output_mode(value: &str) -> OutputMode {
    match value {
        "machine-readable" | "json" | "experimental-json" => OutputMode::ExperimentalJson,
        "text" | "raw" => OutputMode::Text,
        _ => OutputMode::Text,
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OutputMode {
    Text,
    ExperimentalJson,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_normalization_accepts_both_casings() {
        assert_eq!(approval_policy("never"), Some("never"));
        assert_eq!(approval_policy("on_request"), Some("on-request"));
        assert_eq!(approval_policy("bogus"), None);
    }

    #[test]
    fn sandbox_normalization() {
        assert_eq!(sandbox_mode("read-only"), Some("read-only"));
        assert_eq!(sandbox_mode("workspace_write"), Some("workspace-write"));
    }

    #[test]
    fn output_mode_maps_to_known_values() {
        assert_eq!(output_mode("machine-readable"), OutputMode::ExperimentalJson);
        assert_eq!(output_mode("text"), OutputMode::Text);
    }
}
