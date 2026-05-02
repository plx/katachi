//! Markdown-with-frontmatter parser.
//!
//! A tolerant parser for Claude's frontmatter dialect: a document begins
//! with `---`, contains YAML-ish key/value pairs, and ends with `---`.
//! We deliberately avoid pulling in a full YAML dependency — Claude's own
//! frontmatter uses a small, predictable shape (strings, simple lists, and
//! booleans), and we'd rather fail gracefully than depend on yaml-rust for
//! the full syntax.
//!
//! Supported shapes:
//!
//! - `key: value` → string
//! - `key: "value"` → string (quotes stripped)
//! - `key: true|false` → bool
//! - `key: [a, b, c]` → inline list of strings
//! - `key:` followed by `  - item` lines → block list of strings
//!
//! Unknown shapes round-trip as raw strings so callers can inspect them.

use std::collections::BTreeMap;

use camino::Utf8Path;

use crate::error::ClaudeDiscoveryError;

/// Parsed frontmatter value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FmValue {
    String(String),
    Bool(bool),
    List(Vec<String>),
}

impl FmValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::String(s) => match s.trim() {
                "true" | "yes" => Some(true),
                "false" | "no" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<Vec<&str>> {
        match self {
            Self::List(v) => Some(v.iter().map(|s| s.as_str()).collect()),
            _ => None,
        }
    }
}

/// A successfully split file: the parsed frontmatter + the markdown body.
#[derive(Clone, Debug, Default)]
pub struct ParsedDoc {
    pub frontmatter: BTreeMap<String, FmValue>,
    pub body: String,
}

impl ParsedDoc {
    pub fn get(&self, key: &str) -> Option<&FmValue> {
        self.frontmatter.get(key)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.frontmatter.get(key).and_then(FmValue::as_str)
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.frontmatter.get(key).and_then(FmValue::as_bool)
    }

    pub fn get_list(&self, key: &str) -> Option<Vec<&str>> {
        self.frontmatter.get(key).and_then(FmValue::as_list)
    }
}

/// Parse the file contents `source`, attributing any errors to `path`.
pub fn parse(path: &Utf8Path, source: &str) -> Result<ParsedDoc, ClaudeDiscoveryError> {
    let (fm_text, body) = split_frontmatter(source);
    let frontmatter =
        parse_frontmatter(fm_text).map_err(|message| ClaudeDiscoveryError::Frontmatter {
            path: path.to_owned(),
            message,
        })?;
    Ok(ParsedDoc {
        frontmatter,
        body: body.to_string(),
    })
}

/// Split off the frontmatter block if present. Returns (frontmatter_text,
/// body). Files without a frontmatter return ("", whole file).
fn split_frontmatter(source: &str) -> (&str, &str) {
    // Tolerant BOM and leading whitespace.
    let s = source.strip_prefix('\u{feff}').unwrap_or(source);
    let trimmed_start = s.trim_start_matches([' ', '\t', '\r', '\n']);
    if !trimmed_start.starts_with("---") {
        return ("", source);
    }
    // Require the opening --- to be on its own line.
    let after_first = match trimmed_start.find('\n') {
        Some(idx) => &trimmed_start[idx + 1..],
        None => return ("", source),
    };
    // Find the closing `---` line.
    let mut total_offset = 0usize;
    for line in after_first.split_inclusive('\n') {
        let test = line.trim_end_matches(['\n', '\r']);
        if test == "---" {
            let fm = &after_first[..total_offset];
            let body = &after_first[total_offset + line.len()..];
            return (fm, body);
        }
        total_offset += line.len();
    }
    // No close found; treat whole file as body.
    ("", source)
}

