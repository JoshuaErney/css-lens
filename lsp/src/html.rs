// ---------------------------------------------------------------------------
// HTML attribute scanning and per-document CSS scoping
// ---------------------------------------------------------------------------

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use lsp_types::Url;
use regex::Regex;

use crate::css::{extract_imports, is_too_large, parse_css_content, Source};
use crate::model::{ClassInfo, ClassMap};
use crate::regexes::{class_attr_re, href_val_re, id_attr_re, link_tag_re, rel_stylesheet_re, style_block_re};

pub fn is_html_uri(uri: &Url) -> bool {
    let p = uri.path();
    p.ends_with(".html") || p.ends_with(".htm")
}

/// One class or ID token inside an HTML `class=""` / `id=""` attribute value.
pub struct SelectorRef {
    pub line: u32,
    pub col_start: u32,
    pub col_end: u32,
    pub name: String,
    pub is_id: bool,
}

impl SelectorRef {
    /// The class_map key for this token: bare for classes, `#`-prefixed for IDs.
    pub fn key(&self) -> String {
        if self.is_id { format!("#{}", self.name) } else { self.name.clone() }
    }
}

pub fn html_selector_refs(text: &str) -> Vec<SelectorRef> {
    let mut refs = Vec::new();
    let mut open_attr: Option<(char, bool)> = None;
    for (line_num, line) in text.lines().enumerate() {
        if let Some((quote, is_id)) = open_attr.take() {
            open_attr = collect_continuation(line, line_num as u32, quote, is_id, &mut refs);
            if open_attr.is_some() {
                continue;
            }
        }
        let class_open = collect_attr_refs(line, line_num as u32, class_attr_re(), false, &mut refs);
        let id_open = collect_attr_refs(line, line_num as u32, id_attr_re(), true, &mut refs);
        open_attr = class_open.or(id_open);
    }
    refs
}

fn collect_value_tokens(
    line: &str,
    line_num: u32,
    value_start: usize,
    end: usize,
    is_id: bool,
    refs: &mut Vec<SelectorRef>,
) {
    let value = &line[value_start..end];
    let mut tok_start = 0usize;
    for (i, &b) in value.as_bytes().iter().enumerate() {
        if b == b' ' || b == b'\t' {
            if tok_start < i {
                refs.push(SelectorRef {
                    line: line_num,
                    col_start: (value_start + tok_start) as u32,
                    col_end: (value_start + i) as u32,
                    name: value[tok_start..i].to_string(),
                    is_id,
                });
            }
            tok_start = i + 1;
        }
    }
    if tok_start < value.len() {
        refs.push(SelectorRef {
            line: line_num,
            col_start: (value_start + tok_start) as u32,
            col_end: (value_start + value.len()) as u32,
            name: value[tok_start..].to_string(),
            is_id,
        });
    }
}

/// Locates the value of the attribute whose opening quote ends at `value_start`.
/// Returns `(value_end, still_open)`; a raw `<` before the closing quote means
/// the attribute is malformed, so the value is cut there rather than continued.
pub fn attr_value_extent(line: &str, value_start: usize, quote: char) -> (usize, bool) {
    let rest = &line[value_start..];
    let tag_start = rest.find('<');
    match rest.find(quote) {
        Some(len) if tag_start.is_none_or(|t| len < t) => (value_start + len, false),
        _ => match tag_start {
            Some(t) => (value_start + t, false), // terminate at '<', don't continue
            None => (value_start + rest.len(), true),
        },
    }
}

fn collect_attr_refs(
    line: &str,
    line_num: u32,
    attr_re: &Regex,
    is_id: bool,
    refs: &mut Vec<SelectorRef>,
) -> Option<(char, bool)> {
    let mut last_open = None;
    for cap in attr_re.captures_iter(line) {
        let Some(whole) = cap.get(0) else { continue };
        let quote = cap[1].chars().next().unwrap_or('"');
        let value_start = whole.end();
        let (value_end, open) = attr_value_extent(line, value_start, quote);
        collect_value_tokens(line, line_num, value_start, value_end, is_id, refs);
        last_open = if open { Some((quote, is_id)) } else { None };
    }
    last_open
}

