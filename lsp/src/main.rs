use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use lsp_server::{Connection, Message, RequestId, Response};
use lsp_types::*;
use regex::Regex;
use serde_json::{json, Value};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Static regexes — compiled once, reused across every request and file parse.
// ---------------------------------------------------------------------------

fn pseudo_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"::?[a-zA-Z][a-zA-Z0-9_-]*(?:\([^)]*\))?").unwrap())
}

fn class_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.[a-zA-Z][a-zA-Z0-9_-]*").unwrap())
}

fn id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"#[a-zA-Z][a-zA-Z0-9_-]*").unwrap())
}

fn class_attr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)class\s*=\s*(["'])"#).unwrap())
}

fn id_attr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\bid\s*=\s*(["'])"#).unwrap())
}

fn style_attr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\bstyle\s*=\s*(["'])"#).unwrap())
}

fn import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"@import\s+(?:url\()?["']([^"']+)["']"#).unwrap())
}

// Matches common CSS color values for display in hover tooltips.
fn color_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)#[0-9a-f]{3,8}\b|rgba?\s*\([^)]+\)|hsla?\s*\([^)]+\)").unwrap()
    })
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ClassInfo {
    properties: String,
    selector: String,
    media_query: Option<String>,
    source_file: String,
    source_path: String,
    definition_line: u32,
}

/// CSS custom properties map: `"--name"` → `"value"`.
type VarMap = HashMap<String, String>;
type ClassMap = HashMap<String, Vec<ClassInfo>>;
type DocumentMap = HashMap<Url, String>;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (connection, io_threads) = Connection::stdio();

    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![
                " ".to_string(),
                "\"".to_string(),
                "'".to_string(),
            ]),
            resolve_provider: Some(false),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        ..Default::default()
    };

    let (init_id, init_params_raw) = connection.initialize_start()?;
    let init_params: InitializeParams = serde_json::from_value(init_params_raw)?;

    let root_path: Option<PathBuf> = init_params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .and_then(|folder| folder.uri.to_file_path().ok())
        .or_else(|| {
            #[allow(deprecated)]
            init_params.root_uri.as_ref().and_then(|u| u.to_file_path().ok())
        });

    let mut class_map: ClassMap = HashMap::new();
    if let Some(ref root) = root_path {
        scan_directory(root, &mut class_map);
    }
    let mut var_map: VarMap = refresh_var_map(&class_map);

    let init_result = InitializeResult {
        capabilities,
        server_info: Some(ServerInfo {
            name: "css-class-mapper-lsp".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    };
    connection.initialize_finish(init_id, serde_json::to_value(init_result)?)?;

    let mut documents: DocumentMap = HashMap::new();

    loop {
        let msg = match connection.receiver.recv() {
            Ok(m) => m,
            Err(_) => break,
        };

        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    break;
                }
                let resp = route_request(
                    req.id.clone(),
                    &req.method,
                    &req.params,
                    &class_map,
                    &var_map,
                    &documents,
                    root_path.as_deref(),
                );
                connection.sender.send(Message::Response(resp))?;
            }
            Message::Notification(notif) => {
                let outgoing = route_notification(
                    &notif.method,
                    notif.params,
                    &mut class_map,
                    &mut var_map,
                    &mut documents,
                );
                for n in outgoing {
                    connection.sender.send(Message::Notification(n))?;
                }
            }
            Message::Response(_) => {}
        }
    }

    io_threads.join()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Request routing
// ---------------------------------------------------------------------------

