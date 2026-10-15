// ---------------------------------------------------------------------------
// Workspace file discovery and the incremental usage index
// ---------------------------------------------------------------------------

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use lsp_types::{Location, Position, Range, Url};
use walkdir::{DirEntry, WalkDir};

use crate::html::html_selector_refs;
use crate::js::collect_js_class_refs;
use crate::model::DocumentMap;

pub const HTML_EXTENSIONS: &[&str] = &["html", "htm"];
pub const JS_EXTENSIONS: &[&str] = &["js"];

/// Dependency and hidden directories (and hidden files) are never indexed.
fn is_excluded_name(name: &str) -> bool {
    name == "node_modules" || name.starts_with('.')
}

pub fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.contains(&e))
}

/// Every file under `root` with one of `extensions`, skipping excluded names.
pub fn workspace_files<'a>(root: &Path, extensions: &'a [&'a str]) -> impl Iterator<Item = PathBuf> + 'a {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        // The root itself is always walked, even when its own name starts with '.'.
        .filter_entry(|e| e.depth() == 0 || !is_excluded_name(e.file_name().to_str().unwrap_or("")))
        .filter_map(|e| e.ok())
        .map(DirEntry::into_path)
        .filter(move |p| has_extension(p, extensions))
}

/// True when `path` is a file `workspace_files` would visit: inside `root` and
/// not under an excluded directory.
pub fn is_workspace_file(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|rel| {
        rel.components()
            .all(|c| !is_excluded_name(&c.as_os_str().to_string_lossy()))
    })
}

// ---------------------------------------------------------------------------
// Workspace HTML scanning (references + rename)
// ---------------------------------------------------------------------------

/// Visits every HTML file in the workspace, passing `(uri, text)` to `visitor`.
/// Prefers the editor's in-memory buffer; falls back to disk for closed files.
pub fn walk_html_files<F>(root: &Path, documents: &DocumentMap, mut visitor: F)
where
    F: FnMut(&Url, &str),
{
    for path in workspace_files(root, HTML_EXTENSIONS) {
        let uri = match Url::from_file_path(&path) {
            Ok(u) => u,
            Err(_) => continue,
        };
        let owned_text;
        let text: &str = if let Some(t) = documents.get(&uri) {
            t.as_str()
        } else {
            match fs::read_to_string(&path) {
                Ok(t) => { owned_text = t; owned_text.as_str() }
                Err(_) => continue,
            }
        };
        visitor(&uri, text);
    }
}

/// Returns all locations in workspace HTML files where `lookup_key` is used as
/// a class name (bare) or an ID value (`#`-prefixed key → bare in attribute).
pub fn workspace_html_refs(
    lookup_key: &str,
    root: &Path,
    documents: &DocumentMap,
) -> Vec<Location> {
    let (bare, is_id) = match lookup_key.strip_prefix('#') {
        Some(b) => (b, true),
        None => (lookup_key, false),
    };
    let mut locs = Vec::new();
    walk_html_files(root, documents, |uri, text| {
        for r in html_selector_refs(text) {
            if r.is_id == is_id && r.name == bare {
                locs.push(Location {
                    uri: uri.clone(),
                    range: Range {
                        start: Position { line: r.line, character: r.col_start },
                        end: Position { line: r.line, character: r.col_end },
                    },
                });
            }
        }
    });
    locs
}

// ---------------------------------------------------------------------------
// Incremental usage index
// ---------------------------------------------------------------------------

/// Selector usage across every workspace HTML file, plus the class and ID names
/// referenced from workspace JavaScript. Built once at startup, then updated one
/// file at a time from editor buffers and file-watcher events, so keystrokes and
/// saves never trigger a full workspace walk.
#[derive(Default)]
pub struct WorkspaceIndex {
    /// Per-file usage counts, keyed by class_map key.
    html_counts: HashMap<PathBuf, HashMap<String, usize>>,
    /// Sum of `html_counts` across all files.
    usage_counts: HashMap<String, usize>,
    js_refs: HashMap<PathBuf, HashSet<String>>,
}

impl WorkspaceIndex {
    pub fn build(root: &Path) -> Self {
        let mut index = Self::default();
        for path in workspace_files(root, HTML_EXTENSIONS) {
            index.reload_html(&path);
        }
        for path in workspace_files(root, JS_EXTENSIONS) {
            index.reload_js(&path);
        }
        index
    }

    /// Replaces the usage recorded for one HTML file with the usage in `text`.
    pub fn set_html(&mut self, path: PathBuf, text: &str) {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for r in html_selector_refs(text) {
            *counts.entry(r.key()).or_insert(0) += 1;
        }
        self.remove_html(&path);
        for (key, n) in &counts {
            *self.usage_counts.entry(key.clone()).or_insert(0) += n;
        }
        self.html_counts.insert(path, counts);
    }

    pub fn remove_html(&mut self, path: &Path) {
        let Some(old) = self.html_counts.remove(path) else { return };
        for (key, n) in old {
            if let Some(total) = self.usage_counts.get_mut(&key) {
                *total -= n;
                if *total == 0 {
                    self.usage_counts.remove(&key);
                }
            }
        }
    }

    /// Re-reads one HTML file from disk, dropping it if it can't be read.
    pub fn reload_html(&mut self, path: &Path) {
        match fs::read_to_string(path) {
            Ok(text) => self.set_html(path.to_path_buf(), &text),
            Err(_) => self.remove_html(path),
        }
    }

    /// Re-reads one JS file from disk, dropping it if it can't be read.
    pub fn reload_js(&mut self, path: &Path) {
        match fs::read_to_string(path) {
            Ok(text) => {
                let mut used = HashSet::new();
                collect_js_class_refs(&text, &mut used);
                self.js_refs.insert(path.to_path_buf(), used);
            }
            Err(_) => {
                self.js_refs.remove(path);
            }
        }
    }

    /// Number of times `key` appears in workspace HTML class/id attributes.
    pub fn usage_count(&self, key: &str) -> usize {
        self.usage_counts.get(key).copied().unwrap_or(0)
    }

    /// Every class and ID name referenced from workspace JavaScript.
    pub fn js_refs(&self) -> impl Iterator<Item = &String> {
        self.js_refs.values().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluded_paths_are_not_workspace_files() {
        let root = Path::new("/proj");
        assert!(is_workspace_file(root, Path::new("/proj/src/index.html")));
        assert!(!is_workspace_file(root, Path::new("/proj/node_modules/x/index.html")));
        assert!(!is_workspace_file(root, Path::new("/proj/.git/index.html")));
        assert!(!is_workspace_file(root, Path::new("/elsewhere/index.html")));
    }

    #[test]
    fn index_counts_update_per_file() {
        let mut index = WorkspaceIndex::default();
        index.set_html(PathBuf::from("/a.html"), r#"<div class="btn card" id="hero">"#);
        index.set_html(PathBuf::from("/b.html"), r#"<div class="btn">"#);
        assert_eq!(index.usage_count("btn"), 2);
        assert_eq!(index.usage_count("#hero"), 1);

        index.set_html(PathBuf::from("/a.html"), r#"<div class="card">"#);
        assert_eq!(index.usage_count("btn"), 1);
        assert_eq!(index.usage_count("#hero"), 0);

        index.remove_html(Path::new("/b.html"));
        assert_eq!(index.usage_count("btn"), 0);
        assert_eq!(index.usage_count("card"), 1);
    }
}