/// Processes the leading portion of a line that continues an open attribute from
/// the previous line. Returns `Some((quote, is_id))` if still unclosed at line end.
/// A raw `<` before the closing quote is treated as a tag boundary — the
/// attribute is terminated there rather than spilling into adjacent markup.
fn collect_continuation(
    line: &str,
    line_num: u32,
    quote: char,
    is_id: bool,
    refs: &mut Vec<SelectorRef>,
) -> Option<(char, bool)> {
    let tag_boundary = line.find('<').unwrap_or(line.len());
    let search = &line[..tag_boundary];
    match search.find(quote) {
        Some(close) => {
            collect_value_tokens(line, line_num, 0, close, is_id, refs);
            None
        }
        None if tag_boundary < line.len() => {
            // '<' found before closing quote — malformed attribute, terminate here.
            collect_value_tokens(line, line_num, 0, tag_boundary, is_id, refs);
            None
        }
        None => {
            collect_value_tokens(line, line_num, 0, line.len(), is_id, refs);
            Some((quote, is_id))
        }
    }
}

// ---------------------------------------------------------------------------
// Per-document CSS scoping
// ---------------------------------------------------------------------------

/// Extracts hrefs from `<link rel="stylesheet" href="...">` tags, skipping
/// network URLs and server-absolute paths that can't be resolved locally.
pub fn extract_linked_css(html_text: &str) -> Vec<String> {
    let mut links = Vec::new();
    for m in link_tag_re().find_iter(html_text) {
        let tag = m.as_str();
        if !rel_stylesheet_re().is_match(tag) {
            continue;
        }
        let cap = match href_val_re().captures(tag) {
            Some(c) => c,
            None => continue,
        };
        let href_raw = cap[1].to_string();
        if href_raw.starts_with("http://")
            || href_raw.starts_with("https://")
            || href_raw.starts_with("//")
            || href_raw.starts_with('/')
        {
            continue;
        }
        // Strip query string and fragment before resolving.
        let href = href_raw.split('?').next().unwrap_or(&href_raw);
        let href = href.split('#').next().unwrap_or(href);
        if !href.is_empty() {
            links.push(href.to_string());
        }
    }
    links
}

/// Returns canonical paths of all CSS files reachable from `html_path` via
/// `<link rel="stylesheet">` tags and their `@import` chains.
/// Returns an empty set when no locally-resolvable links are found.
pub fn reachable_css_paths(html_path: &Path, html_text: &str) -> HashSet<PathBuf> {
    let hrefs = extract_linked_css(html_text);
    if hrefs.is_empty() {
        return HashSet::new();
    }
    let html_dir = match html_path.parent() {
        Some(d) => d,
        None => return HashSet::new(),
    };
    let mut reachable: HashSet<PathBuf> = HashSet::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    for href in hrefs {
        collect_reachable(&html_dir.join(&href), &mut reachable, &mut visited);
    }
    reachable
}

fn collect_reachable(path: &Path, reachable: &mut HashSet<PathBuf>, visited: &mut HashSet<PathBuf>) {
    // Mirror parse_css_file_inner: skip minified/large files so the reachable
    // set never includes a file whose classes were excluded from class_map.
    if is_too_large(path) {
        return;
    }
    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return,
    };
    if !visited.insert(canonical.clone()) {
        return;
    }
    reachable.insert(canonical);
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let parent = path.parent().unwrap_or(Path::new("."));
    for import_path in extract_imports(&content) {
        collect_reachable(&parent.join(&import_path), reachable, visited);
    }
}

pub fn is_reachable(source_path: &str, reachable: &HashSet<PathBuf>) -> bool {
    Path::new(source_path)
        .canonicalize()
        .map(|c| reachable.contains(&c))
        .unwrap_or(false)
}