fn route_request(
    id: RequestId,
    method: &str,
    params: &Value,
    class_map: &ClassMap,
    var_map: &VarMap,
    documents: &DocumentMap,
    root_path: Option<&Path>,
) -> Response {
    match method {
        "textDocument/completion" => {
            match serde_json::from_value::<CompletionParams>(params.clone()) {
                Ok(p) => completion_handler(id, p, class_map, var_map, documents),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        "textDocument/hover" => {
            match serde_json::from_value::<HoverParams>(params.clone()) {
                Ok(p) => hover_handler(id, p, class_map, var_map, documents),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        "textDocument/definition" => {
            match serde_json::from_value::<GotoDefinitionParams>(params.clone()) {
                Ok(p) => definition_handler(id, p, class_map, documents),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        "textDocument/references" => {
            match serde_json::from_value::<ReferenceParams>(params.clone()) {
                Ok(p) => references_handler(id, p, class_map, documents, root_path),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        "textDocument/rename" => {
            match serde_json::from_value::<RenameParams>(params.clone()) {
                Ok(p) => rename_handler(id, p, class_map, documents, root_path),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        "textDocument/codeAction" => {
            match serde_json::from_value::<CodeActionParams>(params.clone()) {
                Ok(p) => code_action_handler(id, p, class_map),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        _ => Response::new_err(id, -32601, format!("method not found: {method}")),
    }
}

// ---------------------------------------------------------------------------
// Notification routing
// ---------------------------------------------------------------------------

fn route_notification(
    method: &str,
    params: Value,
    class_map: &mut ClassMap,
    var_map: &mut VarMap,
    documents: &mut DocumentMap,
) -> Vec<lsp_server::Notification> {
    let mut out: Vec<lsp_server::Notification> = Vec::new();

    match method {
        "textDocument/didOpen" => {
            if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(params) {
                let uri = p.text_document.uri;
                let text = p.text_document.text;
                documents.insert(uri.clone(), text.clone());
                if is_html_uri(&uri) {
                    out.push(publish_diagnostics(uri, diagnostics_for_html(&text, class_map)));
                }
            }
        }
        "textDocument/didChange" => {
            if let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(params) {
                if let Some(last) = p.content_changes.into_iter().last() {
                    let uri = p.text_document.uri;
                    let text = last.text;
                    documents.insert(uri.clone(), text.clone());
                    if is_html_uri(&uri) {
                        out.push(publish_diagnostics(uri, diagnostics_for_html(&text, class_map)));
                    }
                }
            }
        }
        "textDocument/didClose" => {
            if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(params) {
                let uri = p.text_document.uri;
                documents.remove(&uri);
                out.push(publish_diagnostics(uri, vec![]));
            }
        }
        "workspace/didChangeWatchedFiles" => {
            if let Ok(p) = serde_json::from_value::<DidChangeWatchedFilesParams>(params) {
                let affected: Vec<(PathBuf, FileChangeType)> = p
                    .changes
                    .iter()
                    .filter_map(|c| {
                        let path = c.uri.to_file_path().ok()?;
                        if path.extension().map_or(false, |e| e == "css") {
                            Some((path, c.typ))
                        } else {
                            None
                        }
                    })
                    .collect();

                update_css_map(&p, class_map);
                *var_map = refresh_var_map(class_map);

                for (path, typ) in &affected {
                    if let Ok(uri) = Url::from_file_path(path) {
                        let diags = if *typ == FileChangeType::DELETED {
                            vec![]
                        } else {
                            diagnostics_for_css_duplicates(class_map, &path.to_string_lossy())
                        };
                        out.push(publish_diagnostics(uri, diags));
                    }
                }

                // Unused-selector hints for CSS files (based on open HTML docs).
                for (css_uri, diags) in diagnostics_for_unused(class_map, documents) {
                    out.push(publish_diagnostics(css_uri, diags));
                }

                // Refresh HTML diagnostics since the class map changed.
                let html_diags: Vec<_> = documents
                    .iter()
                    .filter(|(uri, _)| is_html_uri(uri))
                    .map(|(uri, text)| {
                        publish_diagnostics(uri.clone(), diagnostics_for_html(text, class_map))
                    })
                    .collect();
                out.extend(html_diags);
            }
        }
        _ => {}
    }

    out
}

// ---------------------------------------------------------------------------
// LSP handlers
// ---------------------------------------------------------------------------

fn completion_handler(
    id: RequestId,
    params: CompletionParams,
    class_map: &ClassMap,
    var_map: &VarMap,
    documents: &DocumentMap,
) -> Response {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;

    let text = match documents.get(uri) {
        Some(t) => t,
        None => return Response::new_ok(id, json!(null)),
    };

    // class="..." — offer class names
    if in_class_attribute(text, pos) {
        let (existing_classes, prefix) = completion_context(text, pos);
        let prefix_lower = prefix.to_lowercase();
        // Compute line/col once; build_insert_text no longer calls cursor_context
        // per candidate (which was O(candidates × pos.line) work).
        let (ins_line, ins_col) = cursor_context(text, pos)
            .map(|(_, l, c)| (l, c))
            .unwrap_or(("", 0));

        let mut items: Vec<CompletionItem> = class_map
            .iter()
            .filter(|(name, _)| !name.starts_with('#'))
            .filter(|(name, _)| {
                let name_lower = name.to_lowercase();
                !existing_classes.contains(&name_lower)
                    && (prefix.is_empty() || name_lower.starts_with(&prefix_lower))
            })
            .map(|(name, infos)| {
                let insert = build_insert_text(name, ins_line, ins_col);
                let detail = infos.first().map(|i| i.source_file.clone());
                CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail,
                    insert_text: Some(insert),
                    filter_text: Some(name.to_lowercase()),
                    ..Default::default()
                }
            })
            .collect();

        items.sort_by(|a, b| a.label.cmp(&b.label));
        return Response::new_ok(
            id,
            serde_json::to_value(CompletionResponse::Array(items)).unwrap_or(json!(null)),
        );
    }

    // id="..." — offer ID names (single-value guard)
    if in_id_attribute(text, pos) {
        let (has_existing_value, prefix) = id_completion_context(text, pos);
        if has_existing_value {
            return Response::new_ok(id, json!(null));
        }
        let prefix_lower = prefix.to_lowercase();

        let mut items: Vec<CompletionItem> = class_map
            .iter()
            .filter(|(name, _)| name.starts_with('#'))
            .filter(|(name, _)| {
                let bare_lower = name[1..].to_lowercase();
                prefix.is_empty() || bare_lower.starts_with(&prefix_lower)
            })
            .map(|(name, infos)| {
                let bare = name[1..].to_string();
                let detail = infos.first().map(|i| i.source_file.clone());
                CompletionItem {
                    label: bare.clone(),
                    kind: Some(CompletionItemKind::VALUE),
                    detail,
                    insert_text: Some(bare.clone()),
                    filter_text: Some(bare.to_lowercase()),
                    ..Default::default()
                }
            })
            .collect();

        items.sort_by(|a, b| a.label.cmp(&b.label));
        return Response::new_ok(
            id,
            serde_json::to_value(CompletionResponse::Array(items)).unwrap_or(json!(null)),
        );
    }

    // style="..." — offer CSS custom properties when prefix starts with `--`
    if in_style_attribute(text, pos) {
        let prefix = style_prefix(text, pos);
        if prefix.starts_with("--") {
            let prefix_lower = prefix.to_lowercase();
            let mut items: Vec<CompletionItem> = var_map
                .iter()
                .filter(|(name, _)| name.to_lowercase().starts_with(&prefix_lower))
                .map(|(name, value)| CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(value.clone()),
                    insert_text: Some(name.clone()),
                    filter_text: Some(name.to_lowercase()),
                    ..Default::default()
                })
                .collect();
            items.sort_by(|a, b| a.label.cmp(&b.label));
            return Response::new_ok(
                id,
                serde_json::to_value(CompletionResponse::Array(items)).unwrap_or(json!(null)),
            );
        }
    }

    Response::new_ok(id, json!(null))
}

fn hover_handler(
    id: RequestId,
    params: HoverParams,
    class_map: &ClassMap,
    var_map: &VarMap,
    documents: &DocumentMap,
) -> Response {
    let uri = &params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;

    let text = match documents.get(uri) {
        Some(t) => t,
        None => return Response::new_ok(id, json!(null)),
    };

    let (before, line, col) = match cursor_context(text, pos) {
        Some(ctx) => ctx,
        None => return Response::new_ok(id, json!(null)),
    };

    // style="..." — hover over a CSS variable name (--foo)
    if in_attr_before(&before, style_attr_re()) {
        let word = match word_at_ctx(line, col) {
            Some(w) if w.starts_with("--") => w,
            _ => return Response::new_ok(id, json!(null)),
        };
        let value = match var_map.get(&word) {
            Some(v) => v,
            None => return Response::new_ok(id, json!(null)),
        };
        let hover = Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("**{}**\n\n```css\n{}: {};\n```", word, word, value),
            }),
            range: None,
        };
        return Response::new_ok(id, serde_json::to_value(hover).unwrap_or(json!(null)));
    }

    let lookup_key = if in_attr_before(&before, class_attr_re()) {
        match word_at_ctx(line, col) {
            Some(w) => w,
            None => return Response::new_ok(id, json!(null)),
        }
    } else if in_attr_before(&before, id_attr_re()) {
        match word_at_ctx(line, col) {
            Some(w) => format!("#{w}"),
            None => return Response::new_ok(id, json!(null)),
        }
    } else {
        return Response::new_ok(id, json!(null));
    };

    let infos = match class_map.get(&lookup_key) {
        Some(v) if !v.is_empty() => v,
        _ => return Response::new_ok(id, json!(null)),
    };

    let markdown = if infos.len() == 1 {
        let info = &infos[0];
        let mq = info
            .media_query
            .as_deref()
            .map(|mq| format!("\n_inside_ `{mq}`"))
            .unwrap_or_default();
        let (a, b, c) = specificity(&info.selector);
        let colors = color_summary(&info.properties);
        let color_line = if colors.is_empty() {
            String::new()
        } else {
            format!("\n\nColors: {colors}")
        };
        format!(
            "**{}** — {}:{}{}\n\nSpecificity: `({a},{b},{c})`{color_line}\n\n```css\n{} {{\n{}\n}}\n```",
            lookup_key,
            info.source_file,
            info.definition_line + 1,
            mq,
            info.selector,
            info.properties,
        )
    } else {
        let mut parts = vec![format!("**{}** — {} definitions\n", lookup_key, infos.len())];
        for (i, info) in infos.iter().enumerate() {
            let mq = info
                .media_query
                .as_deref()
                .map(|mq| format!(" _(inside `{mq}`)_"))
                .unwrap_or_default();
            let (a, b, c) = specificity(&info.selector);
            parts.push(format!(
                "**{}.** {}:{}{} — Specificity: `({a},{b},{c})`\n```css\n{} {{\n{}\n}}\n```",
                i + 1,
                info.source_file,
                info.definition_line + 1,
                mq,
                info.selector,
                info.properties,
            ));
        }
        parts.join("\n\n")
    };

    let hover = Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: None,
    };

    Response::new_ok(id, serde_json::to_value(hover).unwrap_or(json!(null)))
}

fn definition_handler(
    id: RequestId,
    params: GotoDefinitionParams,
    class_map: &ClassMap,
    documents: &DocumentMap,
) -> Response {
    let uri = &params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;

    let text = match documents.get(uri) {
        Some(t) => t,
        None => return Response::new_ok(id, json!(null)),
    };

    let lookup_key = if in_class_attribute(text, pos) {
        match word_at(text, pos) {
            Some(w) => w,
            None => return Response::new_ok(id, json!(null)),
        }
    } else if in_id_attribute(text, pos) {
        match word_at(text, pos) {
            Some(w) => format!("#{w}"),
            None => return Response::new_ok(id, json!(null)),
        }
    } else {
        return Response::new_ok(id, json!(null));
    };

    let infos = match class_map.get(&lookup_key) {
        Some(v) if !v.is_empty() => v,
        _ => return Response::new_ok(id, json!(null)),
    };

    let locations: Vec<Location> = infos
        .iter()
        .filter_map(|info| {
            let uri = Url::from_file_path(&info.source_path).ok()?;
            Some(Location {
                uri,
                range: Range {
                    start: Position { line: info.definition_line, character: 0 },
                    end: Position { line: info.definition_line, character: 0 },
                },
            })
        })
        .collect();

    let result = match locations.len() {
        0 => json!(null),
        1 => serde_json::to_value(GotoDefinitionResponse::Scalar(
            locations.into_iter().next().unwrap(),
        ))
        .unwrap_or(json!(null)),
        _ => serde_json::to_value(GotoDefinitionResponse::Array(locations))
            .unwrap_or(json!(null)),
    };
    Response::new_ok(id, result)
}

fn references_handler(
    id: RequestId,
    params: ReferenceParams,
    _class_map: &ClassMap,
    documents: &DocumentMap,
    root_path: Option<&Path>,
) -> Response {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;

    let text = match documents.get(uri) {
        Some(t) => t,
        None => return Response::new_ok(id, json!([])),
    };

    let lookup_key = if in_class_attribute(text, pos) {
        match word_at(text, pos) {
            Some(w) => w,
            None => return Response::new_ok(id, json!([])),
        }
    } else if in_id_attribute(text, pos) {
        match word_at(text, pos) {
            Some(w) => format!("#{w}"),
            None => return Response::new_ok(id, json!([])),
        }
    } else {
        return Response::new_ok(id, json!([]));
    };

    let root = match root_path {
        Some(r) => r,
        None => return Response::new_ok(id, json!([])),
    };

    let locations = workspace_html_refs(&lookup_key, root, documents);
    Response::new_ok(id, serde_json::to_value(locations).unwrap_or(json!([])))
}

fn rename_handler(
    id: RequestId,
    params: RenameParams,
    class_map: &ClassMap,
    documents: &DocumentMap,
    root_path: Option<&Path>,
) -> Response {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;
    let new_name = &params.new_name;

    if !is_valid_css_ident(new_name) {
        return Response::new_err(
            id,
            -32602,
            format!("'{new_name}' is not a valid CSS identifier"),
        );
    }

    let text = match documents.get(uri) {
        Some(t) => t,
        None => return Response::new_ok(id, json!(null)),
    };

    let lookup_key = if in_class_attribute(text, pos) {
        match word_at(text, pos) {
            Some(w) => w,
            None => return Response::new_ok(id, json!(null)),
        }
    } else if in_id_attribute(text, pos) {
        match word_at(text, pos) {
            Some(w) => format!("#{w}"),
            None => return Response::new_ok(id, json!(null)),
        }
    } else {
        return Response::new_ok(id, json!(null));
    };

    let is_id = lookup_key.starts_with('#');
    let old_bare = if is_id { &lookup_key[1..] } else { &lookup_key };
    let prefix_char = if is_id { '#' } else { '.' };
    let old_pattern = format!("{prefix_char}{old_bare}");
    let new_pattern = format!("{prefix_char}{new_name}");

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();

    // CSS edits: replace selector on each definition line.
    if let Some(infos) = class_map.get(&lookup_key) {
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
                            character: (col + old_pattern.len()) as u32,
                        },
                    },
                    new_text: new_pattern.clone(),
                });
            }
        }
    }

    // HTML edits: rename every attribute token across all workspace HTML files.
    let root = match root_path {
        Some(r) => r,
        None => {
            let edit = WorkspaceEdit { changes: Some(changes), ..Default::default() };
            return Response::new_ok(id, serde_json::to_value(edit).unwrap_or(json!(null)));
        }
    };

    for r in workspace_html_refs(&lookup_key, root, documents) {
        // workspace_html_refs already filters to exact matches; each location is
        // one token in an attribute value.
        changes.entry(r.uri.clone()).or_default().push(TextEdit {
            range: r.range,
            new_text: new_name.clone(),
        });
    }

    let edit = WorkspaceEdit { changes: Some(changes), ..Default::default() };
    Response::new_ok(id, serde_json::to_value(edit).unwrap_or(json!(null)))
}