fn parse_frontmatter(text: &str) -> Result<BTreeMap<String, FmValue>, String> {
    let mut out = BTreeMap::new();
    let mut iter = text.lines().enumerate().peekable();
    while let Some((_ln, line)) = iter.next() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim().starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once(':') else {
            return Err(format!("missing `:` in line `{line}`"));
        };
        let key = raw_key.trim().to_string();
        let value = raw_value.trim();
        if value.is_empty() {
            // Block list: following lines indented with `-`.
            let mut items = Vec::new();
            while let Some((_, next)) = iter.peek() {
                let trimmed = next.trim_start();
                if !trimmed.starts_with('-') {
                    break;
                }
                let item = trimmed[1..].trim().to_string();
                items.push(strip_optional_quotes(item));
                iter.next();
            }
            out.insert(key, FmValue::List(items));
        } else if value.starts_with('[') && value.ends_with(']') {
            let inner = &value[1..value.len() - 1];
            let items: Vec<String> = inner
                .split(',')
                .map(|s| strip_optional_quotes(s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect();
            out.insert(key, FmValue::List(items));
        } else if matches!(value.to_lowercase().as_str(), "true" | "false") {
            out.insert(key, FmValue::Bool(value.eq_ignore_ascii_case("true")));
        } else {
            let stripped = strip_optional_quotes(value.to_string());
            out.insert(key, FmValue::String(stripped));
        }
    }
    Ok(out)
}

fn strip_optional_quotes(mut s: String) -> String {
    if ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
        && s.len() >= 2
    {
        s.pop();
        s.remove(0);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_typical_skill_frontmatter() {
        let src = r#"---
name: axe-runner
description: "Run axe-core on the current page"
agent: reviewer
allowed-tools: [Bash, Read]
mcp_required: true
---
Body here.
"#;
        let doc = parse(Utf8Path::new("t.md"), src).unwrap();
        assert_eq!(doc.get_str("name"), Some("axe-runner"));
        assert_eq!(
            doc.get_str("description"),
            Some("Run axe-core on the current page")
        );
        assert_eq!(doc.get_str("agent"), Some("reviewer"));
        assert_eq!(doc.get_bool("mcp_required"), Some(true));
        let tools = doc.get_list("allowed-tools").unwrap();
        assert_eq!(tools, vec!["Bash", "Read"]);
        assert!(doc.body.starts_with("Body here"));
    }

    #[test]
    fn parse_block_list() {
        let src = r#"---
preloaded_skills:
  - axe-runner
  - axe-reporter
---
body
"#;
        let doc = parse(Utf8Path::new("t.md"), src).unwrap();
        let list = doc.get_list("preloaded_skills").unwrap();
        assert_eq!(list, vec!["axe-runner", "axe-reporter"]);
    }

    #[test]
    fn parse_no_frontmatter() {
        let src = "# Just body\ncontent";
        let doc = parse(Utf8Path::new("t.md"), src).unwrap();
        assert!(doc.frontmatter.is_empty());
        assert!(doc.body.contains("Just body"));
    }

    #[test]
    fn parse_tolerates_bom_and_trailing_whitespace() {
        let src = "\u{feff}---\nname: x   \n---\nbody";
        let doc = parse(Utf8Path::new("t.md"), src).unwrap();
        assert_eq!(doc.get_str("name"), Some("x"));
        assert_eq!(doc.body.trim(), "body");
    }

    #[test]
    fn parse_rejects_malformed_line() {
        let src = "---\njust-a-word\n---\n";
        let err = parse(Utf8Path::new("t.md"), src).unwrap_err();
        assert!(matches!(err, ClaudeDiscoveryError::Frontmatter { .. }));
    }

    #[test]
    fn parse_unterminated_frontmatter_treated_as_body() {
        let src = "---\nname: x\nno-end-marker";
        let doc = parse(Utf8Path::new("t.md"), src).unwrap();
        assert!(doc.frontmatter.is_empty());
        assert!(doc.body.contains("no-end-marker"));
    }

    #[test]
    fn bool_via_string_coercion() {
        let src = "---\nflag: yes\n---\n";
        let doc = parse(Utf8Path::new("t.md"), src).unwrap();
        assert_eq!(doc.get_bool("flag"), Some(true));
    }

    #[test]
    fn skips_blank_and_comment_lines() {
        let src = r#"---
# comment
name: x

description: y
---
body
"#;
        let doc = parse(Utf8Path::new("t.md"), src).unwrap();
        assert_eq!(doc.get_str("name"), Some("x"));
        assert_eq!(doc.get_str("description"), Some("y"));
    }
}