/// Returns a ClassMap filtered to entries whose source file is in `reachable`.
/// Returns `None` when `reachable` is empty — callers fall back to the global map.
pub fn build_scoped_class_map(class_map: &ClassMap, reachable: &HashSet<PathBuf>) -> Option<ClassMap> {
    if reachable.is_empty() {
        return None;
    }
    // Canonicalize each source file once rather than once per selector entry.
    let mut file_reachable: HashMap<&str, bool> = HashMap::new();
    let mut scoped = ClassMap::new();
    for (key, infos) in class_map {
        let scoped_infos: Vec<ClassInfo> = infos
            .iter()
            .filter(|i| {
                *file_reachable
                    .entry(i.source_path.as_str())
                    .or_insert_with(|| is_reachable(&i.source_path, reachable))
            })
            .cloned()
            .collect();
        if !scoped_infos.is_empty() {
            scoped.insert(key.clone(), scoped_infos);
        }
    }
    Some(scoped)
}

/// Returns `(base_line, content)` for every `<style>...</style>` block in the
/// HTML, where `base_line` is the 0-indexed line number of the first content
/// character so selector `definition_line`s point into the HTML file.
pub fn extract_style_blocks(html_text: &str) -> Vec<(u32, String)> {
    style_block_re()
        .captures_iter(html_text)
        .filter_map(|cap| {
            let m = cap.get(1)?;
            let base_line = html_text[..m.start()]
                .bytes()
                .filter(|&b| b == b'\n')
                .count() as u32;
            Some((base_line, m.as_str().to_string()))
        })
        .collect()
}