fn code_action_handler(
    id: RequestId,
    params: CodeActionParams,
    class_map: &ClassMap,
) -> Response {
    let html_uri = &params.text_document.uri;
    let mut actions: Vec<CodeActionOrCommand> = Vec::new();

    for diag in &params.context.diagnostics {
        if diag.source.as_deref() != Some("css-class-mapper") {
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

        let target_css = match find_target_css_file(html_uri, class_map) {
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

    Response::new_ok(id, serde_json::to_value(actions).unwrap_or(json!([])))
}

// ---------------------------------------------------------------------------
// Context detection
// ---------------------------------------------------------------------------

fn in_class_attribute(text: &str, pos: Position) -> bool {
    in_attr(text, pos, class_attr_re())
}

fn in_id_attribute(text: &str, pos: Position) -> bool {
    in_attr(text, pos, id_attr_re())
}

fn in_style_attribute(text: &str, pos: Position) -> bool {
    in_attr(text, pos, style_attr_re())
}

fn in_attr_before(before: &str, attr_re: &Regex) -> bool {
    if let Some(m) = attr_re.captures_iter(before).last() {
        let quote = m[1].chars().next().unwrap_or('"');
        !before[m.get(0).unwrap().end()..].contains(quote)
    } else {
        false
    }
}

fn in_attr(text: &str, pos: Position, attr_re: &Regex) -> bool {
    let (before, _, _) = match cursor_context(text, pos) {
        Some(ctx) => ctx,
        None => return false,
    };
    in_attr_before(&before, attr_re)
}

/// Converts a UTF-16 code-unit offset (as sent by LSP clients) to a UTF-8 byte
/// offset into `line`. The LSP spec mandates UTF-16 column counts; naively using
/// them as byte indices panics on any line containing multi-byte UTF-8 characters.
fn utf16_offset_to_byte(line: &str, utf16_col: usize) -> usize {
    let mut u16_count = 0usize;
    for (byte_idx, c) in line.char_indices() {
        if u16_count >= utf16_col {
            return byte_idx;
        }
        u16_count += c.len_utf16();
    }
    line.len()
}

/// Returns `(text_before_cursor, current_line, col)` in a single pass over
/// `text.lines()`. Shared by all position-dependent helpers to avoid building
/// the "before" string redundantly in each one.
fn cursor_context<'a>(text: &'a str, pos: Position) -> Option<(String, &'a str, usize)> {
    let line_idx = pos.line as usize;
    let mut before = String::new();
    for (i, l) in text.lines().enumerate() {
        if i < line_idx {
            before.push_str(l);
            before.push('\n');
        } else {
            let col = utf16_offset_to_byte(l, pos.character as usize);
            before.push_str(&l[..col]);
            return Some((before, l, col));
        }
    }
    None
}

/// Returns the CSS identifier fragment immediately before the cursor within a
/// `style="..."` attribute value. Used to detect `--variable` prefixes.
fn style_prefix(text: &str, pos: Position) -> String {
    let (before, _, _) = match cursor_context(text, pos) {
        Some(ctx) => ctx,
        None => return String::new(),
    };
    let last_match = match style_attr_re().captures_iter(&before).last() {
        Some(m) => m,
        None => return String::new(),
    };
    extract_prefix(&before[last_match.get(0).unwrap().end()..])
}

fn completion_context(text: &str, pos: Position) -> (HashSet<String>, String) {
    let (before, line, col) = match cursor_context(text, pos) {
        Some(ctx) => ctx,
        None => return (HashSet::new(), String::new()),
    };

    let last_match = match class_attr_re().captures_iter(&before).last() {
        Some(m) => m,
        None => return (HashSet::new(), String::new()),
    };

    let quote = last_match[1].chars().next().unwrap_or('"');
    let attr_value_start = last_match.get(0).unwrap().end();
    let value_before_cursor = &before[attr_value_start..];
    let prefix = extract_prefix(value_before_cursor);
    let value_after = line[col..].splitn(2, quote).next().unwrap_or("");
    let full_value = format!("{value_before_cursor}{value_after}");

    let prefix_lower = prefix.to_lowercase();
    let existing: HashSet<String> = full_value
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| s != &prefix_lower)
        .collect();

    (existing, prefix)
}

fn id_completion_context(text: &str, pos: Position) -> (bool, String) {
    let (before, line, col) = match cursor_context(text, pos) {
        Some(ctx) => ctx,
        None => return (false, String::new()),
    };

    let last_match = match id_attr_re().captures_iter(&before).last() {
        Some(m) => m,
        None => return (false, String::new()),
    };

    let quote = last_match[1].chars().next().unwrap_or('"');
    let attr_value_start = last_match.get(0).unwrap().end();
    let value_before_cursor = &before[attr_value_start..];
    let prefix = extract_prefix(value_before_cursor);
    let value_after = line[col..].splitn(2, quote).next().unwrap_or("");
    let full_value = format!("{value_before_cursor}{value_after}");

    let prefix_lower = prefix.to_lowercase();
    let has_existing = full_value
        .split_whitespace()
        .any(|s| s.to_lowercase() != prefix_lower);

    (has_existing, prefix)
}

fn extract_prefix(s: &str) -> String {
    let bytes = s.as_bytes();
    let end = bytes.len();
    if end == 0 || !is_ident_byte(bytes[end - 1]) {
        return String::new();
    }
    let start = (0..end)
        .rev()
        .find(|&i| !is_ident_byte(bytes[i]))
        .map(|i| i + 1)
        .unwrap_or(0);
    s[start..].to_string()
}

fn build_insert_text(name: &str, line: &str, col: usize) -> String {
    let bytes = line.as_bytes();
    let before_char = if col > 0 { bytes.get(col - 1).copied() } else { None };
    let after_char = bytes.get(col).copied();
    let needs_space_before = before_char.is_some_and(is_ident_byte);
    let needs_space_after = after_char.is_some_and(is_ident_byte);
    match (needs_space_before, needs_space_after) {
        (true, true) => format!(" {name} "),
        (true, false) => format!(" {name}"),
        (false, true) => format!("{name} "),
        (false, false) => name.to_string(),
    }
}

fn word_at_ctx(line: &str, col: usize) -> Option<String> {
    let bytes = line.as_bytes();
    if col >= bytes.len() || !is_ident_byte(bytes[col]) {
        return None;
    }
    let start = (0..col)
        .rev()
        .find(|&i| !is_ident_byte(bytes[i]))
        .map(|i| i + 1)
        .unwrap_or(0);
    let end = (col + 1..bytes.len())
        .find(|&i| !is_ident_byte(bytes[i]))
        .unwrap_or(bytes.len());
    Some(line[start..end].to_string())
}

fn word_at(text: &str, pos: Position) -> Option<String> {
    let (_, line, col) = cursor_context(text, pos)?;
    word_at_ctx(line, col)
}

#[inline]
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// Returns true if `s` is a safe ASCII CSS identifier.
/// Guards rename and code-action inputs against CSS syntax injection.
/// Per CSS Syntax Level 3, identifiers cannot start with an unescaped digit.
fn is_valid_css_ident(s: &str) -> bool {
    if s.is_empty() { return false; }
    let bytes = s.as_bytes();
    !bytes[0].is_ascii_digit()
        && bytes
            .iter()
            .all(|&b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_'))
}

// ---------------------------------------------------------------------------
// CSS parsing
// ---------------------------------------------------------------------------

/// Skips CSS files larger than 500 KB to avoid stalling on minified files.
const MAX_CSS_BYTES: u64 = 500 * 1024;

fn scan_directory(root: &Path, class_map: &mut ClassMap) {
    // A single shared visited set across all files prevents parsing the same
    // file twice when it appears directly in the workspace AND is @import-ed by
    // another file — which would otherwise produce false-positive duplicate-selector
    // warnings for every class in the imported file.
    let mut visited: HashSet<PathBuf> = HashSet::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            name != "node_modules" && !name.starts_with('.')
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "css"))
    {
        parse_css_file_inner(entry.path(), class_map, &mut visited);
    }
}

