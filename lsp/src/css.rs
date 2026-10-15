// ---------------------------------------------------------------------------
// CSS parsing
// ---------------------------------------------------------------------------

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use lsp_types::FileChangeType;

use crate::model::{ClassInfo, ClassMap, KeyframeInfo, KeyframesMap, VarMap};
use crate::regexes::{class_re, id_re, import_re, keyframes_re};
use crate::workspace::workspace_files;

/// Skips CSS files larger than 500 KB to avoid stalling on minified files.
const MAX_CSS_BYTES: u64 = 500 * 1024;

pub fn is_too_large(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0) > MAX_CSS_BYTES
}

pub fn is_css_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "css")
}

/// The file a parsed rule came from.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub file: &'a str,
    pub path: &'a str,
}

/// The at-rule context a rule is nested inside.
#[derive(Clone, Copy, Default)]
struct Scope<'a> {
    media_query: Option<&'a str>,
    layer: Option<&'a str>,
}

pub fn scan_directory(root: &Path, class_map: &mut ClassMap) {
    // A single shared visited set across all files prevents parsing the same
    // file twice when it appears directly in the workspace AND is @import-ed by
    // another file — which would otherwise produce false-positive duplicate-selector
    // warnings for every class in the imported file.
    let mut visited: HashSet<PathBuf> = HashSet::new();
    for path in workspace_files(root, &["css"]) {
        parse_css_file_inner(&path, class_map, &mut visited);
    }
}

/// Re-indexes one CSS file after it is created, changed, or deleted.
///
/// Every other already-indexed file is pre-seeded into the visited set so the
/// changed file's `@import`s are not parsed again — otherwise each save would
/// add a second copy of every selector in the imported files.
pub fn reload_css_file(path: &Path, typ: FileChangeType, class_map: &mut ClassMap) {
    let source_path = path.to_string_lossy().into_owned();
    for infos in class_map.values_mut() {
        infos.retain(|info| info.source_path != source_path);
    }
    class_map.retain(|_, infos| !infos.is_empty());

    if typ != FileChangeType::CREATED && typ != FileChangeType::CHANGED {
        return;
    }
    let indexed: HashSet<&str> = class_map
        .values()
        .flatten()
        .map(|info| info.source_path.as_str())
        .collect();
    let mut visited: HashSet<PathBuf> = indexed
        .into_iter()
        .filter_map(|p| Path::new(p).canonicalize().ok())
        .collect();
    parse_css_file_inner(path, class_map, &mut visited);
}