/// Builds the effective ClassMap for a single HTML document by combining:
///   - CSS reachable via `<link rel="stylesheet">` tags (or the full global map
///     when no resolvable links are found)
///   - Classes defined in inline `<style>` blocks
///
/// Returns `None` when neither linked CSS nor inline styles are present,
/// signalling callers to use the unmodified global class_map.
pub fn build_document_class_map(class_map: &ClassMap, html_path: &Path, html_text: &str) -> Option<ClassMap> {
    let reachable = reachable_css_paths(html_path, html_text);
    let style_blocks = extract_style_blocks(html_text);

    if reachable.is_empty() && style_blocks.is_empty() {
        return None;
    }

    // Start from linked CSS scope, or clone global when there are no <link> tags.
    let mut doc_map = if reachable.is_empty() {
        class_map.clone()
    } else {
        build_scoped_class_map(class_map, &reachable).unwrap_or_default()
    };

    // Merge inline style blocks (definition_line is relative to the HTML file).
    if !style_blocks.is_empty() {
        let source_file = html_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let source_path = html_path.to_string_lossy().into_owned();
        let source = Source { file: &source_file, path: &source_path };
        for (base_line, content) in style_blocks {
            parse_css_content(&content, base_line, source, &mut doc_map);
        }
    }

    Some(doc_map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(refs: &[SelectorRef]) -> Vec<&str> {
        refs.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn single_line_class_attr() {
        let html = r#"<div class="foo bar baz"></div>"#;
        let refs = html_selector_refs(html);
        assert_eq!(names(&refs), ["foo", "bar", "baz"]);
    }

    #[test]
    fn multi_line_class_attr() {
        let html = "<div class=\"foo\n  bar baz\">";
        let refs = html_selector_refs(html);
        assert_eq!(names(&refs), ["foo", "bar", "baz"]);
    }

    #[test]
    fn multi_line_class_attr_three_lines() {
        let html = "<div class=\"foo\n  bar\n  baz\">";
        let refs = html_selector_refs(html);
        assert_eq!(names(&refs), ["foo", "bar", "baz"]);
    }

    #[test]
    fn multi_line_then_new_attr_on_same_line() {
        let html = "<div class=\"foo\n  bar\"> <span class=\"qux\">";
        let refs = html_selector_refs(html);
        assert_eq!(names(&refs), ["foo", "bar", "qux"]);
    }

    #[test]
    fn continuation_line_numbers() {
        let html = "<div class=\"foo\n  bar\">";
        let refs = html_selector_refs(html);
        assert_eq!(refs[0].line, 0); // foo on line 0
        assert_eq!(refs[1].line, 1); // bar on line 1
    }

    #[test]
    fn extract_linked_css_basic() {
        let html = r#"<link rel="stylesheet" href="styles.css">"#;
        assert_eq!(extract_linked_css(html), vec!["styles.css"]);
    }

    #[test]
    fn extract_linked_css_href_before_rel() {
        let html = r#"<link href="main.css" rel="stylesheet">"#;
        assert_eq!(extract_linked_css(html), vec!["main.css"]);
    }

    #[test]
    fn extract_linked_css_skips_network() {
        let html = r#"<link rel="stylesheet" href="https://cdn.example.com/style.css">"#;
        assert!(extract_linked_css(html).is_empty());
    }

    #[test]
    fn extract_linked_css_skips_absolute_path() {
        let html = r#"<link rel="stylesheet" href="/assets/style.css">"#;
        assert!(extract_linked_css(html).is_empty());
    }

    #[test]
    fn extract_linked_css_strips_query_string() {
        let html = r#"<link rel="stylesheet" href="styles.css?v=123">"#;
        assert_eq!(extract_linked_css(html), vec!["styles.css"]);
    }

    #[test]
    fn extract_linked_css_ignores_non_stylesheet() {
        let html = r#"<link rel="icon" href="favicon.ico">"#;
        assert!(extract_linked_css(html).is_empty());
    }

    #[test]
    fn extract_linked_css_multiple() {
        let html = r#"
            <link rel="stylesheet" href="base.css">
            <link rel="stylesheet" href="theme.css">
            <link rel="icon" href="favicon.ico">
        "#;
        let links = extract_linked_css(html);
        assert_eq!(links.len(), 2);
        assert!(links.contains(&"base.css".to_string()));
        assert!(links.contains(&"theme.css".to_string()));
    }

    // ── Inline <style> blocks ──────────────────────────────────────────────

    #[test]
    fn extract_style_blocks_basic() {
        let html = "<style>.btn { color: red; }</style>";
        let blocks = extract_style_blocks(html);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, 0); // base_line = 0
        assert!(blocks[0].1.contains(".btn"));
    }

    #[test]
    fn extract_style_blocks_line_offset() {
        let html = "<html>\n<head>\n<style>\n.btn { color: red; }\n</style>";
        let blocks = extract_style_blocks(html);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, 2); // <style> opens on line 2, content starts on line 2/3
    }

    #[test]
    fn extract_style_blocks_multiple() {
        let html = "<style>.a{}</style>\n<style>.b{}</style>";
        let blocks = extract_style_blocks(html);
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn extract_style_blocks_none() {
        let html = "<div class=\"foo\"></div>";
        assert!(extract_style_blocks(html).is_empty());
    }

    // ── Unclosed attribute fix ─────────────────────────────────────────────

    #[test]
    fn unclosed_attr_does_not_bleed_into_next_tag() {
        // The class attribute on line 0 has no closing quote.
        // Line 1 is a completely separate element — its class should not be
        // contaminated by the continuation of line 0's attribute.
        let html = "<div class=\"btn\n<p class=\"card\">";
        let refs = html_selector_refs(html);
        // 'btn' is valid; 'card' must be found independently on line 1.
        // '<p class=' must NOT appear as a class name token.
        assert!(refs.iter().all(|r| !r.name.contains('<')));
        assert!(refs.iter().all(|r| !r.name.contains('=')));
        assert!(refs.iter().any(|r| r.name == "card" && r.line == 1));
    }

    #[test]
    fn unclosed_attr_same_line_stops_at_tag_boundary() {
        // If a quote appears inside a following attribute on the same line,
        // e.g. class="foo onclick="handler", only 'foo' should be collected.
        let html = r#"<div class="foo <span class="bar">"#;
        let refs = html_selector_refs(html);
        let names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
        // 'foo' is in the class attribute up to '<'; '<span' should not be a class
        assert!(!names.iter().any(|n| n.contains('<')));
    }

    #[test]
    fn multi_line_attr_still_works_without_intervening_tag() {
        // Genuine multi-line class attribute (no '<' between open and close).
        let html = "<div class=\"foo\n  bar baz\">";
        let refs = html_selector_refs(html);
        assert_eq!(names(&refs), ["foo", "bar", "baz"]);
    }
}
