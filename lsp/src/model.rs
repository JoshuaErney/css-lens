// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

use std::collections::{HashMap, HashSet};

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

/// One `--name: value` declaration.
#[derive(Debug, Clone)]
pub struct VarInfo {
    pub value: String,
    /// Selector of the rule the declaration sits in, e.g. `:root`.
    pub selector: String,
    pub source_file: String,
    pub source_path: String,
    pub definition_line: u32,
}

/// CSS custom properties: `"--name"` → every declaration of it, in parse order.
pub type VarMap = HashMap<String, Vec<VarInfo>>;
/// Selector definitions keyed by bare class name (`btn`) or `#`-prefixed ID (`#hero`).
pub type ClassMap = HashMap<String, Vec<ClassInfo>>;
pub type DocumentMap = HashMap<Url, String>;

/// Everything parsed from stylesheets: selectors and custom properties.
#[derive(Debug, Clone, Default)]
pub struct CssIndex {
    pub classes: ClassMap,
    pub vars: VarMap,
}

impl CssIndex {
    /// Drops every definition parsed from `source_path`.
    pub fn remove_source(&mut self, source_path: &str) {
        for infos in self.classes.values_mut() {
            infos.retain(|i| i.source_path != source_path);
        }
        self.classes.retain(|_, infos| !infos.is_empty());
        for infos in self.vars.values_mut() {
            infos.retain(|i| i.source_path != source_path);
        }
        self.vars.retain(|_, infos| !infos.is_empty());
    }

    /// Paths of every file that contributed a selector or custom property.
    pub fn source_paths(&self) -> HashSet<&str> {
        let classes = self.classes.values().flatten().map(|i| i.source_path.as_str());
        let vars = self.vars.values().flatten().map(|i| i.source_path.as_str());
        classes.chain(vars).collect()
    }

    /// A copy holding only definitions whose source file passes `keep`, which
    /// is called once per distinct file.
    pub fn filtered(&self, mut keep: impl FnMut(&str) -> bool) -> CssIndex {
        let mut verdicts: HashMap<String, bool> = HashMap::new();
        let mut keeps = |path: &str| match verdicts.get(path) {
            Some(&verdict) => verdict,
            None => {
                let verdict = keep(path);
                verdicts.insert(path.to_string(), verdict);
                verdict
            }
        };
        CssIndex {
            classes: filter_entries(&self.classes, |i| keeps(&i.source_path)),
            vars: filter_entries(&self.vars, |i| keeps(&i.source_path)),
        }
    }
}

fn filter_entries<T: Clone>(map: &HashMap<String, Vec<T>>, mut keep: impl FnMut(&T) -> bool) -> HashMap<String, Vec<T>> {
    map.iter()
        .filter_map(|(key, infos)| {
            let kept: Vec<T> = infos.iter().filter(|i| keep(i)).cloned().collect();
            (!kept.is_empty()).then(|| (key.clone(), kept))
        })
        .collect()
}

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