fn parse_css_file_inner(path: &Path, class_map: &mut ClassMap, visited: &mut HashSet<PathBuf>) {
    // Skip files that are too large (e.g. minified bundles).
    if is_too_large(path) {
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

    parse_css_content(&content, 0, Source { file: &source_file, path: &source_path }, class_map);

    let parent = path.parent().unwrap_or(Path::new("."));
    for import_path in extract_imports(&content) {
        parse_css_file_inner(&parent.join(&import_path), class_map, visited);
    }
}

pub fn parse_css_content(content: &str, base_line: u32, source: Source, class_map: &mut ClassMap) {
    let stripped = strip_comments(content);
    parse_rules_at_level(&stripped, base_line, Scope::default(), source, class_map);
}

fn parse_rules_at_level<'a>(
    content: &'a str,
    base_line: u32,
    scope: Scope<'a>,
    source: Source,
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
                    let media_query = if is_conditional { Some(pending) } else { scope.media_query };

                    // Track @layer name for selectors nested inside named layers.
                    let layer = match pending.strip_prefix("@layer").map(str::trim) {
                        Some(name) if !name.is_empty() => Some(name),
                        _ => scope.layer,
                    };

                    let block_start = i + 1;
                    i = advance_past_block(bytes, i + 1);
                    let block = &content[block_start..i];
                    let block_base = base_line + byte_offset_to_line(content, block_start);
                    parse_rules_at_level(block, block_base, Scope { media_query, layer }, source, class_map);
                    i += 1;
                } else if !pending.is_empty() {
                    let trim_offset = raw_chunk.len() - raw_chunk.trim_start().len();
                    let definition_line = base_line + byte_offset_to_line(content, chunk_start + trim_offset);

                    let props_start = i + 1;
                    i = advance_past_block(bytes, i + 1);
                    let properties = content[props_start..i].trim();

                    if !properties.is_empty() {
                        process_selector(pending, properties, definition_line, scope, source, class_map);
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
    scope: Scope,
    source: Source,
    class_map: &mut ClassMap,
) {
    // Use the full raw selector for display (shows :hover, ::before, :is() context, etc.)
    let selector_display = selector_raw.trim().to_string();
    let mut add = |name: String| {
        class_map.entry(name).or_default().push(ClassInfo {
            properties: properties.to_string(),
            selector: selector_display.clone(),
            media_query: scope.media_query.map(str::to_string),
            layer: scope.layer.map(str::to_string),
            source_file: source.file.to_string(),
            source_path: source.path.to_string(),
            definition_line,
        });
    };

    // Extract classes and IDs from the raw selector so that names inside
    // :is(), :has(), :where(), :not() are captured and added to the map.
    // class_re/id_re never false-match pseudo content since :hover, :nth-child(2n+1),
    // etc. contain no '.' or '#' tokens.
    for m in class_re().find_iter(selector_raw) {
        add(m.as_str()[1..].to_string());
    }
    for m in id_re().find_iter(selector_raw) {
        add(m.as_str().to_string());
    }
}

pub fn extract_imports(content: &str) -> Vec<String> {
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

// ---------------------------------------------------------------------------
// @keyframes
// ---------------------------------------------------------------------------

pub fn scan_keyframes(root: &Path) -> KeyframesMap {
    let mut map = KeyframesMap::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    for path in workspace_files(root, &["css"]) {
        scan_keyframes_file_inner(&path, &mut map, &mut visited);
    }
    map
}

/// Scans `path` for `@keyframes` declarations and follows `@import` chains,
/// mirroring the `visited`-set dedup that `parse_css_file_inner` uses.
fn scan_keyframes_file_inner(path: &Path, keyframes_map: &mut KeyframesMap, visited: &mut HashSet<PathBuf>) {
    if is_too_large(path) {
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
        let Some(whole) = cap.get(0) else { continue };
        keyframes_map.insert(cap[1].to_string(), KeyframeInfo {
            source_file: source_file.clone(),
            source_path: source_path.clone(),
            definition_line: byte_offset_to_line(&content, whole.start()),
        });
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    for import_path in extract_imports(&content) {
        scan_keyframes_file_inner(&parent.join(&import_path), keyframes_map, visited);
    }
}

/// Re-scans one CSS file (and its imports) for keyframes after it changes.
pub fn reload_keyframes_file(path: &Path, typ: FileChangeType, keyframes_map: &mut KeyframesMap) {
    let source_path = path.to_string_lossy().into_owned();
    keyframes_map.retain(|_, info| info.source_path != source_path);
    if typ == FileChangeType::CREATED || typ == FileChangeType::CHANGED {
        scan_keyframes_file_inner(path, keyframes_map, &mut HashSet::new());
    }
}

// ---------------------------------------------------------------------------
// CSS variable map
// ---------------------------------------------------------------------------

/// Rebuilds the CSS variable map from all property blobs in `class_map`.
/// Variables are discovered as `--name: value` lines inside any rule.
pub fn refresh_var_map(class_map: &ClassMap) -> VarMap {
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
// Rule extents
// ---------------------------------------------------------------------------

/// Scans `content` from `definition_line` to find the full rule block extent,
/// handling comments and string literals that may contain braces.
/// Returns `(definition_line, closing_brace_line)` as 0-indexed line numbers.
pub fn find_rule_extent(content: &str, definition_line: u32) -> Option<(u32, u32)> {
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
                depth = depth.saturating_sub(1);
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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn parse(css: &str) -> ClassMap {
        let mut map = ClassMap::new();
        parse_css_content(css, 0, Source { file: "t.css", path: "/t.css" }, &mut map);
        map
    }

    #[test]
    fn parses_media_and_layer_context() {
        let map = parse("@layer base {\n  @media (min-width: 1px) {\n    .btn { color: red; }\n  }\n}\n");
        let info = &map["btn"][0];
        assert_eq!(info.definition_line, 2);
        assert_eq!(info.layer.as_deref(), Some("base"));
        assert_eq!(info.media_query.as_deref(), Some("@media (min-width: 1px)"));
    }

    #[test]
    fn parses_classes_inside_functional_pseudo_classes() {
        let map = parse(".btn:is(.active, #main) { color: red; }");
        assert!(map.contains_key("btn"));
        assert!(map.contains_key("active"));
        assert!(map.contains_key("#main"));
    }

    #[test]
    fn comments_do_not_shift_line_numbers() {
        let map = parse("/* one\n two */\n.btn { color: red; }");
        assert_eq!(map["btn"][0].definition_line, 2);
    }
}