fn parse_css_file(path: &Path, class_map: &mut ClassMap) {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    parse_css_file_inner(path, class_map, &mut visited);
}

fn parse_css_file_inner(path: &Path, class_map: &mut ClassMap, visited: &mut HashSet<PathBuf>) {
    // Skip files that are too large (e.g. minified bundles).
    if fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_CSS_BYTES {
        return;
    }

    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return,
    };
    if !visited.insert(canonical) {
        return;
    }

    let source_file = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let source_path = path.to_string_lossy().into_owned();

    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };

    parse_css_content(&content, &source_file, &source_path, class_map);

    let parent = path.parent().unwrap_or(Path::new("."));
    for import_path in extract_imports(&content) {
        parse_css_file_inner(&parent.join(&import_path), class_map, visited);
    }
}

fn parse_css_content(content: &str, source_file: &str, source_path: &str, class_map: &mut ClassMap) {
    let stripped = strip_comments(content);
    parse_rules_at_level(&stripped, 0, None, source_file, source_path, class_map);
}

fn parse_rules_at_level(
    content: &str,
    base_line: u32,
    media_query: Option<&str>,
    source_file: &str,
    source_path: &str,
    class_map: &mut ClassMap,
) {
    let bytes = content.as_bytes();
    let mut i = 0usize;
    let mut chunk_start = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' { i += 1; }
                    else if bytes[i] == quote { break; }
                    i += 1;
                }
                i += 1;
            }
            b';' => {
                i += 1;
                chunk_start = i;
            }
            b'{' => {
                let raw_chunk = &content[chunk_start..i];
                let pending = raw_chunk.trim();

                if pending.starts_with('@') {
                    let is_conditional = pending.starts_with("@media")
                        || pending.starts_with("@supports");
                    let child_mq = if is_conditional { Some(pending) } else { media_query };

                    let block_start = i + 1;
                    i = advance_past_block(bytes, i + 1);
                    let block = &content[block_start..i];
                    let block_base = base_line + byte_offset_to_line(content, block_start);
                    parse_rules_at_level(block, block_base, child_mq, source_file, source_path, class_map);
                    i += 1;
                } else if !pending.is_empty() {
                    let trim_offset = raw_chunk.len() - raw_chunk.trim_start().len();
                    let definition_line = base_line + byte_offset_to_line(content, chunk_start + trim_offset);

                    let props_start = i + 1;
                    i = advance_past_block(bytes, i + 1);
                    let properties = content[props_start..i].trim();

                    if !properties.is_empty() {
                        process_selector(pending, properties, definition_line, media_query, source_file, source_path, class_map);
                    }
                    i += 1;
                } else {
                    i += 1;
                }
                chunk_start = i;
            }
            b'}' => {
                i += 1;
                chunk_start = i;
            }
            _ => { i += 1; }
        }
    }
}

