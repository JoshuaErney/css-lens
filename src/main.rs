use std::collections::HashMap;
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

fn comment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/\*[^*]*\*+(?:[^/*][^*]*\*+)*/").unwrap())
}

fn rule_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([^{}@]+)\{([^{}]*)\}").unwrap())
}

fn pseudo_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"::?[a-zA-Z][a-zA-Z0-9_-]*(?:\([^)]*\))?").unwrap())
}

fn class_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.[a-zA-Z][a-zA-Z0-9_-]*").unwrap())
}

fn class_attr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)class\s*=\s*(["'])"#).unwrap())
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ClassInfo {
    /// Raw property declarations inside the `{ }` block (trimmed).
    properties: String,
    /// Basename of the CSS file — used for display only (e.g. "styles.css").
    source_file: String,
    /// Full canonical path — used to remove stale entries when a file changes.
    /// Stored as a String to avoid PathBuf hashing complexity.
    source_path: String,
}

type ClassMap = HashMap<String, ClassInfo>;
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
        ..Default::default()
    };

    let (init_id, init_params_raw) = connection.initialize_start()?;
    let init_params: InitializeParams = serde_json::from_value(init_params_raw)?;

    // Prefer workspace_folders (modern); fall back to root_uri (deprecated but
    // still set by most clients including Zed).
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
                    &documents,
                );
                connection.sender.send(Message::Response(resp))?;
            }
            Message::Notification(notif) => {
                route_notification(&notif.method, notif.params, &mut class_map, &mut documents);
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
    documents: &DocumentMap,
) -> Response {
    match method {
        "textDocument/completion" => {
            match serde_json::from_value::<CompletionParams>(params.clone()) {
                Ok(p) => completion_handler(id, p, class_map, documents),
                Err(e) => Response::new_err(id, -32602, e.to_string()),
            }
        }
        "textDocument/hover" => {
            match serde_json::from_value::<HoverParams>(params.clone()) {
                Ok(p) => hover_handler(id, p, class_map, documents),
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
    documents: &mut DocumentMap,
) {
    match method {
        "textDocument/didOpen" => {
            if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(params) {
                documents.insert(p.text_document.uri, p.text_document.text);
            }
        }
        "textDocument/didChange" => {
            if let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(params) {
                // FULL sync — the last entry is the complete document.
                if let Some(last) = p.content_changes.into_iter().last() {
                    documents.insert(p.text_document.uri, last.text);
                }
            }
        }
        "textDocument/didClose" => {
            if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(params) {
                documents.remove(&p.text_document.uri);
            }
        }
        "workspace/didChangeWatchedFiles" => {
            if let Ok(p) = serde_json::from_value::<DidChangeWatchedFilesParams>(params) {
                update_css_map(&p, class_map);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// LSP handlers
// ---------------------------------------------------------------------------

fn completion_handler(
    id: RequestId,
    params: CompletionParams,
    class_map: &ClassMap,
    documents: &DocumentMap,
) -> Response {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;

    let text = match documents.get(uri) {
        Some(t) => t,
        None => return Response::new_ok(id, json!(null)),
    };

    if !in_class_attribute(text, pos) {
        return Response::new_ok(id, json!(null));
    }

    let items: Vec<CompletionItem> = class_map
        .iter()
        .map(|(name, info)| CompletionItem {
            label: name.clone(),
            kind: Some(CompletionItemKind::CLASS),
            // The Wasm extension reads this field in label_for_completion.
            detail: Some(info.source_file.clone()),
            insert_text: Some(name.clone()),
            ..Default::default()
        })
        .collect();

    let result = serde_json::to_value(CompletionResponse::Array(items)).unwrap_or(json!(null));
    Response::new_ok(id, result)
}

fn hover_handler(
    id: RequestId,
    params: HoverParams,
    class_map: &ClassMap,
    documents: &DocumentMap,
) -> Response {
    let uri = &params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;

    let text = match documents.get(uri) {
        Some(t) => t,
        None => return Response::new_ok(id, json!(null)),
    };

    if !in_class_attribute(text, pos) {
        return Response::new_ok(id, json!(null));
    }

    let class_name = match word_at(text, pos) {
        Some(w) => w,
        None => return Response::new_ok(id, json!(null)),
    };

    let info = match class_map.get(&class_name) {
        Some(i) => i,
        // LSP spec: return null (not an error) for unknown classes.
        None => return Response::new_ok(id, json!(null)),
    };

    let markdown = format!(
        "**{}** — {}\n\n```css\n.{} {{\n{}\n}}\n```",
        class_name, info.source_file, class_name, info.properties
    );

    let hover = Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: None,
    };

    Response::new_ok(id, serde_json::to_value(hover).unwrap_or(json!(null)))
}

// ---------------------------------------------------------------------------
// Context detection — are we inside class="..." ?
// ---------------------------------------------------------------------------

/// Returns true when `pos` lies within the value of an HTML `class` attribute.
fn in_class_attribute(text: &str, pos: Position) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    let line_idx = pos.line as usize;

    if line_idx >= lines.len() {
        return false;
    }

    // Collect all text up to (but not including) the cursor character.
    let mut before = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i < line_idx {
            before.push_str(line);
            before.push('\n');
        } else {
            // Clamp to actual line length to avoid panics on surrogate offsets.
            let col = (pos.character as usize).min(line.len());
            before.push_str(&line[..col]);
            break;
        }
    }

    if let Some(m) = class_attr_re().captures_iter(&before).last() {
        let quote = m[1].chars().next().unwrap_or('"');
        let after_quote = &before[m.get(0).unwrap().end()..];
        // Inside the value if the opening quote has not been closed yet.
        !after_quote.contains(quote)
    } else {
        false
    }
}

/// Returns the CSS-identifier word (alphanumeric, `-`, `_`) at `pos`.
fn word_at(text: &str, pos: Position) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let line_idx = pos.line as usize;

    if line_idx >= lines.len() {
        return None;
    }

    let line = lines[line_idx];
    let col = pos.character as usize;

    // CSS identifiers are ASCII, so byte indexing is safe here.
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

#[inline]
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

// ---------------------------------------------------------------------------
// CSS parsing
// ---------------------------------------------------------------------------

/// Walks `root` recursively and parses every `.css` file found.
///
/// Symlinks are NOT followed — a symlink could point outside the workspace.
/// Directories named `node_modules` or starting with `.` are skipped entirely
/// so dependency CSS files don't pollute completions or slow startup.
fn scan_directory(root: &Path, class_map: &mut ClassMap) {
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
        parse_css_file(entry.path(), class_map);
    }
}

/// Reads a single CSS file and merges its classes into `class_map`.
fn parse_css_file(path: &Path, class_map: &mut ClassMap) {
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
}

/// Parses CSS text and inserts discovered classes into `class_map`.
///
/// Approach (regex-only, no external CSS parser):
///   1. Strip block comments.
///   2. Match non-nested rule blocks: `selector { declarations }`.
///      At-rules (`@media`, `@keyframes`, …) are excluded at the selector
///      level, but inner rule blocks within them are still matched.
///   3. Strip pseudo-classes/elements from the selector.
///   4. Extract every `.identifier` from the cleaned selector.
fn parse_css_content(
    content: &str,
    source_file: &str,
    source_path: &str,
    class_map: &mut ClassMap,
) {
    let stripped = comment_re().replace_all(content, " ");

    for cap in rule_re().captures_iter(&stripped) {
        let selector_raw = &cap[1];
        let properties = cap[2].trim().to_string();

        if properties.is_empty() {
            continue;
        }

        let selector = pseudo_re().replace_all(selector_raw, "");

        for class_match in class_re().find_iter(&selector) {
            let name = &class_match.as_str()[1..]; // strip leading `.`

            // Always overwrite so the last definition wins (mirrors CSS cascade).
            class_map.insert(
                name.to_string(),
                ClassInfo {
                    properties: properties.clone(),
                    source_file: source_file.to_string(),
                    source_path: source_path.to_string(),
                },
            );
        }
    }
}

// ---------------------------------------------------------------------------
// File watcher — incremental CSS map updates
// ---------------------------------------------------------------------------

/// Handles `workspace/didChangeWatchedFiles`. Only the affected file is
/// re-parsed; the rest of the map is untouched.
///
/// Entries are keyed by full path (not just basename) so two files named
/// `styles.css` in different directories don't interfere with each other.
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

        // Remove stale entries for this specific file using the full path,
        // so identically-named files in different directories are unaffected.
        class_map.retain(|_, info| info.source_path != source_path);

        if change.typ == FileChangeType::CREATED || change.typ == FileChangeType::CHANGED {
            parse_css_file(&path, class_map);
        }
        // FileChangeType::DELETED: entries already removed above.
    }
}
