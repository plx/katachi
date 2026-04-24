//! Gemini-specific validators.
//!
//! These hook into the shared validator framework from `katachi_core::validate`.
//! They consult the resolved policy (admin/security/extension-tier) and the
//! selected items to flag illegal loadouts before execution.

use std::sync::Arc;

use katachi_core::diagnostic::Diagnostic;
use katachi_core::roster::EdgeKind;
use katachi_core::validate::{ValidateContext, Validator};

use crate::item::GeminiItemKind;
use crate::policy::ResolvedPolicy;

/// A validator that enforces admin/security constraints on the resolved
/// selection of extensions and MCP servers.
pub struct GeminiPolicyValidator {
    policy: ResolvedPolicy,
}

impl GeminiPolicyValidator {
    pub fn new(policy: ResolvedPolicy) -> Self {
        Self { policy }
    }
}

impl Validator for GeminiPolicyValidator {
    fn code_prefix(&self) -> &'static str {
        "gemini.policy"
    }

    fn validate(&self, ctx: &ValidateContext<'_>) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        if self.policy.extensions_disabled {
            for item in &ctx.resolved.selected_items {
                if item.item.kind == GeminiItemKind::Extension.as_str() {
                    diags.push(
                        Diagnostic::error(
                            "gemini.policy.extensions-disabled",
                            format!(
                                "extension `{}` is selected but admin policy disables all extensions",
                                item.item.id
                            ),
                        )
                        .with_data(serde_json::json!({ "extension": item.item.id })),
                    );
                }
            }
        } else {
            for item in &ctx.resolved.selected_items {
                if item.item.kind == GeminiItemKind::Extension.as_str()
                    && !self.policy.extension_allowed(&item.item.id)
                {
                    diags.push(
                        Diagnostic::error(
                            "gemini.policy.extension-not-allowed",
                            format!(
                                "extension `{}` is not permitted by allow/blocklist policy",
                                item.item.id
                            ),
                        )
                        .with_data(serde_json::json!({ "extension": item.item.id })),
                    );
                }
            }
        }
        if self.policy.mcp_disabled {
            for item in &ctx.resolved.selected_items {
                if item.item.kind == GeminiItemKind::McpServer.as_str() {
                    diags.push(
                        Diagnostic::error(
                            "gemini.policy.mcp-disabled",
                            format!(
                                "MCP server `{}` is selected but admin policy disables MCP",
                                item.item.id
                            ),
                        )
                        .with_data(serde_json::json!({ "mcp_server": item.item.id })),
                    );
                }
            }
        }
        diags
    }
}

/// Validator that checks whether any selected subagent requires
/// preview/experimental feature flags and whether those flags are
/// available in the resolved policy.
pub struct GeminiPreviewValidator {
    policy: ResolvedPolicy,
}

impl GeminiPreviewValidator {
    pub fn new(policy: ResolvedPolicy) -> Self {
        Self { policy }
    }
}

impl Validator for GeminiPreviewValidator {
    fn code_prefix(&self) -> &'static str {
        "gemini.preview"
    }

    fn validate(&self, ctx: &ValidateContext<'_>) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        if self.policy.preview_features_enabled {
            return diags;
        }
        for item in &ctx.resolved.selected_items {
            if item.item.kind != GeminiItemKind::Subagent.as_str() {
                continue;
            }
            if let Some(node) = ctx.catalog.get(&item.item) {
                if node
                    .capabilities
                    .iter()
                    .any(|c| c == "preview-required")
                {
                    diags.push(
                        Diagnostic::error(
                            "gemini.preview.required",
                            format!(
                                "subagent `{}` requires preview features, but policy has them disabled",
                                item.item.id
                            ),
                        )
                        .with_data(serde_json::json!({ "subagent": item.item.id })),
                    );
                }
            }
        }
        diags
    }
}

/// Validator that flags MCP servers whose projection depends on a
/// conflicting (same-name) server in a higher-precedence layer.
pub struct GeminiMcpConflictValidator;

