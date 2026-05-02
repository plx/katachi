//! Codex instruction-chain loader.
//!
//! Codex assembles an instruction chain by walking `AGENTS.md` (and
//! `AGENTS.override.md`) from the Codex home, then from the filesystem
//! root down to the cwd for each project root. Later files override (or
//! extend) earlier ones; preserving source order is essential.

use camino::{Utf8Path, Utf8PathBuf};
use katachi_core::diagnostic::Diagnostic;
use katachi_core::model::{HarnessKind, ItemRef};
use katachi_core::roster::{
    DependencyEdge, DiscoveredItem, EdgeKind, ItemSource, RosterBuildError, RosterCatalog,
};

use crate::items::{CodexEdgeKind, CodexItemKind};
use crate::CodexSettings;

/// Known instruction-doc file names, in precedence order (later overrides
/// earlier within the same directory).
const INSTRUCTION_FILES: &[&str] = &["AGENTS.md", "AGENTS.override.md"];

/// Source category of an instruction doc.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InstructionScope {
    Global,
    Project,
}

/// A single discovered instruction document.
#[derive(Clone, Debug)]
pub struct InstructionDoc {
    pub id: String,
    pub path: Utf8PathBuf,
    pub scope: InstructionScope,
    pub order: u32,
    pub body: String,
}

impl InstructionDoc {
    pub fn item_ref(&self) -> ItemRef {
        ItemRef::new(
            HarnessKind::Codex,
            CodexItemKind::InstructionDoc.as_str(),
            &self.id,
        )
    }

