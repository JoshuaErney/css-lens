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

// Matches @import "path", @import 'path', @import url("path"), @import url('path').
fn import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"@import\s+(?:url\()?["']([^"']+)["']"#).unwrap())
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
    source_path: String,
    /// 0-based line number of the rule in the source file — used for go-to-definition.
    definition_line: u32,
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
        definition_provider: Some(OneOf::Left(true)),
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
        "textDocument/definition" => {
            match serde_json::from_value::<GotoDefinitionParams>(params.clone()) {
                Ok(p) => definition_handler(id, p, class_map, documents),
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

    let (existing_classes, prefix) = completion_context(text, pos);
    let prefix_lower = prefix.to_lowercase();

    let mut items: Vec<CompletionItem> = class_map
        .iter()
        .filter(|(name, _)| {
            let name_lower = name.to_lowercase();
            // Exclude classes already present in this attribute.
            !existing_classes.contains(&name_lower)
                // Case-insensitive prefix match against what's been typed so far.
                && (prefix.is_empty() || name_lower.starts_with(&prefix_lower))
        })
        .map(|(name, info)| {
            // Determine correct spacing around the inserted class name.
            let insert = build_insert_text(name, text, pos);
            CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some(info.source_file.clone()),
                insert_text: Some(insert),
                // filter_text is the label lowercased so the editor's own
                // filtering also behaves case-insensitively.
                filter_text: Some(name.to_lowercase()),
                ..Default::default()
            }
        })
        .collect();

    // Sort alphabetically so the list is stable and predictable.
    items.sort_by(|a, b| a.label.cmp(&b.label));

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

    if !in_class_attribute(text, pos) {
        return Response::new_ok(id, json!(null));
    }

    let class_name = match word_at(text, pos) {
        Some(w) => w,
        None => return Response::new_ok(id, json!(null)),
    };

    let info = match class_map.get(&class_name) {
        Some(i) => i,
        None => return Response::new_ok(id, json!(null)),
    };

    let def_uri = match Url::from_file_path(&info.source_path) {
        Ok(u) => u,
        Err(_) => return Response::new_ok(id, json!(null)),
    };

    let location = Location {
        uri: def_uri,
        range: Range {
            start: Position {
                line: info.definition_line,
                character: 0,
            },
            end: Position {
                line: info.definition_line,
                character: 0,
            },
        },
    };

    let result = serde_json::to_value(GotoDefinitionResponse::Scalar(location))
        .unwrap_or(json!(null));
    Response::new_ok(id, result)
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

    let mut before = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i < line_idx {
            before.push_str(line);
            before.push('\n');
        } else {
            let col = (pos.character as usize).min(line.len());
            before.push_str(&line[..col]);
            break;
        }
    }

    if let Some(m) = class_attr_re().captures_iter(&before).last() {
        let quote = m[1].chars().next().unwrap_or('"');
        let after_quote = &before[m.get(0).unwrap().end()..];
        !after_quote.contains(quote)
    } else {
        false
    }
}

/// Returns the completion context: (already-present classes, current prefix).
///
/// `existing_classes` are the class names already in the attribute (lowercased)
/// excluding the partial word at the cursor so the user can still complete it.
/// `prefix` is the partial word immediately before the cursor (may be empty).
fn completion_context(text: &str, pos: Position) -> (HashSet<String>, String) {
    let lines: Vec<&str> = text.lines().collect();
    let line_idx = pos.line as usize;

    if line_idx >= lines.len() {
        return (HashSet::new(), String::new());
    }

    let line = lines[line_idx];
    let col = (pos.character as usize).min(line.len());

    // Text up to the cursor across all lines.
    let mut before = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i < line_idx {
            before.push_str(l);
            before.push('\n');
        } else {
            before.push_str(&l[..col]);
            break;
        }
    }

    let last_match = match class_attr_re().captures_iter(&before).last() {
        Some(m) => m,
        None => return (HashSet::new(), String::new()),
    };

    let quote = last_match[1].chars().next().unwrap_or('"');
    let attr_value_start = last_match.get(0).unwrap().end();
    let value_before_cursor = &before[attr_value_start..];

    // Partial word immediately before the cursor (what the user is typing).
    let prefix = {
        let bytes = value_before_cursor.as_bytes();
        let end = bytes.len();
        if end == 0 || !is_ident_byte(bytes[end - 1]) {
            String::new()
        } else {
            let start = (0..end)
                .rev()
                .find(|&i| !is_ident_byte(bytes[i]))
                .map(|i| i + 1)
                .unwrap_or(0);
            value_before_cursor[start..].to_string()
        }
    };

    // Full attribute value = what's before cursor + what's after cursor up to
    // the closing quote. Assumes the attribute fits on one line (typical).
    let value_after = line[col..]
        .splitn(2, quote)
        .next()
        .unwrap_or("");
    let full_value = format!("{}{}", value_before_cursor, value_after);

    // All classes already in the attribute, excluding the one being typed.
    let prefix_lower = prefix.to_lowercase();
    let existing: HashSet<String> = full_value
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| s != &prefix_lower)
        .collect();

    (existing, prefix)
}