impl Validator for GeminiMcpConflictValidator {
    fn code_prefix(&self) -> &'static str {
        "gemini.mcp-conflict"
    }

    fn validate(&self, ctx: &ValidateContext<'_>) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        for item in &ctx.resolved.selected_items {
            if item.item.kind != GeminiItemKind::McpServer.as_str() {
                continue;
            }
            let overrides: Vec<_> = ctx
                .catalog
                .edges_from(&item.item)
                .filter(|e| e.kind == EdgeKind::Projection)
                .collect();
            if !overrides.is_empty() {
                let shadows: Vec<String> =
                    overrides.iter().map(|e| e.to.to_string()).collect();
                diags.push(
                    Diagnostic::warning(
                        "gemini.mcp-conflict.settings-wins",
                        format!(
                            "extension MCP `{}` is shadowed by settings-layer servers: {}",
                            item.item.id,
                            shadows.join(", ")
                        ),
                    )
                    .with_data(serde_json::json!({
                        "extension_mcp": item.item.id,
                        "shadowed_by": shadows,
                    })),
                );
            }
        }
        diags
    }
}

/// Validator that rejects SDK projections depending on features the SDK
/// doesn't implement. The planner already rejects these at plan time,
/// but surfacing it earlier (at validate time) yields a cleaner error
/// path for `katachi have ... describe`-style inspections.
pub struct GeminiSdkProjectionValidator {
    pub backend_hint: Option<katachi_core::model::BackendKind>,
}

impl Validator for GeminiSdkProjectionValidator {
    fn code_prefix(&self) -> &'static str {
        "gemini.projection"
    }

    fn validate(&self, ctx: &ValidateContext<'_>) -> Vec<Diagnostic> {
        use katachi_core::model::BackendKind;
        let backend = self.backend_hint.unwrap_or(ctx.resolved.backend);
        let mut diags = Vec::new();
        if backend != BackendKind::SdkTs {
            return diags;
        }
        for item in &ctx.resolved.selected_items {
            let unsupported = match item.item.kind.as_str() {
                "extension" | "subagent" | "hook_set" | "policy_set" => Some(item.item.kind.as_str()),
                _ => None,
            };
            if let Some(kind) = unsupported {
                diags.push(
                    Diagnostic::error(
                        "gemini.projection.sdk-ts-unsupported",
                        format!(
                            "SDK-ts projection does not support `{}` items (via `{}`)",
                            kind, item.item
                        ),
                    )
                    .with_data(serde_json::json!({
                        "item": item.item,
                        "backend": "sdk-ts",
                    })),
                );
            }
        }
        diags
    }
}