    pub fn to_item(&self) -> DiscoveredItem {
        let scope = match self.scope {
            InstructionScope::Global => "global",
            InstructionScope::Project => "project",
        };
        let raw = serde_json::json!({
            "scope": scope,
            "path": self.path.as_str(),
            "order": self.order,
            "bytes": self.body.len(),
        });
        DiscoveredItem {
            item_ref: self.item_ref(),
            display_name: self.id.clone(),
            source: ItemSource {
                path: Some(self.path.clone()),
                scope: Some(scope.to_string()),
                provenance: Some("codex.instruction_doc".into()),
            },
            packaging: None,
            raw,
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }
}

/// Walk Codex home + project roots for instruction documents.
pub fn discover_instruction_chain(
    settings: &CodexSettings,
    project_roots: &[Utf8PathBuf],
    cwd: &Utf8Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<InstructionDoc> {
    let mut docs: Vec<InstructionDoc> = Vec::new();
    let mut order: u32 = 0;

    // 1. Codex home AGENTS.md chain.
    for filename in INSTRUCTION_FILES {
        let path = settings.codex_home.join(filename);
        if path.exists() {
            if let Some(doc) = read_doc(&path, InstructionScope::Global, &mut order, diagnostics) {
                docs.push(doc);
            }
        }
    }

    // 2. Project roots, each root-to-cwd.
    for root in project_roots {
        for ancestor in ancestors_root_to(root, cwd) {
            for filename in INSTRUCTION_FILES {
                let path = ancestor.join(filename);
                if path.exists() {
                    if let Some(doc) =
                        read_doc(&path, InstructionScope::Project, &mut order, diagnostics)
                    {
                        docs.push(doc);
                    }
                }
            }
        }
    }

    // Deduplicate by absolute path while preserving order.
    let mut seen = std::collections::HashSet::new();
    docs.retain(|d| seen.insert(d.path.clone()));
    // Re-assign order indices after deduplication for stability.
    for (idx, doc) in docs.iter_mut().enumerate() {
        doc.order = idx as u32;
    }
    docs
}

/// Insert [`CodexEdgeKind::InstructionChainBefore`] edges so each document
/// points to the one immediately preceding it in the chain.
pub fn insert_instruction_edges(
    docs: &[InstructionDoc],
    catalog: &mut RosterCatalog,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for window in docs.windows(2) {
        let earlier = &window[0];
        let later = &window[1];
        let edge = DependencyEdge {
            from: later.item_ref(),
            to: earlier.item_ref(),
            kind: EdgeKind::Semantic,
            required: false,
            note: Some(CodexEdgeKind::InstructionChainBefore.as_str().to_string()),
        };
        insert_edge(catalog, edge, diagnostics);
    }
}

fn insert_edge(
    catalog: &mut RosterCatalog,
    edge: DependencyEdge,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match catalog.insert_edge(edge) {
        Ok(()) | Err(RosterBuildError::DuplicateEdge { .. }) => {}
        Err(err) => diagnostics.push(Diagnostic::warning(
            "codex.instruction.edge",
            format!("failed to insert instruction edge: {err}"),
        )),
    }
}

fn read_doc(
    path: &Utf8Path,
    scope: InstructionScope,
    order: &mut u32,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<InstructionDoc> {
    let body = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) => {
            diagnostics.push(Diagnostic::warning(
                "codex.instruction.read",
                format!("failed to read `{path}`: {err}"),
            ));
            return None;
        }
    };
    let id = format!(
        "{}:{}",
        match scope {
            InstructionScope::Global => "global",
            InstructionScope::Project => "project",
        },
        path
    );
    let doc = InstructionDoc {
        id,
        path: path.to_path_buf(),
        scope,
        order: *order,
        body,
    };
    *order = order.saturating_add(1);
    Some(doc)
}

fn ancestors_root_to(root: &Utf8Path, cwd: &Utf8Path) -> Vec<Utf8PathBuf> {
    if !cwd.starts_with(root) {
        return vec![root.to_path_buf()];
    }
    let relative = cwd.strip_prefix(root).unwrap();
    let mut out = vec![root.to_path_buf()];
    let mut acc = root.to_path_buf();
    for comp in relative.components() {
        if let camino::Utf8Component::Normal(n) = comp {
            acc.push(n);
            out.push(acc.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(p: &Utf8Path, body: &str) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn chain_is_ordered_root_to_cwd() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        write(&root.join("AGENTS.md"), "root");
        let a = root.join("a");
        write(&a.join("AGENTS.md"), "a");
        let b = a.join("b");
        write(&b.join("AGENTS.md"), "b");

        let settings = CodexSettings {
            codex_home: td.path().join("absent").try_into().unwrap(),
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let docs = discover_instruction_chain(&settings, &[root.clone()], &b, &mut diags);
        let bodies: Vec<_> = docs.iter().map(|d| d.body.trim().to_string()).collect();
        assert_eq!(bodies, vec!["root", "a", "b"]);
        for (i, d) in docs.iter().enumerate() {
            assert_eq!(d.order, i as u32);
        }
    }

    #[test]
    fn override_file_picked_up_alongside_main() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        write(&root.join("AGENTS.md"), "main");
        write(&root.join("AGENTS.override.md"), "over");

        let settings = CodexSettings {
            codex_home: td.path().join("absent").try_into().unwrap(),
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let docs = discover_instruction_chain(&settings, &[root.clone()], &root, &mut diags);
        let bodies: Vec<_> = docs.iter().map(|d| d.body.trim().to_string()).collect();
        assert_eq!(bodies, vec!["main", "over"]);
    }

    #[test]
    fn codex_home_file_included_first() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        let home = root.join("codex-home");
        write(&home.join("AGENTS.md"), "global");
        write(&root.join("AGENTS.md"), "local");

        let settings = CodexSettings {
            codex_home: home,
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let docs = discover_instruction_chain(&settings, &[root.clone()], &root, &mut diags);
        let bodies: Vec<_> = docs.iter().map(|d| d.body.trim().to_string()).collect();
        assert_eq!(bodies, vec!["global", "local"]);
    }

    #[test]
    fn edges_connect_adjacent_docs() {
        let td = TempDir::new().unwrap();
        let root = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).unwrap();
        write(&root.join("AGENTS.md"), "root");
        let a = root.join("a");
        write(&a.join("AGENTS.md"), "a");
        let settings = CodexSettings {
            codex_home: td.path().join("absent").try_into().unwrap(),
            ..CodexSettings::default()
        };
        let mut diags = Vec::new();
        let docs = discover_instruction_chain(&settings, &[root.clone()], &a, &mut diags);
        let mut cat = RosterCatalog::empty(HarnessKind::Codex);
        for d in &docs {
            cat.insert_item(d.to_item()).unwrap();
        }
        insert_instruction_edges(&docs, &mut cat, &mut diags);
        assert_eq!(cat.iter_edges().count(), 1);
        let e = cat.iter_edges().next().unwrap();
        assert_eq!(e.note.as_deref(), Some("instruction_chain_before"));
    }
}
