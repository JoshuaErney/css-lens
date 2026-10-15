use std::collections::HashSet;

use lsp_server::{RequestId, Response};
use lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};
use serde_json::json;

use crate::cursor::{cursor_context, in_attr_before, style_context, word_at_ctx, StyleContext};
use crate::model::VarInfo;
use crate::regexes::{
    attr_selector_re, class_attr_re, class_re, color_re, element_type_re, id_attr_re, id_re, pseudo_re,
    style_attr_re,
};
use crate::server::{DocMaps, ServerState};

pub fn hover(id: RequestId, params: HoverParams, state: &ServerState, maps: &DocMaps) -> Response {
    let uri = &params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;

    let text = match state.documents.get(uri) {
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
                if let Some(defs) = state.css.vars.get(w.as_str()) {
                    return markdown(id, var_markdown(&w, defs));
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
                return match maps.vars.get(w.as_str()).or_else(|| state.css.vars.get(w.as_str())) {
                    Some(defs) => markdown(id, var_markdown(w, defs)),
                    None => Response::new_ok(id, json!(null)),
                };
            }
        }
        // animation-name value hover
        if let (Some(name), StyleContext::PropertyValue { property, .. }) = (word.as_deref(), style_context(text, pos)) {
            if property == "animation-name" {
                if let Some(info) = state.keyframes_map.get(name) {
                    return markdown(
                        id,
                        format!("**@keyframes {name}** — {}:{}", info.source_file, info.definition_line + 1),
                    );
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

    let infos = match maps.classes.get(&lookup_key) {
        Some(v) if !v.is_empty() => v,
        _ => return Response::new_ok(id, json!(null)),
    };

    let value = if infos.len() == 1 {
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

    markdown(id, value)
}

/// Hover text for a custom property, listing every declaration with the rule
/// it sits in, so theme overrides like `[data-theme="dark"]` are visible.
fn var_markdown(name: &str, defs: &[VarInfo]) -> String {
    if let [def] = defs {
        return format!(
            "**{name}** — {}:{} in `{}`\n\n```css\n{name}: {};\n```",
            def.source_file,
            def.definition_line + 1,
            def.selector,
            def.value,
        );
    }
    let mut parts = vec![format!("**{name}** — {} definitions", defs.len())];
    for (i, def) in defs.iter().enumerate() {
        parts.push(format!(
            "**{}.** {}:{} in `{}`\n```css\n{name}: {};\n```",
            i + 1,
            def.source_file,
            def.definition_line + 1,
            def.selector,
            def.value,
        ));
    }
    parts.join("\n\n")
}

fn markdown(id: RequestId, value: String) -> Response {
    let hover = Hover {
        contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value }),
        range: None,
    };
    Response::new_ok(id, serde_json::to_value(hover).unwrap_or(json!(null)))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specificity_counts_ids_classes_and_elements() {
        assert_eq!(specificity("#nav .btn:hover"), (1, 1, 0));
        assert_eq!(specificity("div.btn"), (0, 1, 1));
        assert_eq!(specificity("ul li a[href].link"), (0, 1, 3));
        assert_eq!(specificity("* .btn"), (0, 1, 0));
    }

    #[test]
    fn color_summary_dedupes_in_order() {
        let props = "color: #fff;\nbackground: rgb(0, 0, 0);\nborder-color: #fff;";
        assert_eq!(color_summary(props), "`#fff` `rgb(0, 0, 0)`");
    }
}
