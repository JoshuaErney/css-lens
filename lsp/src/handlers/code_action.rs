use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use lsp_server::{RequestId, Response};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, DiagnosticSeverity, Position, Range,
    TextEdit, Url, WorkspaceEdit,
};
use serde_json::json;

use crate::cursor::is_valid_css_ident;
use crate::css::find_rule_extent;
use crate::html::{is_reachable, reachable_css_paths};
use crate::model::{display_key, ClassMap, DocumentMap};
use crate::server::ServerState;

pub fn code_action(id: RequestId, params: CodeActionParams, state: &ServerState) -> Response {
    let class_map = &state.css.classes;
    let documents = &state.documents;
    let html_uri = &params.text_document.uri;
    let mut actions: Vec<CodeActionOrCommand> = Vec::new();

    for diag in &params.context.diagnostics {
        if diag.source.as_deref() != Some("css-lens") {
            continue;
        }
        if diag.severity != Some(DiagnosticSeverity::ERROR) {
            continue;
        }

        // Extract selector name from message: "Unknown CSS class 'foo'" or "Unknown CSS id 'bar'".
        let msg = &diag.message;
        let is_id = msg.contains("Unknown CSS id");
        // Guard against CSS injection — only proceed if the name is a safe identifier.
        let name = match extract_quoted(msg) {
            Some(n) if is_valid_css_ident(&n) => n,
            _ => continue,
        };

        let target_css = match find_target_css_file(html_uri, class_map, documents) {
            Some(p) => p,
            None => continue,
        };
        let css_uri = match Url::from_file_path(&target_css) {
            Ok(u) => u,
            Err(_) => continue,
        };

        let css_content = fs::read_to_string(&target_css).unwrap_or_default();
        let last_line = css_content.lines().count() as u32;
        let needs_newline = !css_content.ends_with('\n');
        let prefix = if is_id { "#" } else { "." };
        let new_rule = format!(
            "{}{prefix}{name} {{\n  \n}}\n",
            if needs_newline { "\n" } else { "" }
        );
        let file_name = target_css
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("CSS file");

        let action = CodeAction {
            title: format!(
                "Create CSS {}{}{name} in {file_name}",
                if is_id { "id " } else { "class " },
                prefix
            ),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diag.clone()]),
            is_preferred: Some(true),
            edit: Some(WorkspaceEdit {
                changes: Some(HashMap::from([(
                    css_uri,
                    vec![TextEdit {
                        range: Range {
                            start: Position { line: last_line, character: 0 },
                            end: Position { line: last_line, character: 0 },
                        },
                        new_text: new_rule,
                    }],
                )])),
                ..Default::default()
            }),
            ..Default::default()
        };
        actions.push(CodeActionOrCommand::CodeAction(action));
    }

    // ── Remove unused rule (HINT diagnostics, triggered from a CSS file) ───
    //
    // The diagnostics passed here are only those overlapping the cursor range,
    // so `hinted_lines` is naturally scoped to the lines the user is acting on.
    let mut hinted_lines: HashSet<u32> = HashSet::new();
    let mut hint_keys: HashSet<String> = HashSet::new();
    for d in params.context.diagnostics.iter().filter(|d| {
        d.source.as_deref() == Some("css-lens") && d.severity == Some(DiagnosticSeverity::HINT)
    }) {
        hinted_lines.insert(d.range.start.line);
        if let Some(display) = extract_quoted(&d.message) {
            // class_map stores classes without '.' and IDs with '#'.
            let key = match display.strip_prefix('.') {
                Some(bare) => bare.to_string(),
                None => display,
            };
            hint_keys.insert(key);
        }
    }

    if !hint_keys.is_empty() {
        // Group hinted keys by (source_path, definition_line), restricted to
        // lines that actually have a HINT diagnostic in this request.
        let mut rule_blocks: HashMap<(String, u32), HashSet<String>> = HashMap::new();
        for key in &hint_keys {
            if let Some(infos) = class_map.get(key.as_str()) {
                for info in infos {
                    if hinted_lines.contains(&info.definition_line) {
                        rule_blocks
                            .entry((info.source_path.clone(), info.definition_line))
                            .or_default()
                            .insert(key.clone());
                    }
                }
            }
        }

        for ((source_path, def_line), hinted_at_block) in &rule_blocks {
            // Guard: if any co-selector at this (source_path, def_line) is still
            // in use (no HINT for it), we must not delete the shared rule block.
            let all_unused = class_map.iter().all(|(key, infos)| {
                let at_block = infos
                    .iter()
                    .any(|i| i.source_path == *source_path && i.definition_line == *def_line);
                !at_block || hint_keys.contains(key.as_str())
            });
            if !all_unused {
                continue;
            }

            let css_path = Path::new(source_path);
            let css_uri = match Url::from_file_path(css_path) {
                Ok(u) => u,
                Err(_) => continue,
            };
            // Prefer the editor's live buffer so def_line stays in sync with
            // the WorkspaceEdit target even when the CSS file has unsaved edits.
            let content = if let Some(t) = documents.get(&css_uri) {
                t.clone()
            } else {
                match fs::read_to_string(css_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                }
            };
            let (start_line, end_line) = match find_rule_extent(&content, *def_line) {
                Some(e) => e,
                None => continue,
            };

            // Clamp to a valid end position so the edit range is never out of bounds.
            let total_lines = content.lines().count() as u32;
            let (end_edit_line, end_edit_char) = if end_line + 1 < total_lines {
                (end_line + 1, 0)
            } else {
                let last_len = content.lines().last().map(|l| l.len() as u32).unwrap_or(0);
                (end_line, last_len)
            };

            let file_name = css_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("CSS file");

            let mut selectors: Vec<String> = hinted_at_block.iter().map(|k| display_key(k)).collect();
            selectors.sort();
            let display = selectors.join(", ");

            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Remove unused rule '{display}' from {file_name}"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: None,
                is_preferred: Some(false),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(
                        css_uri,
                        vec![TextEdit {
                            range: Range {
                                start: Position { line: start_line, character: 0 },
                                end: Position { line: end_edit_line, character: end_edit_char },
                            },
                            new_text: String::new(),
                        }],
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }
    }

    Response::new_ok(id, serde_json::to_value(actions).unwrap_or(json!([])))
}

