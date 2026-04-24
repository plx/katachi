//! Policy set discovery.
//!
//! Policies may live inside an extension (under `policies/`) or as admin/
//! security settings inside a settings layer. Either way we model them as
//! `PolicySet` items whose raw JSON is preserved verbatim.
//!
//! This module also exports a small helper for projecting admin/security
//! constraints from a settings layer into a structured `ResolvedPolicy`
//! used by the validator.

use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{DiscoveredItem, ItemSource, PackageRef};

use crate::item::GeminiItemKind;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicySet {
    pub id: String,
    pub path: Utf8PathBuf,
    pub owner: PolicyOwner,
    pub body: Value,
    pub tier: PolicyTier,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PolicyOwner {
    Extension { extension: String },
    Settings { scope: String },
    BuiltIn,
}

impl PolicyOwner {
    pub fn as_scope(&self) -> &'static str {
        match self {
            Self::Extension { .. } => "extension",
            Self::Settings { .. } => "settings",
            Self::BuiltIn => "built-in",
        }
    }
}

/// Policy tier as exposed by Gemini's policy engine. Extensions run in a
/// restricted tier and cannot silently grant yolo behavior.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyTier {
    Admin,
    User,
    Extension,
    Project,
}

impl PolicyTier {
    pub fn from_owner(owner: &PolicyOwner) -> Self {
        match owner {
            PolicyOwner::Extension { .. } => Self::Extension,
            PolicyOwner::Settings { scope } => match scope.as_str() {
                "user" => Self::User,
                "project" => Self::Project,
                _ => Self::Admin,
            },
            PolicyOwner::BuiltIn => Self::Admin,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::User => "user",
            Self::Extension => "extension",
            Self::Project => "project",
        }
    }
}

