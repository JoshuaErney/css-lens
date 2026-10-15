// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

use std::collections::HashMap;

use lsp_types::{Position, Range, Url};

#[derive(Debug, Clone)]
pub struct ClassInfo {
    pub properties: String,
    pub selector: String,
    pub media_query: Option<String>,
    pub layer: Option<String>,
    pub source_file: String,
    pub source_path: String,
    pub definition_line: u32,
}

/// CSS custom properties map: `"--name"` → `"value"`.
pub type VarMap = HashMap<String, String>;
/// Selector definitions keyed by bare class name (`btn`) or `#`-prefixed ID (`#hero`).
pub type ClassMap = HashMap<String, Vec<ClassInfo>>;
pub type DocumentMap = HashMap<Url, String>;

#[derive(Debug, Clone)]
pub struct KeyframeInfo {
    pub source_file: String,
    pub source_path: String,
    pub definition_line: u32,
}
pub type KeyframesMap = HashMap<String, KeyframeInfo>;

/// Formats a class_map key for display: `btn` → `.btn`; `#hero` is unchanged.
pub fn display_key(key: &str) -> String {
    if key.starts_with('#') { key.to_string() } else { format!(".{key}") }
}

/// A zero-width range at the start of `line`, used to point at definitions.
pub fn line_start(line: u32) -> Range {
    let pos = Position { line, character: 0 };
    Range { start: pos, end: pos }
}
