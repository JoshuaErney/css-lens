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

// Matches a full <link .../> or <link ...> tag (including multi-line via [^>]).
fn link_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)<link\b[^>]*/?>").unwrap())
}

fn rel_stylesheet_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\brel\s*=\s*["']stylesheet["']"#).unwrap())
}

fn href_val_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\bhref\s*=\s*["']([^"']+)["']"#).unwrap())
}

// Matches the content of <style>...</style> blocks (case-insensitive, DOTALL).
fn style_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<style\b[^>]*>(.*?)</style>").unwrap())
}

fn keyframes_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)@keyframes\s+([a-zA-Z_-][a-zA-Z0-9_-]*)").unwrap())
}

fn attr_selector_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[[^\]]*\]").unwrap())
}

fn element_type_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b[a-zA-Z][a-zA-Z0-9]*\b").unwrap())
}

fn js_classlist_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"classList\s*\.\s*(?:add|remove|toggle|contains|replace)\s*\(\s*["']([^"']+)["']"#).unwrap()
    })
}

fn js_gebc_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"getElementsByClassName\s*\(\s*["']([^"']+)["']"#).unwrap()
    })
}

fn js_query_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"querySelectorAll?\s*\(\s*["']([^"']+)["']"#).unwrap()
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
    layer: Option<String>,
    source_file: String,
    source_path: String,
    definition_line: u32,
}

/// CSS custom properties map: `"--name"` → `"value"`.
type VarMap = HashMap<String, String>;
type ClassMap = HashMap<String, Vec<ClassInfo>>;
type DocumentMap = HashMap<Url, String>;