fn advance_past_block(bytes: &[u8], mut i: usize) -> usize {
    let mut depth = 1usize;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'"' | b'\'' => {
                let q = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' { i += 1; }
                    else if bytes[i] == q { break; }
                    i += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 { return i; }
            }
            _ => {}
        }
        i += 1;
    }
    i
}

fn process_selector(
    selector_raw: &str,
    properties: &str,
    definition_line: u32,
    media_query: Option<&str>,
    source_file: &str,
    source_path: &str,
    class_map: &mut ClassMap,
) {
    let selector = pseudo_re().replace_all(selector_raw, "");
    let selector_display = selector.trim().to_string();

    for m in class_re().find_iter(&selector) {
        let name = m.as_str()[1..].to_string();
        class_map.entry(name).or_default().push(ClassInfo {
            properties: properties.to_string(),
            selector: selector_display.clone(),
            media_query: media_query.map(str::to_string),
            source_file: source_file.to_string(),
            source_path: source_path.to_string(),
            definition_line,
        });
    }

    for m in id_re().find_iter(&selector) {
        let name = m.as_str().to_string();
        class_map.entry(name).or_default().push(ClassInfo {
            properties: properties.to_string(),
            selector: selector_display.clone(),
            media_query: media_query.map(str::to_string),
            source_file: source_file.to_string(),
            source_path: source_path.to_string(),
            definition_line,
        });
    }
}