/// Determines the correct insert text for a completion item, adding a leading
/// or trailing space when the cursor is adjacent to other class names.
fn build_insert_text(name: &str, text: &str, pos: Position) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let line_idx = pos.line as usize;

    if line_idx >= lines.len() {
        return name.to_string();
    }

    let line = lines[line_idx];
    let col = (pos.character as usize).min(line.len());
    let bytes = line.as_bytes();

    // Character immediately before and after the cursor.
    let before_char = if col > 0 { bytes.get(col - 1).copied() } else { None };
    let after_char = bytes.get(col).copied();

    // Need a leading space if the cursor is flush against an existing class.
    let needs_space_before = before_char.map_or(false, is_ident_byte);
    // Need a trailing space if the next character is the start of another class.
    let needs_space_after = after_char.map_or(false, is_ident_byte);

    match (needs_space_before, needs_space_after) {
        (true, true) => format!(" {} ", name),
        (true, false) => format!(" {}", name),
        (false, true) => format!("{} ", name),
        (false, false) => name.to_string(),
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
/// Symlinks are NOT followed. `node_modules` and hidden directories are skipped.
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

/// Reads a single CSS file and merges its classes into `class_map`, following
/// `@import` statements recursively. Uses `visited` to prevent cycles.
fn parse_css_file(path: &Path, class_map: &mut ClassMap) {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    parse_css_file_inner(path, class_map, &mut visited);
}

fn parse_css_file_inner(path: &Path, class_map: &mut ClassMap, visited: &mut HashSet<PathBuf>) {
    // Canonicalize to resolve symlinks and `..` so cycle detection is reliable.
    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return,
    };

    if !visited.insert(canonical) {
        return; // already processed — skip to break cycles
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

    // Follow @import statements relative to this file's directory.
    let parent = path.parent().unwrap_or(Path::new("."));
    for import_path in extract_imports(&content) {
        let imported = parent.join(&import_path);
        parse_css_file_inner(&imported, class_map, visited);
    }
}

/// Extracts local @import paths from CSS content.
/// Skips URL imports (http/https/protocol-relative).
fn extract_imports(content: &str) -> Vec<String> {
    import_re()
        .captures_iter(content)
        .filter_map(|cap| cap.get(1))
        .map(|m| m.as_str().to_string())
        .filter(|p| {
            !p.starts_with("http://")
                && !p.starts_with("https://")
                && !p.starts_with("//")
        })
        .map(|mut p| {
            // Some authors omit the .css extension: `@import "variables"`.
            if !p.contains('.') {
                p.push_str(".css");
            }
            p
        })
        .collect()
}

/// Strips CSS block comments while preserving newlines so that line numbers
/// in the stripped content match the original file.
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
                result.push('\n'); // preserve newlines for line-number accuracy
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

/// Returns the 0-based line number of `offset` within `content`.
fn byte_offset_to_line(content: &str, offset: usize) -> u32 {
    content[..offset.min(content.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32
}

/// Parses CSS text and inserts discovered classes into `class_map`.
fn parse_css_content(
    content: &str,
    source_file: &str,
    source_path: &str,
    class_map: &mut ClassMap,
) {
    let stripped = strip_comments(content);

    for cap in rule_re().captures_iter(&stripped) {
        let selector_raw = &cap[1];
        let properties = cap[2].trim().to_string();

        if properties.is_empty() {
            continue;
        }

        // Skip leading whitespace in the selector to find the actual rule line.
        let rule_offset = cap.get(1).unwrap().start();
        let ws_skip = selector_raw.len() - selector_raw.trim_start().len();
        let definition_line = byte_offset_to_line(&stripped, rule_offset + ws_skip);

        let selector = pseudo_re().replace_all(selector_raw, "");

        for class_match in class_re().find_iter(&selector) {
            let name = &class_match.as_str()[1..]; // strip leading `.`

            class_map.insert(
                name.to_string(),
                ClassInfo {
                    properties: properties.clone(),
                    source_file: source_file.to_string(),
                    source_path: source_path.to_string(),
                    definition_line,
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

        class_map.retain(|_, info| info.source_path != source_path);

        if change.typ == FileChangeType::CREATED || change.typ == FileChangeType::CHANGED {
            parse_css_file(&path, class_map);
        }
    }
}
