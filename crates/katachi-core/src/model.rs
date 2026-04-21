//! Core shared data types: harness/backend enums, materialization mode,
//! and the `ItemRef` used to identify roster items across harnesses.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The set of coding-agent harnesses katachi knows about.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum HarnessKind {
    Claude,
    Codex,
    Gemini,
}

impl HarnessKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
        }
    }

    pub const ALL: &'static [HarnessKind] = &[Self::Claude, Self::Codex, Self::Gemini];
}

impl fmt::Display for HarnessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HarnessKind {
    type Err = ParseKindError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "gemini" => Ok(Self::Gemini),
            other => Err(ParseKindError::UnknownHarness(other.to_owned())),
        }
    }
}

/// Concrete execution route for a harness.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    Cli,
    SdkTs,
    SdkPy,
    McpServer,
    AppServer,
}

impl BackendKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::SdkTs => "sdk-ts",
            Self::SdkPy => "sdk-py",
            Self::McpServer => "mcp-server",
            Self::AppServer => "app-server",
        }
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for BackendKind {
    type Err = ParseKindError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cli" => Ok(Self::Cli),
            "sdk-ts" => Ok(Self::SdkTs),
            "sdk-py" => Ok(Self::SdkPy),
            "mcp-server" => Ok(Self::McpServer),
            "app-server" => Ok(Self::AppServer),
            other => Err(ParseKindError::UnknownBackend(other.to_owned())),
        }
    }
}

/// How katachi constructs the environment a harness sees.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaterializationMode {
    /// Use the user's existing harness installation and discovery as-is.
    Ambient,
    /// Build an ephemeral overlay with only the selected items and configs.
    TempOverlay,
}

impl Default for MaterializationMode {
    fn default() -> Self {
        Self::TempOverlay
    }
}

/// Stable identifier for a roster item.
///
/// The `kind` string is opaque at the shared layer — each harness owns its
/// own enum and stringifies into this field (e.g. `"plugin"`, `"skill"`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct ItemRef {
    pub harness: HarnessKind,
    pub kind: String,
    pub id: String,
}

impl ItemRef {
    pub fn new(harness: HarnessKind, kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self { harness, kind: kind.into(), id: id.into() }
    }
}

/// `ItemRef` renders as `harness:kind:id`, matching its `FromStr` parser.
impl fmt::Display for ItemRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.harness, self.kind, self.id)
    }
}

impl FromStr for ItemRef {
    type Err = ParseKindError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // harness : kind : id  (id may itself contain colons)
        let (harness_s, rest) =
            s.split_once(':').ok_or_else(|| ParseKindError::BadItemRef(s.to_owned()))?;
        let (kind_s, id_s) =
            rest.split_once(':').ok_or_else(|| ParseKindError::BadItemRef(s.to_owned()))?;
        let harness = HarnessKind::from_str(harness_s)?;
        if kind_s.is_empty() || id_s.is_empty() {
            return Err(ParseKindError::BadItemRef(s.to_owned()));
        }
        Ok(ItemRef::new(harness, kind_s, id_s))
    }
}

#[derive(Debug, Error)]
pub enum ParseKindError {
    #[error("unknown harness `{0}` (expected claude, codex, or gemini)")]
    UnknownHarness(String),
    #[error("unknown backend `{0}`")]
    UnknownBackend(String),
    #[error("invalid item ref `{0}` (expected `harness:kind:id`)")]
    BadItemRef(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_kind_roundtrip() {
        for h in HarnessKind::ALL {
            let s = h.as_str();
            assert_eq!(HarnessKind::from_str(s).unwrap(), *h);
        }
        assert!(HarnessKind::from_str("nope").is_err());
    }

    #[test]
    fn backend_kind_roundtrip() {
        for b in [
            BackendKind::Cli,
            BackendKind::SdkTs,
            BackendKind::SdkPy,
            BackendKind::McpServer,
            BackendKind::AppServer,
        ] {
            assert_eq!(BackendKind::from_str(b.as_str()).unwrap(), b);
        }
    }

    #[test]
    fn item_ref_display_and_parse() {
        let r = ItemRef::new(HarnessKind::Claude, "skill", "axe-runner");
        assert_eq!(r.to_string(), "claude:skill:axe-runner");
        assert_eq!(ItemRef::from_str("claude:skill:axe-runner").unwrap(), r);
    }

    #[test]
    fn item_ref_id_may_contain_colons() {
        let r = ItemRef::from_str("codex:skill:namespace:foo").unwrap();
        assert_eq!(r.harness, HarnessKind::Codex);
        assert_eq!(r.kind, "skill");
        assert_eq!(r.id, "namespace:foo");
    }

    #[test]
    fn item_ref_rejects_bad_forms() {
        assert!(ItemRef::from_str("claude:skill").is_err());
        assert!(ItemRef::from_str("claude::id").is_err());
        assert!(ItemRef::from_str("claude:skill:").is_err());
    }

    #[test]
    fn item_ref_serializes_as_object() {
        let r = ItemRef::new(HarnessKind::Gemini, "extension", "workspace-a11y");
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(
            j,
            serde_json::json!({
                "harness": "gemini",
                "kind": "extension",
                "id": "workspace-a11y"
            })
        );
    }
}