fn extract_imports(content: &str) -> Vec<String> {
    import_re()
        .captures_iter(content)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().to_string())
        .filter(|p| {
            // Reject network URLs and absolute paths to prevent reading files
            // outside the project tree via @import.
            !p.starts_with("http://")
                && !p.starts_with("https://")
                && !p.starts_with("//")
                && !Path::new(p).is_absolute()
        })
        .map(|mut p| {
            if !p.contains('.') { p.push_str(".css"); }
            p
        })
        .collect()
}

fn strip_comments(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    let mut in_comment = false;

    while let Some(c) = chars.next() {
        if in_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                result.push(' ');
                result.push(' ');
                in_comment = false;
            } else if c == '\n' {
                result.push('\n');
            } else {
                result.push(' ');
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            result.push(' ');
            result.push(' ');
            in_comment = true;
        } else {
            result.push(c);
        }
    }

    result
}

fn byte_offset_to_line(content: &str, offset: usize) -> u32 {
    content[..offset.min(content.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32
}

fn update_css_map(params: &DidChangeWatchedFilesParams, class_map: &mut ClassMap) {
    for change in &params.changes {
        let path: PathBuf = match change.uri.to_file_path() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if path.extension().map_or(true, |e| e != "css") {
            continue;
        }
        let source_path = path.to_string_lossy().into_owned();

        for infos in class_map.values_mut() {
            infos.retain(|info| info.source_path != source_path);
        }
        class_map.retain(|_, infos| !infos.is_empty());

        if change.typ == FileChangeType::CREATED || change.typ == FileChangeType::CHANGED {
            parse_css_file(&path, class_map);
        }
    }
}

// ---------------------------------------------------------------------------
// CSS variable map
// ---------------------------------------------------------------------------

/// Rebuilds the CSS variable map from all property blobs in `class_map`.
/// Variables are discovered as `--name: value` lines inside any rule.
fn refresh_var_map(class_map: &ClassMap) -> VarMap {
    let mut vars = HashMap::new();
    for infos in class_map.values() {
        for info in infos {
            for line in info.properties.lines() {
                let line = line.trim().trim_end_matches(';').trim();
                if let Some(rest) = line.strip_prefix("--") {
                    if let Some(colon) = rest.find(':') {
                        let name = format!("--{}", rest[..colon].trim());
                        let value = rest[colon + 1..].trim().to_string();
                        if !value.is_empty() {
                            vars.entry(name).or_insert(value);
                        }
                    }
                }
            }
        }
    }
    vars
}

// ---------------------------------------------------------------------------
// Workspace HTML scanning (references + rename)
// ---------------------------------------------------------------------------

/// Returns all locations in workspace HTML files where `lookup_key` is used as
/// a class name (bare) or an ID value (`#`-prefixed key → bare in attribute).
fn workspace_html_refs(
    lookup_key: &str,
    root: &Path,
    documents: &DocumentMap,
) -> Vec<Location> {
    let is_id = lookup_key.starts_with('#');
    let bare = if is_id { &lookup_key[1..] } else { lookup_key };
    let mut locs = Vec::new();

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let n = e.file_name().to_str().unwrap_or("");
            n != "node_modules" && !n.starts_with('.')
        })
        .filter_map(|e| e.ok())
        .filter(|e| {
            matches!(
                e.path().extension().and_then(|s| s.to_str()),
                Some("html") | Some("htm")
            )
        })
    {
        let path = entry.path();
        let uri = match Url::from_file_path(path) {
            Ok(u) => u,
            Err(_) => continue,
        };

        let owned;
        let text: &str = if let Some(t) = documents.get(&uri) {
            t
        } else {
            match fs::read_to_string(path) {
                Ok(t) => { owned = t; &owned }
                Err(_) => continue,
            }
        };

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
    }

    locs
}

