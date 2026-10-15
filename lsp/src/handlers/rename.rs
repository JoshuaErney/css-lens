use std::collections::HashMap;
use std::fs;
use std::path::Path;

use lsp_server::{ErrorCode, RequestId, Response};
use lsp_types::{Position, Range, RenameParams, TextEdit, Url, WorkspaceEdit};
use serde_json::json;

use super::attr_key_at;
use crate::css::is_css_path;
use crate::cursor::{is_ident_byte, is_valid_css_ident, word_at};
use crate::model::ClassMap;
use crate::server::ServerState;
use crate::workspace::workspace_html_refs;

/// When the cursor is on a selector token inside a CSS file, returns the class_map
/// key for that token (bare name for classes, `#name` for IDs). Returns None when
/// the cursor is not on a known selector definition at that line.
fn css_selector_at_cursor(
    uri: &Url,
    text: &str,
    pos: Position,
    class_map: &ClassMap,
) -> Option<String> {
    let path = uri.to_file_path().ok()?;
    if !is_css_path(&path) {
        return None;
    }
    let source_path = path.to_string_lossy().into_owned();
    let bare = word_at(text, pos)?;
    let defined_here = |key: &str| {
        class_map.get(key).is_some_and(|infos| {
            infos.iter().any(|i| i.source_path == source_path && i.definition_line == pos.line)
        })
    };

    if defined_here(&bare) {
        return Some(bare);
    }
    let id_key = format!("#{bare}");
    defined_here(&id_key).then_some(id_key)
}

pub fn rename(id: RequestId, params: RenameParams, state: &ServerState) -> Response {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;
    let new_name = &params.new_name;

    if !is_valid_css_ident(new_name) {
        return Response::new_err(
            id,
            ErrorCode::InvalidParams as i32,
            format!("'{new_name}' is not a valid CSS identifier"),
        );
    }

    let text = match state.documents.get(uri) {
        Some(t) => t,
        None => return Response::new_ok(id, json!(null)),
    };

    let lookup_key = match attr_key_at(text, pos)
        .or_else(|| css_selector_at_cursor(uri, text, pos, &state.css.classes))
    {
        Some(k) => k,
        None => return Response::new_ok(id, json!(null)),
    };

    let (old_bare, prefix_char) = match lookup_key.strip_prefix('#') {
        Some(bare) => (bare, '#'),
        None => (lookup_key.as_str(), '.'),
    };
    let old_pattern = format!("{prefix_char}{old_bare}");
    let new_pattern = format!("{prefix_char}{new_name}");

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();

    // CSS edits: replace selector on each definition line.
    if let Some(infos) = state.css.classes.get(&lookup_key) {
        for info in infos {
            let css_path = Path::new(&info.source_path);
            let css_uri = match Url::from_file_path(css_path) {
                Ok(u) => u,
                Err(_) => continue,
            };
            let content = match fs::read_to_string(css_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let line_text = match content.lines().nth(info.definition_line as usize) {
                Some(l) => l,
                None => continue,
            };
            // Replace the first occurrence on the selector line (before any `{`).
            let selector_part = line_text.split('{').next().unwrap_or(line_text);
            if let Some(col) = selector_part.find(&old_pattern) {
                // Guard: ensure the match is a complete token, not a prefix of a
                // longer name (e.g. renaming .btn must not touch .btn-primary).
                let after = col + old_pattern.len();
                if selector_part.as_bytes().get(after).is_some_and(|&b| is_ident_byte(b)) {
                    continue;
                }
                changes.entry(css_uri).or_default().push(TextEdit {
                    range: Range {
                        start: Position {
                            line: info.definition_line,
                            character: col as u32,
                        },
                        end: Position {
                            line: info.definition_line,
                            character: after as u32,
                        },
                    },
                    new_text: new_pattern.clone(),
                });
            }
        }
    }

    // HTML edits: rename every attribute token across all workspace HTML files.
    if let Some(root) = state.root.as_deref() {
        for r in workspace_html_refs(&lookup_key, root, &state.documents) {
            // workspace_html_refs already filters to exact matches; each location is
            // one token in an attribute value.
            changes.entry(r.uri).or_default().push(TextEdit {
                range: r.range,
                new_text: new_name.clone(),
            });
        }
    }

    let edit = WorkspaceEdit { changes: Some(changes), ..Default::default() };
    Response::new_ok(id, serde_json::to_value(edit).unwrap_or(json!(null)))
}
