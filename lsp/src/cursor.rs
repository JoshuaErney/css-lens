// ---------------------------------------------------------------------------
// Cursor context detection
// ---------------------------------------------------------------------------

use std::collections::HashSet;

use lsp_types::Position;
use regex::Regex;

use crate::regexes::{class_attr_re, id_attr_re, style_attr_re};

pub fn in_class_attribute(text: &str, pos: Position) -> bool {
    in_attr(text, pos, class_attr_re())
}

pub fn in_id_attribute(text: &str, pos: Position) -> bool {
    in_attr(text, pos, id_attr_re())
}

pub fn in_style_attribute(text: &str, pos: Position) -> bool {
    in_attr(text, pos, style_attr_re())
}

pub fn in_attr_before(before: &str, attr_re: &Regex) -> bool {
    match attr_re.captures_iter(before).last() {
        Some(cap) => {
            let quote = cap[1].chars().next().unwrap_or('"');
            let value_start = cap.get(0).map_or(0, |m| m.end());
            !before[value_start..].contains(quote)
        }
        None => false,
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
pub fn cursor_context(text: &str, pos: Position) -> Option<(String, &str, usize)> {
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

pub enum StyleContext {
    PropertyName { prefix: String },
    PropertyValue { property: String, prefix: String },
    None,
}

/// Returns the cursor's semantic position within a `style="..."` attribute value.
pub fn style_context(text: &str, pos: Position) -> StyleContext {
    let (before, _, _) = match cursor_context(text, pos) {
        Some(ctx) => ctx,
        None => return StyleContext::None,
    };
    let value_start = match style_attr_re().find_iter(&before).last() {
        Some(m) => m.end(),
        None => return StyleContext::None,
    };
    let value_fragment = &before[value_start..];

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

/// Splits the attribute value around the cursor for `attr_re`, returning
/// `(full_value, prefix)` where `prefix` is the identifier being typed.
fn attr_value_context(text: &str, pos: Position, attr_re: &Regex) -> Option<(String, String)> {
    let (before, line, col) = cursor_context(text, pos)?;
    let cap = attr_re.captures_iter(&before).last()?;
    let quote = cap[1].chars().next().unwrap_or('"');
    let value_before_cursor = &before[cap.get(0)?.end()..];
    let prefix = extract_prefix(value_before_cursor);
    let value_after = line[col..].split(quote).next().unwrap_or("");
    Some((format!("{value_before_cursor}{value_after}"), prefix))
}

/// Returns the lowercased class names already present in the `class=""` value
/// (excluding the one being typed) and the prefix being typed.
pub fn completion_context(text: &str, pos: Position) -> (HashSet<String>, String) {
    let Some((full_value, prefix)) = attr_value_context(text, pos, class_attr_re()) else {
        return (HashSet::new(), String::new());
    };
    let prefix_lower = prefix.to_lowercase();
    let existing: HashSet<String> = full_value
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| s != &prefix_lower)
        .collect();

    (existing, prefix)
}

/// Returns whether the `id=""` value already holds a value other than the one
/// being typed, and the prefix being typed.
pub fn id_completion_context(text: &str, pos: Position) -> (bool, String) {
    let Some((full_value, prefix)) = attr_value_context(text, pos, id_attr_re()) else {
        return (false, String::new());
    };
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

pub fn build_insert_text(name: &str, line: &str, col: usize) -> String {
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

pub fn word_at_ctx(line: &str, col: usize) -> Option<String> {
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

pub fn word_at(text: &str, pos: Position) -> Option<String> {
    let (_, line, col) = cursor_context(text, pos)?;
    word_at_ctx(line, col)
}

#[inline]
pub fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// Returns true if `s` is a safe ASCII CSS identifier.
/// Guards rename and code-action inputs against CSS syntax injection.
/// Per CSS Syntax Level 3, identifiers cannot start with an unescaped digit.
pub fn is_valid_css_ident(s: &str) -> bool {
    if s.is_empty() { return false; }
    let bytes = s.as_bytes();
    !bytes[0].is_ascii_digit()
        && bytes
            .iter()
            .all(|&b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn detects_attribute_context() {
        let html = r#"<div class="btn " id="hero" style="color: red">"#;
        assert!(in_class_attribute(html, pos(0, 15)));
        assert!(!in_class_attribute(html, pos(0, 17)));
        assert!(in_id_attribute(html, pos(0, 22)));
        assert!(in_style_attribute(html, pos(0, 36)));
    }

    #[test]
    fn utf16_columns_do_not_panic_on_multibyte_text() {
        let html = "<p class=\"é btn\">";
        // 'é' is one UTF-16 unit but two UTF-8 bytes; col 12 is inside "btn".
        assert_eq!(word_at(html, pos(0, 12)).as_deref(), Some("btn"));
    }

    #[test]
    fn class_completion_context_excludes_prefix() {
        let html = r#"<div class="card bt">"#;
        let (existing, prefix) = completion_context(html, pos(0, 19));
        assert_eq!(prefix, "bt");
        assert!(existing.contains("card"));
        assert!(!existing.contains("bt"));
    }

    #[test]
    fn style_context_splits_property_and_value() {
        let html = r#"<div style="color: red; display: fl">"#;
        match style_context(html, pos(0, 35)) {
            StyleContext::PropertyValue { property, prefix } => {
                assert_eq!(property, "display");
                assert_eq!(prefix, "fl");
            }
            _ => panic!("expected a property value context"),
        }
    }

    #[test]
    fn insert_text_pads_against_adjacent_names() {
        assert_eq!(build_insert_text("x", "ab", 1), " x ");
        assert_eq!(build_insert_text("x", "a ", 1), " x");
        assert_eq!(build_insert_text("x", " ", 0), "x");
    }

    #[test]
    fn css_ident_validation() {
        assert!(is_valid_css_ident("btn-primary_2"));
        assert!(!is_valid_css_ident("2col"));
        assert!(!is_valid_css_ident("a{}"));
        assert!(!is_valid_css_ident(""));
    }
}
