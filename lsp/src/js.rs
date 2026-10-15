// ---------------------------------------------------------------------------
// JavaScript class references
// ---------------------------------------------------------------------------

use std::collections::HashSet;

use crate::regexes::{class_re, id_re, js_classlist_re, js_gebc_re, js_query_re};

/// Collects class names (bare) and IDs (`#`-prefixed) that vanilla DOM code
/// references, so the unused-selector hint can skip them.
pub fn collect_js_class_refs(content: &str, used: &mut HashSet<String>) {
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