#[derive(Debug, Clone)]
struct KeyframeInfo {
    source_file: String,
    source_path: String,
    definition_line: u32,
}
type KeyframesMap = HashMap<String, KeyframeInfo>;

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
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        code_lens_provider: Some(CodeLensOptions { resolve_provider: Some(false) }),
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
    let mut keyframes_map: KeyframesMap = if let Some(ref root) = root_path {
        scan_keyframes(root)
    } else {
        KeyframesMap::new()
    };

    let init_result = InitializeResult {
        capabilities,
        server_info: Some(ServerInfo {
            name: "css-lens-lsp".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    };
    connection.initialize_finish(init_id, serde_json::to_value(init_result)?)?;

    // Register a file watcher for CSS files so the client sends
    // workspace/didChangeWatchedFiles when any .css file is saved.
    let register_params = RegistrationParams {
        registrations: vec![Registration {
            id: "css-lens-css-watcher".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: Some(
                serde_json::to_value(DidChangeWatchedFilesRegistrationOptions {
                    watchers: vec![FileSystemWatcher {
                        glob_pattern: GlobPattern::String("**/*.css".to_string()),
                        kind: Some(WatchKind::Create | WatchKind::Change | WatchKind::Delete),
                    }],
                })
                .unwrap_or(json!({})),
            ),
        }],
    };
    connection.sender.send(Message::Request(lsp_server::Request {
        id: RequestId::from(0i32),
        method: "client/registerCapability".to_string(),
        params: serde_json::to_value(register_params).unwrap_or(json!({})),
    }))?;

    let mut documents: DocumentMap = HashMap::new();
    // Pre-built usage counts: rebuilt whenever HTML docs change so code lens
    // requests never block on a full workspace walk.
    let mut usage_counts: HashMap<String, usize> = HashMap::new();

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
                    &keyframes_map,
                    &usage_counts,
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
                    &mut keyframes_map,
                    &mut usage_counts,
                    &mut documents,
                    root_path.as_deref(),
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

/// Extracts the `textDocument.uri` field from any `textDocument/*` request params.
fn params_uri(params: &Value) -> Option<Url> {
    let uri_val = params
        .pointer("/textDocument/uri")
        .or_else(|| params.pointer("/textDocumentPositionParams/textDocument/uri"))?;
    Url::parse(uri_val.as_str()?).ok()
}

fn route_request(
    id: RequestId,
    method: &str,
    params: &Value,
    class_map: &ClassMap,
    var_map: &VarMap,
    keyframes_map: &KeyframesMap,
    usage_counts: &HashMap<String, usize>,
    documents: &DocumentMap,
    root_path: Option<&Path>,
) -> Response {
    // For HTML-file requests, build the per-document scoped map once here so
    // completion, hover, and definition handlers all share the same computation.
    let scoped: Option<ClassMap> = params_uri(params)
        .filter(|uri| is_html_uri(uri))
        .and_then(|uri| {
            let path = uri.to_file_path().ok()?;
            let text = documents.get(&uri)?;
            build_document_class_map(class_map, &path, text)
        });
    let scoped_vars: Option<VarMap> = scoped.as_ref().map(|m| refresh_var_map(m));
    let effective_map: &ClassMap = scoped.as_ref().unwrap_or(class_map);
    let effective_vars: &VarMap = scoped_vars.as_ref().unwrap_or(var_map);

    match method {
        "textDocument/completion" => {
            match serde_json::from_value::<CompletionParams>(params.clone()) {
                Ok(p) => completion_handler(id, p, effective_map, effective_vars, keyframes_map, documents),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        "textDocument/hover" => {
            match serde_json::from_value::<HoverParams>(params.clone()) {
                Ok(p) => hover_handler(id, p, effective_map, effective_vars, var_map, keyframes_map, documents),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        "textDocument/definition" => {
            match serde_json::from_value::<GotoDefinitionParams>(params.clone()) {
                Ok(p) => definition_handler(id, p, effective_map, keyframes_map, documents),
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
                Ok(p) => code_action_handler(id, p, class_map, documents),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        "textDocument/documentSymbol" => {
            match serde_json::from_value::<DocumentSymbolParams>(params.clone()) {
                Ok(p) => document_symbol_handler(id, p, class_map),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        "textDocument/codeLens" => {
            match serde_json::from_value::<CodeLensParams>(params.clone()) {
                Ok(p) => code_lens_handler(id, p, class_map, usage_counts),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        "workspace/symbol" => {
            match serde_json::from_value::<WorkspaceSymbolParams>(params.clone()) {
                Ok(p) => workspace_symbol_handler(id, p, class_map),
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
    keyframes_map: &mut KeyframesMap,
    usage_counts: &mut HashMap<String, usize>,
    documents: &mut DocumentMap,
    root_path: Option<&Path>,
) -> Vec<lsp_server::Notification> {
    let mut out: Vec<lsp_server::Notification> = Vec::new();

    match method {
        "textDocument/didOpen" => {
            if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(params) {
                let uri = p.text_document.uri;
                let text = p.text_document.text;
                documents.insert(uri.clone(), text.clone());
                if is_html_uri(&uri) {
                    let diag_map = uri.to_file_path().ok().and_then(|path| {
                        let reachable = reachable_css_paths(&path, &text);
                        build_scoped_class_map(class_map, &reachable)
                    });
                    let effective = diag_map.as_ref().unwrap_or(class_map);
                    out.push(publish_diagnostics(uri.clone(), all_html_diagnostics(&text, &uri, effective)));
                    if let Some(root) = root_path {
                        *usage_counts = build_usage_counts(root, documents);
                    }
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
                        let diag_map = uri.to_file_path().ok().and_then(|path| {
                            let reachable = reachable_css_paths(&path, &text);
                            build_scoped_class_map(class_map, &reachable)
                        });
                        let effective = diag_map.as_ref().unwrap_or(class_map);
                        out.push(publish_diagnostics(uri.clone(), all_html_diagnostics(&text, &uri, effective)));
                        if let Some(root) = root_path {
                            *usage_counts = build_usage_counts(root, documents);
                        }
                    }
                }
            }
        }
        "textDocument/didClose" => {
            if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(params) {
                let uri = p.text_document.uri;
                let was_html = is_html_uri(&uri);
                documents.remove(&uri);
                out.push(publish_diagnostics(uri, vec![]));
                if was_html {
                    if let Some(root) = root_path {
                        *usage_counts = build_usage_counts(root, documents);
                    }
                }
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
                update_keyframes_map(&p, keyframes_map);
                if let Some(root) = root_path {
                    *usage_counts = build_usage_counts(root, documents);
                }

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
                for (css_uri, diags) in diagnostics_for_unused(class_map, documents, root_path) {
                    out.push(publish_diagnostics(css_uri, diags));
                }

                // Refresh HTML diagnostics since the class map changed.
                let html_diags: Vec<_> = documents
                    .iter()
                    .filter(|(uri, _)| is_html_uri(uri))
                    .map(|(uri, text)| {
                        let diag_map = uri.to_file_path().ok().and_then(|path| {
                            let reachable = reachable_css_paths(&path, text);
                            build_scoped_class_map(class_map, &reachable)
                        });
                        let effective = diag_map.as_ref().unwrap_or(class_map);
                        publish_diagnostics(uri.clone(), all_html_diagnostics(text, uri, effective))
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
    effective_map: &ClassMap,
    effective_var_map: &VarMap,
    keyframes_map: &KeyframesMap,
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

        let mut items: Vec<CompletionItem> = effective_map
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

        let mut items: Vec<CompletionItem> = effective_map
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

    // style="..." — property names, keyword values, CSS variables, and animation names
    if in_style_attribute(text, pos) {
        match style_context(text, pos) {
            StyleContext::PropertyName { prefix } => {
                let prefix_lower = prefix.to_lowercase();
                let mut items: Vec<CompletionItem> = css_property_completions()
                    .iter()
                    .filter(|name| prefix.is_empty() || name.to_lowercase().starts_with(&prefix_lower))
                    .map(|name| CompletionItem {
                        label: (*name).to_string(),
                        kind: Some(CompletionItemKind::PROPERTY),
                        insert_text: Some(format!("{name}: ")),
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
            StyleContext::PropertyValue { property, prefix } => {
                // CSS variable completions inside any value (e.g. `color: var(--`)
                if prefix.starts_with("--") {
                    let prefix_lower = prefix.to_lowercase();
                    let mut items: Vec<CompletionItem> = effective_var_map
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
                // animation-name → offer @keyframes names
                if property == "animation-name" {
                    let prefix_lower = prefix.to_lowercase();
                    let mut items: Vec<CompletionItem> = keyframes_map
                        .iter()
                        .filter(|(name, _)| prefix.is_empty() || name.to_lowercase().starts_with(&prefix_lower))
                        .map(|(name, info)| CompletionItem {
                            label: name.clone(),
                            kind: Some(CompletionItemKind::VALUE),
                            detail: Some(info.source_file.clone()),
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
                // Keyword value completions for known properties
                let prefix_lower = prefix.to_lowercase();
                let values = css_value_completions(&property);
                if !values.is_empty() {
                    let mut items: Vec<CompletionItem> = values
                        .iter()
                        .filter(|v| prefix.is_empty() || v.to_lowercase().starts_with(&prefix_lower))
                        .map(|v| CompletionItem {
                            label: (*v).to_string(),
                            kind: Some(CompletionItemKind::VALUE),
                            insert_text: Some((*v).to_string()),
                            filter_text: Some(v.to_lowercase()),
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
            StyleContext::None => {}
        }
    }

    Response::new_ok(id, json!(null))
}

fn hover_handler(
    id: RequestId,
    params: HoverParams,
    effective_map: &ClassMap,
    effective_var_map: &VarMap,
    var_map: &VarMap,
    keyframes_map: &KeyframesMap,
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

    // CSS file hover: show declared value when cursor is on a --custom-property.
    if uri.path().ends_with(".css") {
        if let Some(w) = word_at_ctx(line, col) {
            if w.starts_with("--") {
                if let Some(value) = var_map.get(w.as_str()) {
                    let hover = Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format!("**{}**\n\n```css\n{}: {};\n```", w, w, value),
                        }),
                        range: None,
                    };
                    return Response::new_ok(id, serde_json::to_value(hover).unwrap_or(json!(null)));
                }
            }
        }
        return Response::new_ok(id, json!(null));
    }

    // style="..." — hover over CSS variables or animation-name keyframe names
    if in_attr_before(&before, style_attr_re()) {
        let word = word_at_ctx(line, col);
        // CSS variable hover (--foo) takes priority
        if let Some(ref w) = word {
            if w.starts_with("--") {
                // Fall back to the global var_map when the scoped map doesn't contain
                // the variable (e.g. --brand defined in an unlinked CSS file).
                let value = match effective_var_map.get(w.as_str()).or_else(|| var_map.get(w.as_str())) {
                    Some(v) => v,
                    None => return Response::new_ok(id, json!(null)),
                };
                let hover = Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: format!("**{}**\n\n```css\n{}: {};\n```", w, w, value),
                    }),
                    range: None,
                };
                return Response::new_ok(id, serde_json::to_value(hover).unwrap_or(json!(null)));
            }
        }
        // animation-name value hover
        if let StyleContext::PropertyValue { ref property, .. } = style_context(text, pos) {
            if property == "animation-name" {
                if let Some(ref name) = word {
                    if let Some(info) = keyframes_map.get(name.as_str()) {
                        let hover = Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: format!(
                                    "**@keyframes {}** — {}:{}",
                                    name, info.source_file, info.definition_line + 1
                                ),
                            }),
                            range: None,
                        };
                        return Response::new_ok(id, serde_json::to_value(hover).unwrap_or(json!(null)));
                    }
                }
            }
        }
        return Response::new_ok(id, json!(null));
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

    let infos = match effective_map.get(&lookup_key) {
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
        let layer_ctx = info
            .layer
            .as_deref()
            .map(|l| format!("\n_in `@layer {l}`_"))
            .unwrap_or_default();
        let (a, b, c) = specificity(&info.selector);
        let colors = color_summary(&info.properties);
        let color_line = if colors.is_empty() {
            String::new()
        } else {
            format!("\n\nColors: {colors}")
        };
        format!(
            "**{}** — {}:{}{}{}\n\nSpecificity: `({a},{b},{c})`{color_line}\n\n```css\n{} {{\n{}\n}}\n```",
            lookup_key,
            info.source_file,
            info.definition_line + 1,
            mq,
            layer_ctx,
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
            let layer_ctx = info
                .layer
                .as_deref()
                .map(|l| format!(" _(@layer {l})_"))
                .unwrap_or_default();
            let (a, b, c) = specificity(&info.selector);
            parts.push(format!(
                "**{}.** {}:{}{}{} — Specificity: `({a},{b},{c})`\n```css\n{} {{\n{}\n}}\n```",
                i + 1,
                info.source_file,
                info.definition_line + 1,
                mq,
                layer_ctx,
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
    effective_map: &ClassMap,
    keyframes_map: &KeyframesMap,
    documents: &DocumentMap,
) -> Response {
    let uri = &params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;

    let text = match documents.get(uri) {
        Some(t) => t,
        None => return Response::new_ok(id, json!(null)),
    };

    // style="animation-name: <cursor>" — jump to @keyframes definition
    if in_style_attribute(text, pos) {
        if let StyleContext::PropertyValue { ref property, .. } = style_context(text, pos) {
            if property == "animation-name" {
                let (_, line, col) = match cursor_context(text, pos) {
                    Some(ctx) => ctx,
                    None => return Response::new_ok(id, json!(null)),
                };
                let name = match word_at_ctx(line, col) {
                    Some(w) => w,
                    None => return Response::new_ok(id, json!(null)),
                };
                if let Some(info) = keyframes_map.get(&name) {
                    let kf_uri = match Url::from_file_path(&info.source_path) {
                        Ok(u) => u,
                        Err(_) => return Response::new_ok(id, json!(null)),
                    };
                    let loc = Location {
                        uri: kf_uri,
                        range: Range {
                            start: Position { line: info.definition_line, character: 0 },
                            end: Position { line: info.definition_line, character: 0 },
                        },
                    };
                    return Response::new_ok(
                        id,
                        serde_json::to_value(GotoDefinitionResponse::Scalar(loc)).unwrap_or(json!(null)),
                    );
                }
                return Response::new_ok(id, json!(null));
            }
        }
        return Response::new_ok(id, json!(null));
    }

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

    let infos = match effective_map.get(&lookup_key) {
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
    if path.extension().map_or(true, |e| e != "css") {
        return None;
    }
    let source_path = path.to_string_lossy().into_owned();
    let bare = word_at(text, pos)?;
    let cursor_line = pos.line;

    if class_map.get(&bare).map_or(false, |infos| {
        infos.iter().any(|i| i.source_path == source_path && i.definition_line == cursor_line)
    }) {
        return Some(bare);
    }

    let id_key = format!("#{bare}");
    if class_map.get(&id_key).map_or(false, |infos| {
        infos.iter().any(|i| i.source_path == source_path && i.definition_line == cursor_line)
    }) {
        return Some(id_key);
    }

    None
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
    } else if let Some(key) = css_selector_at_cursor(uri, text, pos, class_map) {
        key
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
    documents: &DocumentMap,
) -> Response {
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
            let key = if display.starts_with('.') { display[1..].to_string() } else { display };
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

            let mut selectors: Vec<String> = hinted_at_block
                .iter()
                .map(|k| if k.starts_with('#') { k.clone() } else { format!(".{k}") })
                .collect();
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

fn document_symbol_handler(
    id: RequestId,
    params: DocumentSymbolParams,
    class_map: &ClassMap,
) -> Response {
    let uri = &params.text_document.uri;
    let path = match uri.to_file_path() {
        Ok(p) => p,
        Err(_) => return Response::new_ok(id, json!([])),
    };
    // Only CSS files get outline symbols from class_map.
    if path.extension().map_or(true, |e| e != "css") {
        return Response::new_ok(id, json!([]));
    }
    let source_path = path.to_string_lossy().into_owned();

    let mut symbols: Vec<SymbolInformation> = Vec::new();
    for (name, infos) in class_map {
        for info in infos {
            if info.source_path != source_path {
                continue;
            }
            let display = if name.starts_with('#') { name.clone() } else { format!(".{name}") };
            #[allow(deprecated)]
            symbols.push(SymbolInformation {
                name: display,
                kind: SymbolKind::CLASS,
                location: Location {
                    uri: uri.clone(),
                    range: Range {
                        start: Position { line: info.definition_line, character: 0 },
                        end: Position { line: info.definition_line, character: 0 },
                    },
                },
                tags: None,
                deprecated: None,
                container_name: match (&info.media_query, &info.layer) {
                    (Some(mq), Some(l)) => Some(format!("{mq} • @layer {l}")),
                    (Some(mq), None)    => Some(mq.clone()),
                    (None,     Some(l)) => Some(format!("@layer {l}")),
                    (None,     None)    => None,
                },
            });
        }
    }

    symbols.sort_by_key(|s| s.location.range.start.line);
    Response::new_ok(
        id,
        serde_json::to_value(DocumentSymbolResponse::Flat(symbols)).unwrap_or(json!([])),
    )
}

fn workspace_symbol_handler(
    id: RequestId,
    params: WorkspaceSymbolParams,
    class_map: &ClassMap,
) -> Response {
    let query = params.query.to_lowercase();
    let mut symbols: Vec<SymbolInformation> = Vec::new();

    for (name, infos) in class_map {
        let display = if name.starts_with('#') { name.clone() } else { format!(".{name}") };
        if !query.is_empty() && !display.to_lowercase().contains(&query) {
            continue;
        }
        for info in infos {
            let uri = match Url::from_file_path(&info.source_path) {
                Ok(u) => u,
                Err(_) => continue,
            };
            #[allow(deprecated)]
            symbols.push(SymbolInformation {
                name: display.clone(),
                kind: SymbolKind::CLASS,
                location: Location {
                    uri,
                    range: Range {
                        start: Position { line: info.definition_line, character: 0 },
                        end: Position { line: info.definition_line, character: 0 },
                    },
                },
                tags: None,
                deprecated: None,
                container_name: Some(info.source_file.clone()),
            });
        }
    }

    symbols.sort_by(|a, b| {
        a.name.cmp(&b.name)
            .then(a.container_name.cmp(&b.container_name))
            .then(a.location.range.start.line.cmp(&b.location.range.start.line))
    });
    Response::new_ok(id, serde_json::to_value(symbols).unwrap_or(json!([])))
}

fn build_usage_counts(root: &Path, documents: &DocumentMap) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    walk_html_files(root, documents, |_uri, text| {
        for r in html_selector_refs(text) {
            let key = if r.is_id { format!("#{}", r.name) } else { r.name };
            *counts.entry(key).or_insert(0) += 1;
        }
    });
    counts
}

fn code_lens_handler(
    id: RequestId,
    params: CodeLensParams,
    class_map: &ClassMap,
    usage_counts: &HashMap<String, usize>,
) -> Response {
    let uri = &params.text_document.uri;
    let path = match uri.to_file_path() {
        Ok(p) => p,
        Err(_) => return Response::new_ok(id, json!([])),
    };
    if path.extension().map_or(true, |e| e != "css") {
        return Response::new_ok(id, json!([]));
    }
    let source_path = path.to_string_lossy().into_owned();
    let counts = usage_counts;

    let mut lenses: Vec<CodeLens> = Vec::new();
    for (name, infos) in class_map {
        for info in infos {
            if info.source_path != source_path {
                continue;
            }
            let count = counts.get(name).copied().unwrap_or(0);
            let title = match count {
                0 => "unused".to_string(),
                1 => "used 1 time".to_string(),
                n => format!("used {n} times"),
            };
            lenses.push(CodeLens {
                range: Range {
                    start: Position { line: info.definition_line, character: 0 },
                    end: Position { line: info.definition_line, character: 0 },
                },
                command: Some(Command {
                    title,
                    command: String::new(),
                    arguments: None,
                }),
                data: None,
            });
        }
    }

    lenses.sort_by_key(|l| l.range.start.line);
    Response::new_ok(id, serde_json::to_value(lenses).unwrap_or(json!([])))
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

/// Returns the byte index of the last `;` that is not inside a single- or
/// double-quoted string. Used by `style_context` to split CSS declarations.
fn last_unquoted_semicolon(s: &str) -> Option<usize> {
    let mut in_quote: Option<char> = None;
    let mut last_semi: Option<usize> = None;
    for (i, c) in s.char_indices() {
        match in_quote {
            Some(q) if c == q => in_quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' => in_quote = Some(c),
                ';' => last_semi = Some(i),
                _ => {}
            }
        }
    }
    last_semi
}

enum StyleContext {
    PropertyName { prefix: String },
    PropertyValue { property: String, prefix: String },
    None,
}

/// Returns the cursor's semantic position within a `style="..."` attribute value.
fn style_context(text: &str, pos: Position) -> StyleContext {
    let (before, _, _) = match cursor_context(text, pos) {
        Some(ctx) => ctx,
        None => return StyleContext::None,
    };
    let last_match = match style_attr_re().captures_iter(&before).last() {
        Some(m) => m,
        None => return StyleContext::None,
    };
    let value_fragment = &before[last_match.get(0).unwrap().end()..];

    // Active declaration = text after the last unquoted `;`.
    // Using a bare rsplit would split on semicolons inside quoted strings
    // (e.g. content: "a;b"), so we locate the last unquoted semicolon instead.
    let active = match last_unquoted_semicolon(value_fragment) {
        Some(pos) => &value_fragment[pos + 1..],
        None => value_fragment,
    };

    match active.find(':') {
        None => {
            StyleContext::PropertyName { prefix: extract_prefix(active) }
        }
        Some(colon) => {
            let property = active[..colon].trim().to_lowercase();
            let value_part = &active[colon + 1..];
            StyleContext::PropertyValue { property, prefix: extract_prefix(value_part) }
        }
    }
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

    parse_css_content(&content, 0, &source_file, &source_path, class_map);

    let parent = path.parent().unwrap_or(Path::new("."));
    for import_path in extract_imports(&content) {
        parse_css_file_inner(&parent.join(&import_path), class_map, visited);
    }
}

fn parse_css_content(content: &str, base_line: u32, source_file: &str, source_path: &str, class_map: &mut ClassMap) {
    let stripped = strip_comments(content);
    parse_rules_at_level(&stripped, base_line, None, None, source_file, source_path, class_map);
}

fn parse_rules_at_level(
    content: &str,
    base_line: u32,
    media_query: Option<&str>,
    layer_name: Option<&str>,
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

                    // Track @layer name for selectors nested inside named layers.
                    let child_layer = if pending.starts_with("@layer") {
                        let name = pending["@layer".len()..].trim();
                        if name.is_empty() { layer_name } else { Some(name) }
                    } else {
                        layer_name
                    };

                    let block_start = i + 1;
                    i = advance_past_block(bytes, i + 1);
                    let block = &content[block_start..i];
                    let block_base = base_line + byte_offset_to_line(content, block_start);
                    parse_rules_at_level(block, block_base, child_mq, child_layer, source_file, source_path, class_map);
                    i += 1;
                } else if !pending.is_empty() {
                    let trim_offset = raw_chunk.len() - raw_chunk.trim_start().len();
                    let definition_line = base_line + byte_offset_to_line(content, chunk_start + trim_offset);

                    let props_start = i + 1;
                    i = advance_past_block(bytes, i + 1);
                    let properties = content[props_start..i].trim();

                    if !properties.is_empty() {
                        process_selector(pending, properties, definition_line, media_query, layer_name, source_file, source_path, class_map);
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
    layer_name: Option<&str>,
    source_file: &str,
    source_path: &str,
    class_map: &mut ClassMap,
) {
    // Use the full raw selector for display (shows :hover, ::before, :is() context, etc.)
    let selector_display = selector_raw.trim().to_string();

    // Extract classes and IDs from the raw selector so that names inside
    // :is(), :has(), :where(), :not() are captured and added to the map.
    // class_re/id_re never false-match pseudo content since :hover, :nth-child(2n+1),
    // etc. contain no '.' or '#' tokens.
    for m in class_re().find_iter(selector_raw) {
        let name = m.as_str()[1..].to_string();
        class_map.entry(name).or_default().push(ClassInfo {
            properties: properties.to_string(),
            selector: selector_display.clone(),
            media_query: media_query.map(str::to_string),
            layer: layer_name.map(str::to_string),
            source_file: source_file.to_string(),
            source_path: source_path.to_string(),
            definition_line,
        });
    }

    for m in id_re().find_iter(selector_raw) {
        let name = m.as_str().to_string();
        class_map.entry(name).or_default().push(ClassInfo {
            properties: properties.to_string(),
            selector: selector_display.clone(),
            media_query: media_query.map(str::to_string),
            layer: layer_name.map(str::to_string),
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

fn scan_keyframes(root: &Path) -> KeyframesMap {
    let mut map = KeyframesMap::new();
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
        scan_keyframes_file_inner(entry.path(), &mut map, &mut visited);
    }
    map
}

/// Scans `path` for `@keyframes` declarations and follows `@import` chains,
/// mirroring the `visited`-set dedup that `parse_css_file_inner` uses.
fn scan_keyframes_file_inner(path: &Path, keyframes_map: &mut KeyframesMap, visited: &mut HashSet<PathBuf>) {
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
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let source_file = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
    let source_path = path.to_string_lossy().into_owned();
    for cap in keyframes_re().captures_iter(&content) {
        let name = cap[1].to_string();
        let definition_line = byte_offset_to_line(&content, cap.get(0).unwrap().start());
        keyframes_map.insert(name, KeyframeInfo {
            source_file: source_file.clone(),
            source_path: source_path.clone(),
            definition_line,
        });
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    for import_path in extract_imports(&content) {
        scan_keyframes_file_inner(&parent.join(&import_path), keyframes_map, visited);
    }
}

fn scan_js_used_classes(root: &Path) -> HashSet<String> {
    let mut used = HashSet::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let n = e.file_name().to_str().unwrap_or("");
            n != "node_modules" && !n.starts_with('.')
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("js"))
    {
        if let Ok(content) = fs::read_to_string(entry.path()) {
            collect_js_class_refs(&content, &mut used);
        }
    }
    used
}

fn collect_js_class_refs(content: &str, used: &mut HashSet<String>) {
    // classList.add/remove/toggle/contains/replace("foo bar") — extract token(s) directly
    for cap in js_classlist_re().captures_iter(content) {
        for token in cap[1].split_whitespace() {
            used.insert(token.to_string());
        }
    }
    // getElementsByClassName("foo bar") — space-separated class names
    for cap in js_gebc_re().captures_iter(content) {
        for token in cap[1].split_whitespace() {
            used.insert(token.to_string());
        }
    }
    // querySelector/querySelectorAll(".foo #bar") — extract .class and #id tokens
    for cap in js_query_re().captures_iter(content) {
        let selector = &cap[1];
        for m in class_re().find_iter(selector) {
            used.insert(m.as_str()[1..].to_string()); // strip leading '.'
        }
        for m in id_re().find_iter(selector) {
            used.insert(m.as_str().to_string()); // keep '#' prefix to match class_map key
        }
    }
}

/// Re-scans a single CSS file (and its imports) for keyframes after a save.
fn scan_keyframes_in_file(path: &Path, keyframes_map: &mut KeyframesMap) {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    scan_keyframes_file_inner(path, keyframes_map, &mut visited);
}

fn update_keyframes_map(params: &DidChangeWatchedFilesParams, keyframes_map: &mut KeyframesMap) {
    for change in &params.changes {
        let path: PathBuf = match change.uri.to_file_path() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if path.extension().map_or(true, |e| e != "css") {
            continue;
        }
        let source_path = path.to_string_lossy().into_owned();
        keyframes_map.retain(|_, info| info.source_path != source_path);
        if change.typ == FileChangeType::CREATED || change.typ == FileChangeType::CHANGED {
            scan_keyframes_in_file(&path, keyframes_map);
        }
    }
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
// Per-document CSS scoping
// ---------------------------------------------------------------------------

/// Extracts hrefs from `<link rel="stylesheet" href="...">` tags, skipping
/// network URLs and server-absolute paths that can't be resolved locally.
fn extract_linked_css(html_text: &str) -> Vec<String> {
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
fn reachable_css_paths(html_path: &Path, html_text: &str) -> HashSet<PathBuf> {
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
    if fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_CSS_BYTES {
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

fn is_reachable(source_path: &str, reachable: &HashSet<PathBuf>) -> bool {
    Path::new(source_path)
        .canonicalize()
        .map(|c| reachable.contains(&c))
        .unwrap_or(false)
}

/// Returns a ClassMap filtered to entries whose source file is in `reachable`.
/// Returns `None` when `reachable` is empty — callers fall back to the global map.
fn build_scoped_class_map(class_map: &ClassMap, reachable: &HashSet<PathBuf>) -> Option<ClassMap> {
    if reachable.is_empty() {
        return None;
    }
    let mut scoped = ClassMap::new();
    for (key, infos) in class_map {
        let scoped_infos: Vec<ClassInfo> = infos
            .iter()
            .filter(|i| is_reachable(&i.source_path, reachable))
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
fn extract_style_blocks(html_text: &str) -> Vec<(u32, String)> {
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
fn build_document_class_map(class_map: &ClassMap, html_path: &Path, html_text: &str) -> Option<ClassMap> {
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
        for (base_line, content) in style_blocks {
            parse_css_content(&content, base_line, &source_file, &source_path, &mut doc_map);
        }
    }

    Some(doc_map)
}

// ---------------------------------------------------------------------------
// style="" property and value completion dictionaries
// ---------------------------------------------------------------------------

fn css_property_completions() -> &'static [&'static str] {
    &[
        "align-content", "align-items", "align-self", "animation", "animation-delay",
        "animation-direction", "animation-duration", "animation-fill-mode",
        "animation-iteration-count", "animation-name", "animation-play-state",
        "animation-timing-function", "aspect-ratio", "background", "background-attachment",
        "background-clip", "background-color", "background-image", "background-position",
        "background-repeat", "background-size", "border", "border-bottom", "border-color",
        "border-left", "border-radius", "border-right", "border-style", "border-top",
        "border-width", "bottom", "box-shadow", "box-sizing", "clear", "clip-path", "color",
        "column-gap", "content", "cursor", "display", "filter", "flex", "flex-direction",
        "flex-grow", "flex-shrink", "flex-wrap", "float", "font", "font-family", "font-size",
        "font-style", "font-variant", "font-weight", "gap", "grid", "grid-column", "grid-row",
        "grid-template", "grid-template-areas", "grid-template-columns", "grid-template-rows",
        "height", "isolation", "justify-content", "justify-items", "justify-self", "left",
        "letter-spacing", "line-height", "list-style", "margin", "margin-bottom", "margin-left",
        "margin-right", "margin-top", "max-height", "max-width", "min-height", "min-width",
        "mix-blend-mode", "object-fit", "opacity", "order", "outline", "overflow", "overflow-x",
        "overflow-y", "padding", "padding-bottom", "padding-left", "padding-right", "padding-top",
        "pointer-events", "position", "resize", "right", "row-gap", "text-align",
        "text-decoration", "text-overflow", "text-shadow", "text-transform", "top", "transform",
        "transition", "user-select", "vertical-align", "visibility", "white-space", "width",
        "word-break", "z-index",
    ]
}

fn css_value_completions(property: &str) -> &'static [&'static str] {
    match property {
        "display" => &["block", "inline", "inline-block", "flex", "inline-flex", "grid",
            "inline-grid", "none", "contents", "flow-root", "table", "table-cell"],
        "position" => &["static", "relative", "absolute", "fixed", "sticky"],
        "visibility" => &["visible", "hidden", "collapse"],
        "overflow" | "overflow-x" | "overflow-y" => &["visible", "hidden", "scroll", "auto", "clip"],
        "box-sizing" => &["content-box", "border-box"],
        "float" => &["left", "right", "none", "inline-start", "inline-end"],
        "clear" => &["left", "right", "both", "none"],
        "flex-direction" => &["row", "row-reverse", "column", "column-reverse"],
        "flex-wrap" => &["nowrap", "wrap", "wrap-reverse"],
        "justify-content" => &["flex-start", "flex-end", "center", "space-between",
            "space-around", "space-evenly", "start", "end", "normal"],
        "align-items" => &["stretch", "flex-start", "flex-end", "center", "baseline",
            "start", "end", "normal"],
        "align-self" => &["auto", "stretch", "flex-start", "flex-end", "center", "baseline",
            "start", "end", "normal"],
        "align-content" => &["normal", "flex-start", "flex-end", "center", "space-between",
            "space-around", "space-evenly", "stretch"],
        "text-align" => &["left", "right", "center", "justify", "start", "end"],
        "text-decoration" => &["none", "underline", "overline", "line-through"],
        "text-transform" => &["none", "uppercase", "lowercase", "capitalize"],
        "white-space" => &["normal", "nowrap", "pre", "pre-wrap", "pre-line", "break-spaces"],
        "vertical-align" => &["baseline", "top", "middle", "bottom", "text-top", "text-bottom",
            "sub", "super"],
        "font-weight" => &["normal", "bold", "lighter", "bolder",
            "100", "200", "300", "400", "500", "600", "700", "800", "900"],
        "font-style" => &["normal", "italic", "oblique"],
        "cursor" => &["auto", "default", "pointer", "move", "text", "wait", "help",
            "not-allowed", "grab", "grabbing", "crosshair", "zoom-in", "zoom-out"],
        "pointer-events" => &["auto", "none"],
        "user-select" => &["auto", "none", "text", "all"],
        "resize" => &["none", "both", "horizontal", "vertical"],
        "object-fit" => &["fill", "contain", "cover", "none", "scale-down"],
        "border-style" => &["none", "solid", "dashed", "dotted", "double", "groove",
            "ridge", "inset", "outset"],
        "mix-blend-mode" => &["normal", "multiply", "screen", "overlay", "darken", "lighten",
            "difference", "exclusion", "hue", "saturation", "color", "luminosity"],
        "isolation" => &["auto", "isolate"],
        "animation-fill-mode" => &["none", "forwards", "backwards", "both"],
        "animation-direction" => &["normal", "reverse", "alternate", "alternate-reverse"],
        "animation-timing-function" => &["ease", "linear", "ease-in", "ease-out", "ease-in-out",
            "step-start", "step-end"],
        "animation-play-state" => &["running", "paused"],
        "animation-iteration-count" => &["infinite"],
        _ => &[],
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

/// Visits every HTML file in the workspace, passing `(uri, text)` to `visitor`.
/// Prefers the editor's in-memory buffer; falls back to disk for closed files.
fn walk_html_files<F>(root: &Path, documents: &DocumentMap, mut visitor: F)
where
    F: FnMut(&Url, &str),
{
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
        let owned_text;
        let text: &str = if let Some(t) = documents.get(&uri) {
            t.as_str()
        } else {
            match fs::read_to_string(path) {
                Ok(t) => { owned_text = t; owned_text.as_str() }
                Err(_) => continue,
            }
        };
        visitor(&uri, text);
    }
}

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
    css_paths.iter().next().map(|s| PathBuf::from(s))
}

/// Scans `content` from `definition_line` to find the full rule block extent,
/// handling comments and string literals that may contain braces.
/// Returns `(definition_line, closing_brace_line)` as 0-indexed line numbers.
fn find_rule_extent(content: &str, definition_line: u32) -> Option<(u32, u32)> {
    let bytes = content.as_bytes();
    let mut i = 0usize;
    let mut cur_line = 0u32;

    // Advance to definition_line.
    while i < bytes.len() && cur_line < definition_line {
        if bytes[i] == b'\n' {
            cur_line += 1;
        }
        i += 1;
    }
    if cur_line < definition_line {
        return None;
    }

    let mut depth = 0usize;
    let mut found_open = false;

    while i < bytes.len() {
        match bytes[i] {
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == b'\n' {
                        cur_line += 1;
                    }
                    if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            b'"' | b'\'' => {
                let q = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\n' {
                        cur_line += 1;
                    } else if bytes[i] == b'\\' {
                        i += 1;
                    } else if bytes[i] == q {
                        break;
                    }
                    i += 1;
                }
            }
            b'\n' => {
                cur_line += 1;
            }
            b'{' => {
                depth += 1;
                found_open = true;
            }
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                }
                if found_open && depth == 0 {
                    return Some((definition_line, cur_line));
                }
            }
            _ => {}
        }
        i += 1;
    }

    None
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
        // A raw '<' in an attribute value means the attribute is malformed.
        // Terminate there so the parser doesn't bleed into adjacent tags.
        let tag_start = rest.find('<');
        let (value_end, open) = match rest.find(quote) {
            Some(len) if tag_start.map_or(true, |t| len < t) => (value_start + len, false),
            _ => match tag_start {
                Some(t) => (value_start + t, false), // terminate at '<', don't continue
                None => (value_start + rest.len(), true),
            },
        };
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

fn all_html_diagnostics(text: &str, html_uri: &Url, class_map: &ClassMap) -> Vec<Diagnostic> {
    let mut diags = diagnostics_for_html(text, html_uri, class_map);
    diags.extend(diagnostics_for_duplicate_classes(text));
    diags
}

fn diagnostics_for_html(text: &str, html_uri: &Url, class_map: &ClassMap) -> Vec<Diagnostic> {
    // Parse inline <style> blocks for any URI scheme (file://, untitled:, etc.).
    let mut inline = ClassMap::new();
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
    for (base_line, content) in extract_style_blocks(text) {
        parse_css_content(&content, base_line, &source_file, &source_path, &mut inline);
    }

    html_selector_refs(text)
        .into_iter()
        .filter_map(|r| {
            let lookup = if r.is_id { format!("#{}", r.name) } else { r.name.clone() };
            if class_map.contains_key(&lookup) || inline.contains_key(&lookup) {
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
            let mut seen: HashSet<String> = HashSet::new();
            let quote = cap[1].chars().next().unwrap_or('"');
            let value_start = scan_from + cap.get(0).unwrap().end();
            let rest = &line[value_start..];
            let tag_start = rest.find('<');
            let (value_end, open) = match rest.find(quote) {
                Some(len) if tag_start.map_or(true, |t| len < t) => (value_start + len, false),
                _ => match tag_start {
                    Some(t) => (value_start + t, false),
                    None => (value_start + rest.len(), true),
                },
            };
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
                source: Some("css-lens".to_string()),
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
    root_path: Option<&Path>,
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

    // Suppress hints for classes/IDs referenced in vanilla JS (classList.add,
    // querySelector, getElementsByClassName, etc.) so DOM manipulation in plain
    // .js files doesn't produce false unused-selector warnings.
    if let Some(root) = root_path {
        used.extend(scan_js_used_classes(root));
    }

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
                source: Some("css-lens".to_string()),
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

    #[test]
    fn rule_extent_single_line() {
        let css = ".btn { color: red; }\n.other { }";
        assert_eq!(find_rule_extent(css, 0), Some((0, 0)));
    }

    #[test]
    fn rule_extent_multi_line() {
        let css = ".btn {\n  color: red;\n  font-size: 1rem;\n}\n.other { }";
        // .btn opens on line 0, closes on line 3
        assert_eq!(find_rule_extent(css, 0), Some((0, 3)));
    }

    #[test]
    fn rule_extent_skips_to_definition_line() {
        let css = ".first { color: red; }\n\n.second {\n  display: flex;\n}\n";
        // .second is at line 2, closes at line 4
        assert_eq!(find_rule_extent(css, 2), Some((2, 4)));
    }

    #[test]
    fn rule_extent_nested_media_query() {
        let css = "@media (max-width: 768px) {\n  .btn {\n    color: blue;\n  }\n}\n";
        // .btn is at line 1, its closing brace is line 3
        assert_eq!(find_rule_extent(css, 1), Some((1, 3)));
    }

    #[test]
    fn rule_extent_comment_with_braces() {
        let css = ".btn {\n  /* } this brace is a comment */\n  color: red;\n}\n";
        assert_eq!(find_rule_extent(css, 0), Some((0, 3)));
    }

    #[test]
    fn rule_extent_missing_line_returns_none() {
        let css = ".btn { color: red; }";
        assert_eq!(find_rule_extent(css, 5), None);
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

// ---------------------------------------------------------------------------
// Hover helpers
// ---------------------------------------------------------------------------

fn specificity(selector: &str) -> (u32, u32, u32) {
    let part = selector.split(',').next().unwrap_or(selector);
    let a = id_re().find_iter(part).count() as u32;
    let b = class_re().find_iter(part).count() as u32;
    // Count element-type selectors (c component) by stripping pseudo-selectors,
    // attribute selectors, class selectors, and ID selectors, then counting the
    // remaining word tokens. The universal selector * contributes 0 to specificity.
    let s = pseudo_re().replace_all(part, " ");
    let s = attr_selector_re().replace_all(&s, " ");
    let s = class_re().replace_all(&s, " ");
    let s = id_re().replace_all(&s, " ");
    let c = element_type_re()
        .find_iter(&s)
        .filter(|m| m.as_str() != "*")
        .count() as u32;
    (a, b, c)
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