pub fn scan_dir(dir: &Utf8Path, extension_name: &str) -> Option<Vec<PolicySet>> {
    if !dir.exists() {
        return None;
    }
    let Ok(iter) = fs::read_dir(dir.as_std_path()) else {
        return None;
    };
    let mut out = Vec::new();
    for entry in iter.flatten() {
        let Ok(p) = Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        if p.extension() == Some("json") && p.is_file() {
            let raw = match fs::read_to_string(p.as_std_path()) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let Ok(body) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let id = p.file_stem().unwrap_or("unknown").to_owned();
            let owner = PolicyOwner::Extension {
                extension: extension_name.to_owned(),
            };
            let tier = PolicyTier::from_owner(&owner);
            out.push(PolicySet {
                id,
                path: p,
                owner,
                body,
                tier,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Some(out)
}

/// Extract a PolicySet from a settings body, if any. We look under
/// `policies`, `security`, or `admin` keys.
pub fn scan_settings(body: &Value, settings_scope: &str, path: &Utf8Path) -> Option<PolicySet> {
    let mut composed = serde_json::Map::new();
    for key in ["policies", "security", "admin"] {
        if let Some(v) = body.get(key) {
            composed.insert(key.to_string(), v.clone());
        }
    }
    if composed.is_empty() {
        return None;
    }
    let owner = PolicyOwner::Settings {
        scope: settings_scope.to_owned(),
    };
    let tier = PolicyTier::from_owner(&owner);
    Some(PolicySet {
        id: format!("settings.{settings_scope}.policies"),
        path: path.to_owned(),
        owner,
        body: Value::Object(composed),
        tier,
    })
}

pub fn to_discovered_item(p: &PolicySet) -> DiscoveredItem {
    let packaging = match &p.owner {
        PolicyOwner::Extension { extension } => Some(PackageRef {
            item_ref: ItemRef::new(
                HarnessKind::Gemini,
                GeminiItemKind::Extension.as_str(),
                extension.clone(),
            ),
            required: true,
        }),
        _ => None,
    };
    DiscoveredItem {
        item_ref: ItemRef::new(HarnessKind::Gemini, GeminiItemKind::PolicySet.as_str(), p.id.clone()),
        display_name: p.id.clone(),
        source: ItemSource {
            path: Some(p.path.clone()),
            scope: Some(p.owner.as_scope().to_owned()),
            provenance: Some("policy".into()),
        },
        packaging,
        raw: serde_json::json!({
            "tier": p.tier,
            "body": p.body,
        }),
        capabilities: vec![format!("policy-tier:{}", p.tier.as_str())],
        constraints: Vec::new(),
    }
}

/// Projected view of admin/security settings used during validation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResolvedPolicy {
    /// Extensions disabled globally by admin.
    pub extensions_disabled: bool,
    /// Allowlist of extension names (empty = all allowed).
    pub allowed_extensions: Vec<String>,
    /// Blocklist of extension names.
    pub blocked_extensions: Vec<String>,
    /// MCP usage disabled globally by admin.
    pub mcp_disabled: bool,
    /// Approval modes forbidden by admin/security.
    pub forbidden_approval_modes: Vec<String>,
    /// Preview/experimental features enabled in settings.
    pub preview_features_enabled: bool,
}

impl ResolvedPolicy {
    /// Fold a sequence of settings bodies (lowest-to-highest precedence)
    /// and extension policy bodies into a single resolved policy.
    pub fn resolve(settings_bodies: &[&Value], extension_policies: &[&Value]) -> Self {
        let mut out = Self::default();
        for body in settings_bodies {
            merge_from(&mut out, body);
        }
        for body in extension_policies {
            // Extensions contribute *restrictive* signals only, never
            // loosen admin constraints.
            merge_from_restrictive(&mut out, body);
        }
        out
    }

    /// Is `extension_name` allowed to load under this policy?
    pub fn extension_allowed(&self, extension_name: &str) -> bool {
        if self.extensions_disabled {
            return false;
        }
        if !self.allowed_extensions.is_empty()
            && !self
                .allowed_extensions
                .iter()
                .any(|n| n == extension_name)
        {
            return false;
        }
        if self.blocked_extensions.iter().any(|n| n == extension_name) {
            return false;
        }
        true
    }

    /// Is `approval_mode` legal under this policy?
    pub fn approval_mode_allowed(&self, approval_mode: &str) -> bool {
        !self
            .forbidden_approval_modes
            .iter()
            .any(|m| m == approval_mode)
    }
}

fn merge_from(out: &mut ResolvedPolicy, body: &Value) {
    // Admin-style overrides under `admin` or flat keys.
    if let Some(sec) = body.get("security").or_else(|| body.get("admin")) {
        if sec.get("disableExtensions").and_then(|v| v.as_bool()) == Some(true) {
            out.extensions_disabled = true;
        }
        if let Some(arr) = sec.get("allowedExtensions").and_then(|v| v.as_array()) {
            out.allowed_extensions = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
        }
        if let Some(arr) = sec.get("blockedExtensions").and_then(|v| v.as_array()) {
            out.blocked_extensions = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
        }
        if sec.get("disableMcp").and_then(|v| v.as_bool()) == Some(true) {
            out.mcp_disabled = true;
        }
        if let Some(arr) = sec.get("forbiddenApprovalModes").and_then(|v| v.as_array()) {
            out.forbidden_approval_modes = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
        }
    }
    // Flat `experimental.*` or `preview` flags.
    let preview = body
        .get("experimental")
        .and_then(|v| v.as_bool())
        .or_else(|| body.get("preview").and_then(|v| v.as_bool()))
        .or_else(|| {
            body.get("experimental")
                .and_then(|v| v.get("enabled"))
                .and_then(|v| v.as_bool())
        });
    if preview == Some(true) {
        out.preview_features_enabled = true;
    }
}

fn merge_from_restrictive(out: &mut ResolvedPolicy, body: &Value) {
    // Extension policies can *tighten* by adding to blocklists, but
    // cannot unset `extensions_disabled` or grant preview flags.
    if let Some(arr) = body.get("blockedExtensions").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(name) = v.as_str() {
                if !out.blocked_extensions.iter().any(|s| s == name) {
                    out.blocked_extensions.push(name.to_owned());
                }
            }
        }
    }
    if let Some(arr) = body.get("forbiddenApprovalModes").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(name) = v.as_str() {
                if !out.forbidden_approval_modes.iter().any(|s| s == name) {
                    out.forbidden_approval_modes.push(name.to_owned());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn utf8(d: &TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(d.path().to_path_buf()).unwrap()
    }

    #[test]
    fn scan_dir_reads_policy_jsons() {
        let tmp = TempDir::new().unwrap();
        let dir = utf8(&tmp);
        fs::write(dir.join("readonly.json").as_std_path(), r#"{"allow": []}"#).unwrap();
        fs::write(dir.join("audit.json").as_std_path(), r#"{"tools": "read"}"#).unwrap();
        let out = scan_dir(&dir, "ext").unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "audit");
        assert_eq!(out[0].tier, PolicyTier::Extension);
    }

    #[test]
    fn settings_policy_combines_keys() {
        let body = serde_json::json!({
            "security": {"disableMcp": true},
            "admin": {"allowedExtensions": ["foo"]}
        });
        let p = scan_settings(&body, "admin", Utf8Path::new("/settings.json")).unwrap();
        assert_eq!(p.tier, PolicyTier::Admin);
        assert!(p.body.get("security").is_some());
        assert!(p.body.get("admin").is_some());
    }

    #[test]
    fn resolve_composes_settings() {
        let user = serde_json::json!({"security": {"allowedExtensions": ["a"]}});
        let project = serde_json::json!({"security": {"blockedExtensions": ["b"]}});
        let pol = ResolvedPolicy::resolve(&[&user, &project], &[]);
        assert_eq!(pol.allowed_extensions, vec!["a"]);
        assert_eq!(pol.blocked_extensions, vec!["b"]);
        assert!(!pol.extensions_disabled);
    }

    #[test]
    fn resolve_honors_disable_extensions() {
        let admin = serde_json::json!({"admin": {"disableExtensions": true}});
        let pol = ResolvedPolicy::resolve(&[&admin], &[]);
        assert!(pol.extensions_disabled);
        assert!(!pol.extension_allowed("anything"));
    }

    #[test]
    fn allowlist_applied_when_nonempty() {
        let user = serde_json::json!({"security": {"allowedExtensions": ["alpha"]}});
        let pol = ResolvedPolicy::resolve(&[&user], &[]);
        assert!(pol.extension_allowed("alpha"));
        assert!(!pol.extension_allowed("beta"));
    }

    #[test]
    fn blocklist_denies_specific() {
        let user = serde_json::json!({"security": {"blockedExtensions": ["evil"]}});
        let pol = ResolvedPolicy::resolve(&[&user], &[]);
        assert!(!pol.extension_allowed("evil"));
        assert!(pol.extension_allowed("fine"));
    }

    #[test]
    fn extension_policy_can_tighten_but_not_loosen() {
        let admin = serde_json::json!({"admin": {"disableExtensions": true}});
        let extension = serde_json::json!({"disableExtensions": false}); // should be ignored
        let pol = ResolvedPolicy::resolve(&[&admin], &[&extension]);
        assert!(pol.extensions_disabled);
    }

    #[test]
    fn preview_enabled_flag_detected() {
        let user = serde_json::json!({"experimental": true});
        let pol = ResolvedPolicy::resolve(&[&user], &[]);
        assert!(pol.preview_features_enabled);
    }
}