/// Build the default Gemini validator bundle. Callers must pass the
/// resolved policy (typically folded from settings + extension policies).
pub fn gemini_validators(policy: ResolvedPolicy) -> Vec<Arc<dyn Validator>> {
    vec![
        Arc::new(GeminiPolicyValidator::new(policy.clone())),
        Arc::new(GeminiPreviewValidator::new(policy)),
        Arc::new(GeminiMcpConflictValidator),
        Arc::new(GeminiSdkProjectionValidator { backend_hint: None }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use katachi_core::diagnostic::Severity;
    use katachi_core::katachi::KatachiDefinition;
    use katachi_core::model::{BackendKind, HarnessKind, ItemRef};
    use katachi_core::plan::{ResolvedItemRef, ResolvedKatachi, RunProfile, SelectionReason};
    use katachi_core::roster::{
        DependencyEdge, DiscoveredItem, EdgeKind as CoreEdgeKind, ItemSource, RosterCatalog,
    };

    fn pick(kind: &str, id: &str) -> ResolvedItemRef {
        ResolvedItemRef {
            item: ItemRef::new(HarnessKind::Gemini, kind, id),
            reason: SelectionReason::Direct,
            pulled_in_by: None,
        }
    }

    fn resolved(items: Vec<ResolvedItemRef>) -> ResolvedKatachi {
        ResolvedKatachi {
            katachi_id: "t".into(),
            harness: HarnessKind::Gemini,
            backend: BackendKind::Cli,
            selected_items: items,
            run_profile: RunProfile::default(),
            diagnostics: Vec::new(),
        }
    }

    fn def() -> KatachiDefinition {
        KatachiDefinition::from_toml_str(
            r#"
id = "t"
[[targets]]
harness = "gemini"
"#,
        )
        .unwrap()
    }

    fn item(kind: &str, id: &str, caps: Vec<&str>) -> DiscoveredItem {
        DiscoveredItem {
            item_ref: ItemRef::new(HarnessKind::Gemini, kind, id),
            display_name: id.into(),
            source: ItemSource::default(),
            packaging: None,
            raw: serde_json::Value::Null,
            capabilities: caps.into_iter().map(str::to_owned).collect(),
            constraints: Vec::new(),
        }
    }

    #[test]
    fn disable_extensions_fails_when_extensions_selected() {
        let mut policy = ResolvedPolicy::default();
        policy.extensions_disabled = true;
        let v = GeminiPolicyValidator::new(policy);
        let res = resolved(vec![pick("extension", "a")]);
        let mut cat = RosterCatalog::empty(HarnessKind::Gemini);
        cat.insert_item(item("extension", "a", vec![])).unwrap();
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &cat,
            definition: &def(),
        });
        assert!(out.iter().any(|d| d.code == "gemini.policy.extensions-disabled"));
    }

    #[test]
    fn allowlist_blocks_other_extensions() {
        let mut policy = ResolvedPolicy::default();
        policy.allowed_extensions = vec!["alpha".into()];
        let v = GeminiPolicyValidator::new(policy);
        let res = resolved(vec![pick("extension", "beta")]);
        let mut cat = RosterCatalog::empty(HarnessKind::Gemini);
        cat.insert_item(item("extension", "beta", vec![])).unwrap();
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &cat,
            definition: &def(),
        });
        assert!(out.iter().any(|d| d.code == "gemini.policy.extension-not-allowed"));
    }

    #[test]
    fn mcp_disabled_fails_when_mcp_selected() {
        let mut policy = ResolvedPolicy::default();
        policy.mcp_disabled = true;
        let v = GeminiPolicyValidator::new(policy);
        let res = resolved(vec![pick("mcp_server", "ext:foo:chrome")]);
        let mut cat = RosterCatalog::empty(HarnessKind::Gemini);
        cat.insert_item(item("mcp_server", "ext:foo:chrome", vec![])).unwrap();
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &cat,
            definition: &def(),
        });
        assert!(out.iter().any(|d| d.code == "gemini.policy.mcp-disabled"));
    }

    #[test]
    fn preview_validator_errors_without_flag() {
        let policy = ResolvedPolicy::default();
        let v = GeminiPreviewValidator::new(policy);
        let res = resolved(vec![pick("subagent", "explorer")]);
        let mut cat = RosterCatalog::empty(HarnessKind::Gemini);
        cat.insert_item(item("subagent", "explorer", vec!["preview-required"]))
            .unwrap();
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &cat,
            definition: &def(),
        });
        assert!(out.iter().any(|d| d.code == "gemini.preview.required"));
    }

    #[test]
    fn preview_validator_silent_when_flag_enabled() {
        let mut policy = ResolvedPolicy::default();
        policy.preview_features_enabled = true;
        let v = GeminiPreviewValidator::new(policy);
        let res = resolved(vec![pick("subagent", "explorer")]);
        let mut cat = RosterCatalog::empty(HarnessKind::Gemini);
        cat.insert_item(item("subagent", "explorer", vec!["preview-required"]))
            .unwrap();
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &cat,
            definition: &def(),
        });
        assert!(out.is_empty());
    }

    #[test]
    fn mcp_conflict_validator_surfaces_warning() {
        let v = GeminiMcpConflictValidator;
        let ext_ref = ItemRef::new(HarnessKind::Gemini, "mcp_server", "ext:pkg:chrome");
        let settings_ref =
            ItemRef::new(HarnessKind::Gemini, "mcp_server", "settings:project:chrome");
        let mut cat = RosterCatalog::empty(HarnessKind::Gemini);
        cat.insert_item(item("mcp_server", "ext:pkg:chrome", vec![]))
            .unwrap();
        cat.insert_item(item("mcp_server", "settings:project:chrome", vec![]))
            .unwrap();
        cat.insert_edge(DependencyEdge {
            from: ext_ref.clone(),
            to: settings_ref.clone(),
            kind: CoreEdgeKind::Projection,
            required: false,
            note: None,
        })
        .unwrap();
        let res = resolved(vec![ResolvedItemRef {
            item: ext_ref,
            reason: SelectionReason::Direct,
            pulled_in_by: None,
        }]);
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &cat,
            definition: &def(),
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, Severity::Warning);
        assert_eq!(out[0].code, "gemini.mcp-conflict.settings-wins");
    }

    #[test]
    fn sdk_projection_rejects_extensions() {
        let v = GeminiSdkProjectionValidator {
            backend_hint: Some(BackendKind::SdkTs),
        };
        let res = resolved(vec![pick("extension", "foo")]);
        let cat = RosterCatalog::empty(HarnessKind::Gemini);
        let out = v.validate(&ValidateContext {
            resolved: &res,
            catalog: &cat,
            definition: &def(),
        });
        assert!(out.iter().any(|d| d.code == "gemini.projection.sdk-ts-unsupported"));
    }
}
