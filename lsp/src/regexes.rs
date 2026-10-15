// ---------------------------------------------------------------------------
// Static regexes — compiled once, reused across every request and file parse.
// ---------------------------------------------------------------------------

use std::sync::OnceLock;

use regex::Regex;

pub fn pseudo_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"::?[a-zA-Z][a-zA-Z0-9_-]*(?:\([^)]*\))?").unwrap())
}

pub fn class_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.[a-zA-Z][a-zA-Z0-9_-]*").unwrap())
}

pub fn id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"#[a-zA-Z][a-zA-Z0-9_-]*").unwrap())
}

pub fn class_attr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)class\s*=\s*(["'])"#).unwrap())
}

pub fn id_attr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\bid\s*=\s*(["'])"#).unwrap())
}

pub fn style_attr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\bstyle\s*=\s*(["'])"#).unwrap())
}

pub fn import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"@import\s+(?:url\()?["']([^"']+)["']"#).unwrap())
}

// Matches common CSS color values for display in hover tooltips.
pub fn color_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)#[0-9a-f]{3,8}\b|rgba?\s*\([^)]+\)|hsla?\s*\([^)]+\)").unwrap()
    })
}

// Matches a full <link .../> or <link ...> tag (including multi-line via [^>]).
pub fn link_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)<link\b[^>]*/?>").unwrap())
}

pub fn rel_stylesheet_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\brel\s*=\s*["']stylesheet["']"#).unwrap())
}

pub fn href_val_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\bhref\s*=\s*["']([^"']+)["']"#).unwrap())
}

// Matches the content of <style>...</style> blocks (case-insensitive, DOTALL).
pub fn style_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<style\b[^>]*>(.*?)</style>").unwrap())
}

pub fn keyframes_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)@keyframes\s+([a-zA-Z_-][a-zA-Z0-9_-]*)").unwrap())
}

pub fn attr_selector_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[[^\]]*\]").unwrap())
}

pub fn element_type_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b[a-zA-Z][a-zA-Z0-9]*\b").unwrap())
}

pub fn js_classlist_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"classList\s*\.\s*(?:add|remove|toggle|contains|replace)\s*\(\s*["']([^"']+)["']"#).unwrap()
    })
}

pub fn js_gebc_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"getElementsByClassName\s*\(\s*["']([^"']+)["']"#).unwrap()
    })
}

pub fn js_query_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"querySelectorAll?\s*\(\s*["']([^"']+)["']"#).unwrap()
    })
}
