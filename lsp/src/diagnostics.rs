// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

use std::collections::{HashMap, HashSet};

use lsp_types::{Diagnostic, DiagnosticSeverity, Position, PublishDiagnosticsParams, Range, Url};
use serde_json::json;

use crate::css::{parse_css_content, Source};
use crate::html::{attr_value_extent, extract_style_blocks, html_selector_refs, is_html_uri};
use crate::model::{display_key, line_start, ClassMap, CssIndex, DocumentMap};
use crate::regexes::class_attr_re;

pub fn publish_diagnostics(uri: Url, diagnostics: Vec<Diagnostic>) -> lsp_server::Notification {
    lsp_server::Notification {
        method: "textDocument/publishDiagnostics".to_string(),
        params: serde_json::to_value(PublishDiagnosticsParams {
            uri,
            diagnostics,
            version: None,
        })
        .unwrap_or(json!({})),
    }
}

pub fn all_html_diagnostics(text: &str, html_uri: &Url, class_map: &ClassMap) -> Vec<Diagnostic> {
    let mut diags = diagnostics_for_html(text, html_uri, class_map);
    diags.extend(diagnostics_for_duplicate_classes(text));
    diags
}

fn diagnostics_for_html(text: &str, html_uri: &Url, class_map: &ClassMap) -> Vec<Diagnostic> {
    // Parse inline <style> blocks for any URI scheme (file://, untitled:, etc.).
    let mut inline = CssIndex::default();
    let (source_file, source_path) = match html_uri.to_file_path() {
        Ok(p) => (
            p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string(),
            p.to_string_lossy().into_owned(),
        ),
        Err(_) => {
            let path_str = html_uri.path();
            let file = path_str.rsplit('/').next().unwrap_or("").to_string();
            (file, html_uri.to_string())
        }
    };
    let source = Source { file: &source_file, path: &source_path };
    for (base_line, content) in extract_style_blocks(text) {
        parse_css_content(&content, base_line, source, &mut inline);
    }

    html_selector_refs(text)
        .into_iter()
        .filter_map(|r| {
            let lookup = r.key();
            if class_map.contains_key(&lookup) || inline.classes.contains_key(&lookup) {
                return None;
            }
            let kind = if r.is_id { "id" } else { "class" };
            Some(Diagnostic {
                range: Range {
                    start: Position { line: r.line, character: r.col_start },
                    end: Position { line: r.line, character: r.col_end },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("css-lens".to_string()),
                message: format!("Unknown CSS {kind} '{}'", r.name),
                ..Default::default()
            })
        })
        .collect()
}

fn diagnostics_for_duplicate_classes(text: &str) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    // (quote char, seen names in current attr)
    let mut continuation: Option<(char, HashSet<String>)> = None;

    for (line_num, line) in text.lines().enumerate() {
        let line_num = line_num as u32;
        let mut scan_from = 0usize;

        // Handle continuation of an open multi-line attribute.
        if let Some((quote, ref mut seen)) = continuation {
            let tag_boundary = line.find('<').unwrap_or(line.len());
            let search = &line[..tag_boundary];
            match search.find(quote) {
                Some(close) => {
                    check_dup_tokens(line, line_num, 0, close, seen, &mut diags);
                    scan_from = close + quote.len_utf8();
                    continuation = None;
                }
                None if tag_boundary < line.len() => {
                    check_dup_tokens(line, line_num, 0, tag_boundary, seen, &mut diags);
                    // The '<' terminates the attr; scan the rest of this line for new attrs.
                    scan_from = tag_boundary;
                    continuation = None;
                }
                None => {
                    check_dup_tokens(line, line_num, 0, line.len(), seen, &mut diags);
                    continue; // still open
                }
            }
        }

        // Scan for new class attributes starting at scan_from.
        let shifted = &line[scan_from..];
        for cap in class_attr_re().captures_iter(shifted) {
            let Some(whole) = cap.get(0) else { continue };
            let mut seen: HashSet<String> = HashSet::new();
            let quote = cap[1].chars().next().unwrap_or('"');
            let value_start = scan_from + whole.end();
            let (value_end, open) = attr_value_extent(line, value_start, quote);
            check_dup_tokens(line, line_num, value_start, value_end, &mut seen, &mut diags);
            if open {
                continuation = Some((quote, seen));
            }
        }
    }

    diags
}