/// Extracts the first single-quoted token from a message like `"Unknown CSS class 'btn'"`.
/// Using find twice (not rfind) ensures we always get the FIRST quoted span,
/// so messages with multiple quoted tokens don't produce garbage extractions.
fn extract_quoted(msg: &str) -> Option<String> {
    let start = msg.find('\'')? + 1;
    let end = start + msg[start..].find('\'')?;
    if end <= start { return None; }
    Some(msg[start..end].to_string())
}

/// Returns the CSS file path that is most likely the right target for a new
/// rule. Preference order:
///   1. A CSS file linked from the HTML and in the same directory
///   2. Any CSS file linked from the HTML
///   3. Any CSS file in the same directory (legacy fallback)
///   4. Any known CSS file
fn find_target_css_file(
    html_uri: &Url,
    class_map: &ClassMap,
    documents: &DocumentMap,
) -> Option<PathBuf> {
    let html_path = html_uri.to_file_path().ok()?;
    let html_dir = html_path.parent()?.to_path_buf();

    let css_paths: HashSet<&str> = class_map
        .values()
        .flat_map(|v| v.iter().map(|i| i.source_path.as_str()))
        .collect();

    // Prefer a CSS file this HTML explicitly links to.
    if let Some(text) = documents.get(html_uri) {
        let reachable = reachable_css_paths(&html_path, text);
        if !reachable.is_empty() {
            for s in &css_paths {
                let p = Path::new(s);
                if p.parent() == Some(html_dir.as_path()) && is_reachable(s, &reachable) {
                    return Some(p.to_path_buf());
                }
            }
            for s in &css_paths {
                if is_reachable(s, &reachable) {
                    return Some(PathBuf::from(s));
                }
            }
        }
    }

    // Fall back to same directory.
    for s in &css_paths {
        let p = Path::new(s);
        if p.parent() == Some(html_dir.as_path()) {
            return Some(p.to_path_buf());
        }
    }

    // Last resort: any known CSS file.
    css_paths.iter().next().map(PathBuf::from)
}