// ---------------------------------------------------------------------------
// Code action helpers
// ---------------------------------------------------------------------------

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
/// rule: prefers a file in the same directory as the HTML file, then any file.
fn find_target_css_file(html_uri: &Url, class_map: &ClassMap) -> Option<PathBuf> {
    let html_dir = html_uri.to_file_path().ok()?.parent()?.to_path_buf();

    let css_paths: HashSet<&str> = class_map
        .values()
        .flat_map(|v| v.iter().map(|i| i.source_path.as_str()))
        .collect();

    // Prefer same directory.
    for s in &css_paths {
        let p = Path::new(s);
        if p.parent() == Some(&html_dir) {
            return Some(p.to_path_buf());
        }
    }

    // Fall back to any known CSS file.
    css_paths.iter().next().map(|s| PathBuf::from(s))
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

fn is_html_uri(uri: &Url) -> bool {
    let p = uri.path();
    p.ends_with(".html") || p.ends_with(".htm")
}

fn publish_diagnostics(uri: Url, diagnostics: Vec<Diagnostic>) -> lsp_server::Notification {
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

struct SelectorRef {
    line: u32,
    col_start: u32,
    col_end: u32,
    name: String,
    is_id: bool,
}

fn html_selector_refs(text: &str) -> Vec<SelectorRef> {
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

fn collect_attr_refs(
    line: &str,
    line_num: u32,
    attr_re: &Regex,
    is_id: bool,
    refs: &mut Vec<SelectorRef>,
) -> Option<(char, bool)> {
    let mut last_open = None;
    for cap in attr_re.captures_iter(line) {
        let quote = cap[1].chars().next().unwrap_or('"');
        let value_start = cap.get(0).unwrap().end();
        let rest = &line[value_start..];
        let (value_end, open) = match rest.find(quote) {
            Some(len) => (value_start + len, false),
            None => (value_start + rest.len(), true),
        };
        collect_value_tokens(line, line_num, value_start, value_end, is_id, refs);
        last_open = if open { Some((quote, is_id)) } else { None };
    }
    last_open
}

/// Processes the leading portion of a line that continues an open attribute from
/// the previous line. Returns `Some((quote, is_id))` if still unclosed at line end.
fn collect_continuation(
    line: &str,
    line_num: u32,
    quote: char,
    is_id: bool,
    refs: &mut Vec<SelectorRef>,
) -> Option<(char, bool)> {
    match line.find(quote) {
        Some(close) => {
            collect_value_tokens(line, line_num, 0, close, is_id, refs);
            None
        }
        None => {
            collect_value_tokens(line, line_num, 0, line.len(), is_id, refs);
            Some((quote, is_id))
        }
    }
}

fn diagnostics_for_html(text: &str, class_map: &ClassMap) -> Vec<Diagnostic> {
    html_selector_refs(text)
        .into_iter()
        .filter_map(|r| {
            let lookup = if r.is_id { format!("#{}", r.name) } else { r.name.clone() };
            if class_map.contains_key(&lookup) {
                return None;
            }
            let kind = if r.is_id { "id" } else { "class" };
            Some(Diagnostic {
                range: Range {
                    start: Position { line: r.line, character: r.col_start },
                    end: Position { line: r.line, character: r.col_end },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("css-class-mapper".to_string()),
                message: format!("Unknown CSS {kind} '{}'", r.name),
                ..Default::default()
            })
        })
        .collect()
}

fn diagnostics_for_css_duplicates(class_map: &ClassMap, source_path: &str) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    for (name, infos) in class_map {
        let in_file: Vec<_> = infos.iter().filter(|i| i.source_path == source_path).collect();
        if in_file.len() <= 1 { continue; }

        let display = if name.starts_with('#') { name.clone() } else { format!(".{name}") };
        for info in &in_file[1..] {
            diags.push(Diagnostic {
                range: Range {
                    start: Position { line: info.definition_line, character: 0 },
                    end: Position { line: info.definition_line, character: 0 },
                },
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("css-class-mapper".to_string()),
                message: format!("'{display}' is already defined earlier in this file"),
                ..Default::default()
            });
        }
    }

    diags
}

/// Returns hint-level diagnostics for CSS selectors that are not referenced in
/// any currently-open HTML document. Callers should decide whether to surface
/// these (they are intentionally soft — JS-driven classes will appear unused).
fn diagnostics_for_unused(
    class_map: &ClassMap,
    documents: &DocumentMap,
) -> HashMap<Url, Vec<Diagnostic>> {
    // Collect all selectors used in open HTML documents.
    let mut used: HashSet<String> = HashSet::new();
    for (uri, text) in documents {
        if !is_html_uri(uri) { continue; }
        for r in html_selector_refs(text) {
            let key = if r.is_id { format!("#{}", r.name) } else { r.name };
            used.insert(key);
        }
    }

    // If no HTML files are open, don't emit any hints — we have no evidence.
    if used.is_empty() { return HashMap::new(); }

    let mut out: HashMap<Url, Vec<Diagnostic>> = HashMap::new();

    for (name, infos) in class_map {
        if used.contains(name) { continue; }
        let display = if name.starts_with('#') { name.clone() } else { format!(".{name}") };

        for info in infos {
            let uri = match Url::from_file_path(&info.source_path) {
                Ok(u) => u,
                Err(_) => continue,
            };
            out.entry(uri).or_default().push(Diagnostic {
                range: Range {
                    start: Position { line: info.definition_line, character: 0 },
                    end: Position { line: info.definition_line, character: 0 },
                },
                severity: Some(DiagnosticSeverity::HINT),
                source: Some("css-class-mapper".to_string()),
                message: format!("'{display}' is not used in any open HTML file"),
                ..Default::default()
            });
        }
    }

    out
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
}

// ---------------------------------------------------------------------------
// Hover helpers
// ---------------------------------------------------------------------------

fn specificity(selector: &str) -> (u32, u32, u32) {
    let part = selector.split(',').next().unwrap_or(selector);
    let a = id_re().find_iter(part).count() as u32;
    let b = class_re().find_iter(part).count() as u32;
    (a, b, 0)
}

/// Returns a formatted string of unique color values found in `properties`.
fn color_summary(properties: &str) -> String {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut order: Vec<&str> = Vec::new();
    for m in color_re().find_iter(properties) {
        let v = m.as_str();
        if seen.insert(v) {
            order.push(v);
        }
    }
    order.into_iter()
        .map(|c| format!("`{c}`"))
        .collect::<Vec<_>>()
        .join(" ")
}
