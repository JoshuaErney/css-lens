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
    /// Full selector string (pseudos stripped), e.g. `.btn.btn--primary` or `#hero`.
    selector: String,
    /// @media or @supports condition if the rule is nested inside one.
    media_query: Option<String>,
    /// Basename of the CSS file — used for display only (e.g. "styles.css").
    source_file: String,
    /// Full canonical path — used to remove stale entries when a file changes.
    source_path: String,
    /// 0-based line number of the rule in the source file.
    definition_line: u32,
}

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

    if in_class_attribute(text, pos) {
        let (existing_classes, prefix) = completion_context(text, pos);
        let prefix_lower = prefix.to_lowercase();

        let mut items: Vec<CompletionItem> = class_map
            .iter()
            .filter(|(name, _)| !name.starts_with('#'))
            .filter(|(name, _)| {
                let name_lower = name.to_lowercase();
                !existing_classes.contains(&name_lower)
                    && (prefix.is_empty() || name_lower.starts_with(&prefix_lower))
            })
            .map(|(name, infos)| {
                let insert = build_insert_text(name, text, pos);
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

    if in_id_attribute(text, pos) {
        let (has_existing_value, prefix) = id_completion_context(text, pos);
        // id="" is single-value — stop suggesting once a complete value exists.
        if has_existing_value {
            return Response::new_ok(id, json!(null));
        }
        let prefix_lower = prefix.to_lowercase();

        let mut items: Vec<CompletionItem> = class_map
            .iter()
            .filter(|(name, _)| name.starts_with('#'))
            .filter(|(name, _)| {
                let bare_lower = name[1..].to_lowercase(); // strip leading `#`
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

    Response::new_ok(id, json!(null))
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

    let markdown = if infos.len() == 1 {
        let info = &infos[0];
        let mq = info
            .media_query
            .as_deref()
            .map(|mq| format!("\n_inside_ `{mq}`"))
            .unwrap_or_default();
        format!(
            "**{}** — {}:{}{}\n\n```css\n{} {{\n{}\n}}\n```",
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
            parts.push(format!(
                "**{}.** {}:{}{}\n```css\n{} {{\n{}\n}}\n```",
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

// ---------------------------------------------------------------------------
// Context detection — are we inside class="..." or id="..."?
// ---------------------------------------------------------------------------

fn in_class_attribute(text: &str, pos: Position) -> bool {
    in_attr(text, pos, class_attr_re())
}

fn in_id_attribute(text: &str, pos: Position) -> bool {
    in_attr(text, pos, id_attr_re())
}

fn in_attr(text: &str, pos: Position, attr_re: &Regex) -> bool {
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
    if let Some(m) = attr_re.captures_iter(&before).last() {
        let quote = m[1].chars().next().unwrap_or('"');
        let after_quote = &before[m.get(0).unwrap().end()..];
        !after_quote.contains(quote)
    } else {
        false
    }
}

/// Returns the completion context for a `class="..."` attribute:
/// the set of already-present class names (lowercased, excluding the partial
/// word at the cursor) and the partial word being typed.
fn completion_context(text: &str, pos: Position) -> (HashSet<String>, String) {
    let lines: Vec<&str> = text.lines().collect();
    let line_idx = pos.line as usize;
    if line_idx >= lines.len() {
        return (HashSet::new(), String::new());
    }
    let line = lines[line_idx];
    let col = (pos.character as usize).min(line.len());

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

/// Returns `(has_existing_value, prefix)` for an `id="..."` attribute.
/// `has_existing_value` is true when a complete word other than the current
/// prefix already occupies the attribute — callers should suppress completions.
fn id_completion_context(text: &str, pos: Position) -> (bool, String) {
    let lines: Vec<&str> = text.lines().collect();
    let line_idx = pos.line as usize;
    if line_idx >= lines.len() {
        return (false, String::new());
    }
    let line = lines[line_idx];
    let col = (pos.character as usize).min(line.len());

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

/// Returns the CSS identifier fragment immediately before the end of `s`.
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

/// Determines the insert text for a class completion, adding spacing when the
/// cursor is flush against existing class names.
fn build_insert_text(name: &str, text: &str, pos: Position) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let line_idx = pos.line as usize;
    if line_idx >= lines.len() {
        return name.to_string();
    }
    let line = lines[line_idx];
    let col = (pos.character as usize).min(line.len());
    let bytes = line.as_bytes();

    let before_char = if col > 0 { bytes.get(col - 1).copied() } else { None };
    let after_char = bytes.get(col).copied();

    let needs_space_before = before_char.map_or(false, is_ident_byte);
    let needs_space_after = after_char.map_or(false, is_ident_byte);

    match (needs_space_before, needs_space_after) {
        (true, true) => format!(" {name} "),
        (true, false) => format!(" {name}"),
        (false, true) => format!("{name} "),
        (false, false) => name.to_string(),
    }
}

/// Returns the CSS identifier word at `pos`.
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

/// Reads a CSS file and merges its selectors into `class_map`, following
/// `@import` statements recursively. Uses `visited` to prevent cycles.
fn parse_css_file(path: &Path, class_map: &mut ClassMap) {
    let mut visited: HashSet<PathBuf> = HashSet::new();
    parse_css_file_inner(path, class_map, &mut visited);
}

fn parse_css_file_inner(path: &Path, class_map: &mut ClassMap, visited: &mut HashSet<PathBuf>) {
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

/// Recursively walks a CSS block, extracting class and ID selectors.
///
/// `base_line` is the 0-based line offset of `content` within the original
/// file, used to produce accurate definition line numbers after recursing into
/// `@media` / `@supports` blocks.
///
/// Keys for classes are stored as bare names (`btn`); keys for IDs include the
/// leading `#` (`#hero`) so they live in the same map without collision.
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
            // Skip string literals so braces inside them don't confuse the parser.
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 1; // skip escaped character
                    } else if bytes[i] == quote {
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }

            // A semicolon ends single-line @-rules (@charset, @import, @namespace).
            // Reset the accumulator so the following selector text is clean.
            b';' => {
                i += 1;
                chunk_start = i;
            }

            b'{' => {
                let raw_chunk = &content[chunk_start..i];
                let pending = raw_chunk.trim();

                if pending.starts_with('@') {
                    // Determine whether this @-rule carries a media query context.
                    let is_conditional = pending.starts_with("@media")
                        || pending.starts_with("@supports");
                    let child_mq = if is_conditional {
                        Some(pending)
                    } else {
                        media_query // @layer, @container, etc. inherit parent context
                    };

                    // Find the matching closing brace.
                    let block_start = i + 1;
                    i = advance_past_block(bytes, i + 1);
                    let block = &content[block_start..i];
                    let block_base = base_line + byte_offset_to_line(content, block_start);
                    parse_rules_at_level(block, block_base, child_mq, source_file, source_path, class_map);
                    i += 1; // skip the closing `}`
                } else if !pending.is_empty() {
                    // Regular CSS rule: collect properties up to the matching `}`.
                    let trim_offset = raw_chunk.len() - raw_chunk.trim_start().len();
                    let definition_line = base_line + byte_offset_to_line(content, chunk_start + trim_offset);

                    let props_start = i + 1;
                    i = advance_past_block(bytes, i + 1);
                    let properties = content[props_start..i].trim();

                    if !properties.is_empty() {
                        process_selector(pending, properties, definition_line, media_query, source_file, source_path, class_map);
                    }
                    i += 1; // skip the closing `}`
                } else {
                    i += 1;
                }
                chunk_start = i;
            }

            b'}' => {
                // Stray closing brace — reset accumulator.
                i += 1;
                chunk_start = i;
            }

            _ => {
                i += 1;
            }
        }
    }
}

/// Advances `i` past a `{...}` block (handling nesting and string literals),
/// stopping with `i` pointing at the matching `}`.
fn advance_past_block(bytes: &[u8], mut i: usize) -> usize {
    let mut depth = 1usize;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'"' | b'\'' => {
                let q = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 1;
                    } else if bytes[i] == q {
                        break;
                    }
                    i += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// Extracts class and ID selectors from `selector_raw` and inserts them into
/// `class_map`. Classes are keyed by bare name; IDs include the leading `#`.
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
        let name = m.as_str()[1..].to_string(); // strip leading `.`
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
        let name = m.as_str().to_string(); // keep the `#` as part of the key
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

/// Returns the 0-based line number of `offset` within `content`.
fn byte_offset_to_line(content: &str, offset: usize) -> u32 {
    content[..offset.min(content.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32
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

        for infos in class_map.values_mut() {
            infos.retain(|info| info.source_path != source_path);
        }
        class_map.retain(|_, infos| !infos.is_empty());

        if change.typ == FileChangeType::CREATED || change.typ == FileChangeType::CHANGED {
            parse_css_file(&path, class_map);
        }
    }
}