fn check_dup_tokens(
    line: &str,
    line_num: u32,
    from: usize,
    to: usize,
    seen: &mut HashSet<String>,
    diags: &mut Vec<Diagnostic>,
) {
    if from >= to { return; }
    let value = &line[from..to];
    let mut tok_start = 0usize;
    let bytes = value.as_bytes();
    for i in 0..=bytes.len() {
        let is_sep = i == bytes.len() || bytes[i] == b' ' || bytes[i] == b'\t';
        if is_sep {
            if tok_start < i {
                let tok = &value[tok_start..i];
                if !tok.is_empty() && !seen.insert(tok.to_string()) {
                    diags.push(Diagnostic {
                        range: Range {
                            start: Position { line: line_num, character: (from + tok_start) as u32 },
                            end: Position { line: line_num, character: (from + i) as u32 },
                        },
                        severity: Some(DiagnosticSeverity::WARNING),
                        source: Some("css-lens".to_string()),
                        message: format!("class '{tok}' is already listed in this attribute"),
                        ..Default::default()
                    });
                }
            }
            tok_start = i + 1;
        }
    }
}

/// Duplicate-selector warnings for every CSS file, keyed by source path: each
/// definition of a name after the first one in the same file is flagged.
pub fn diagnostics_for_css_duplicates(class_map: &ClassMap) -> HashMap<String, Vec<Diagnostic>> {
    let mut out: HashMap<String, Vec<Diagnostic>> = HashMap::new();

    for (name, infos) in class_map {
        let mut seen_in: HashSet<&str> = HashSet::new();
        for info in infos {
            if seen_in.insert(info.source_path.as_str()) {
                continue;
            }
            out.entry(info.source_path.clone()).or_default().push(Diagnostic {
                range: line_start(info.definition_line),
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("css-lens".to_string()),
                message: format!("'{}' is already defined earlier in this file", display_key(name)),
                ..Default::default()
            });
        }
    }

    out
}

/// Returns hint-level diagnostics, keyed by CSS source path, for selectors that
/// are not referenced in any currently-open HTML document nor in `js_refs`.
/// They are intentionally soft — classes added by other means will appear unused.
pub fn diagnostics_for_unused<'a>(
    class_map: &ClassMap,
    documents: &DocumentMap,
    js_refs: impl IntoIterator<Item = &'a String>,
) -> HashMap<String, Vec<Diagnostic>> {
    // Collect all selectors used in open HTML documents.
    let mut used: HashSet<String> = HashSet::new();
    for (uri, text) in documents {
        if !is_html_uri(uri) { continue; }
        used.extend(html_selector_refs(text).iter().map(|r| r.key()));
    }

    // If no HTML files are open, don't emit any hints — we have no evidence.
    if used.is_empty() { return HashMap::new(); }

    // Suppress hints for classes/IDs referenced in vanilla JS (classList.add,
    // querySelector, getElementsByClassName, etc.) so DOM manipulation in plain
    // .js files doesn't produce false unused-selector warnings.
    used.extend(js_refs.into_iter().cloned());

    let mut out: HashMap<String, Vec<Diagnostic>> = HashMap::new();

    for (name, infos) in class_map {
        if used.contains(name) { continue; }
        let display = display_key(name);

        for info in infos {
            out.entry(info.source_path.clone()).or_default().push(Diagnostic {
                range: line_start(info.definition_line),
                severity: Some(DiagnosticSeverity::HINT),
                source: Some("css-lens".to_string()),
                message: format!("'{display}' is not used in any open HTML file"),
                ..Default::default()
            });
        }
    }

    out
}
