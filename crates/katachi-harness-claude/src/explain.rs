//! `ClaudeHarness::explain` implementation.
//!
//! The explain path answers "what is item X and where did it come from?".
//! It mirrors the operator commands in the spec (`katachi harness claude
//! explain <item-id>`) and is driven by the CLI's `harness claude explain`
//! subcommand.

use katachi_core::error::ResolveError;
use katachi_core::harness::{ExplainContext, ExplainResult, ExplainSection};

use crate::item::ClaudeEdgeLabel;

pub fn explain(ctx: &ExplainContext<'_>) -> Result<ExplainResult, ResolveError> {
    let item = ctx
        .catalog
        .get(ctx.item)
        .ok_or_else(|| ResolveError::UnknownItem {
            item: ctx.item.to_string(),
        })?;

    let mut sections = Vec::new();

    if let Some(path) = &item.source.path {
        sections.push(ExplainSection {
            title: "Source".into(),
            body: format!(
                "{} (scope: {})",
                path,
                item.source.scope.clone().unwrap_or_else(|| "?".into())
            ),
        });
    }

    if let Some(pkg) = &item.packaging {
        sections.push(ExplainSection {
            title: "Package".into(),
            body: format!(
                "{} (required: {})",
                pkg.item_ref,
                if pkg.required { "yes" } else { "no" }
            ),
        });
    }

    if !item.capabilities.is_empty() {
        sections.push(ExplainSection {
            title: "Capabilities".into(),
            body: item.capabilities.join(", "),
        });
    }

    // Render edges grouped by whether they originate from or point to the
    // item, using the Claude-specific label that each edge stores in its
    // `note` field when available.
    let outgoing: Vec<_> = ctx.catalog.edges_from(ctx.item).collect();
    if !outgoing.is_empty() {
        let lines: Vec<String> = outgoing
            .iter()
            .map(|e| {
                let tag = e
                    .note
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", e.kind).to_lowercase());
                format!("- {tag} -> {}", e.to)
            })
            .collect();
        sections.push(ExplainSection {
            title: "Outgoing edges".into(),
            body: lines.join("\n"),
        });
    }
    let incoming: Vec<_> = ctx.catalog.edges_to(ctx.item).collect();
    if !incoming.is_empty() {
        let lines: Vec<String> = incoming
            .iter()
            .map(|e| {
                let tag = e
                    .note
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", e.kind).to_lowercase());
                format!("- {} -{tag}->", e.from)
            })
            .collect();
        sections.push(ExplainSection {
            title: "Incoming edges".into(),
            body: lines.join("\n"),
        });
    }

    let summary = if item.display_name == ctx.item.id {
        format!("{} (kind={})", ctx.item.id, ctx.item.kind)
    } else {
        format!(
            "{} — {} (kind={})",
            ctx.item.id, item.display_name, ctx.item.kind
        )
    };

    Ok(ExplainResult {
        item: ctx.item.clone(),
        summary,
        sections,
    })
}

/// Convenience: convert a `Claude`-labelled edge-note string into the typed
/// enum for display. Unknown notes fall through unchanged.
pub fn edge_label_display(note: &str) -> String {
    match note {
        "contains" => ClaudeEdgeLabel::Contains.to_string(),
        "skill_uses_agent" => ClaudeEdgeLabel::SkillUsesAgent.to_string(),
        "agent_preloads_skill" => ClaudeEdgeLabel::AgentPreloadsSkill.to_string(),
        "item_suggests_mcp" => ClaudeEdgeLabel::ItemSuggestsMcp.to_string(),
        "item_requires_mcp" => ClaudeEdgeLabel::ItemRequiresMcp.to_string(),
        "item_adds_hook" => ClaudeEdgeLabel::ItemAddsHook.to_string(),
        "scope_overrides" => ClaudeEdgeLabel::ScopeOverrides.to_string(),
        "backend_incompatible" => ClaudeEdgeLabel::BackendIncompatible.to_string(),
        other => other.to_string(),
    }
}
