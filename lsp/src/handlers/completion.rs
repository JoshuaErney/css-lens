use lsp_server::{RequestId, Response};
use lsp_types::{CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse};
use serde_json::json;

use crate::cursor::{
    build_insert_text, completion_context, cursor_context, id_completion_context, in_class_attribute,
    in_id_attribute, in_style_attribute, style_context, StyleContext,
};
use crate::server::{DocMaps, ServerState};

pub fn completion(id: RequestId, params: CompletionParams, state: &ServerState, maps: &DocMaps) -> Response {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;

    let text = match state.documents.get(uri) {
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

        let items = maps
            .classes
            .iter()
            .filter(|(name, _)| !name.starts_with('#'))
            .filter(|(name, _)| {
                let name_lower = name.to_lowercase();
                !existing_classes.contains(&name_lower)
                    && (prefix.is_empty() || name_lower.starts_with(&prefix_lower))
            })
            .map(|(name, infos)| CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::CLASS),
                detail: infos.first().map(|i| i.source_file.clone()),
                insert_text: Some(build_insert_text(name, ins_line, ins_col)),
                filter_text: Some(name.to_lowercase()),
                ..Default::default()
            })
            .collect();
        return respond(id, items);
    }

    // id="..." — offer ID names (single-value guard)
    if in_id_attribute(text, pos) {
        let (has_existing_value, prefix) = id_completion_context(text, pos);
        if has_existing_value {
            return Response::new_ok(id, json!(null));
        }
        let prefix_lower = prefix.to_lowercase();

        let items = maps
            .classes
            .iter()
            .filter_map(|(name, infos)| Some((name.strip_prefix('#')?, infos)))
            .filter(|(bare, _)| prefix.is_empty() || bare.to_lowercase().starts_with(&prefix_lower))
            .map(|(bare, infos)| CompletionItem {
                label: bare.to_string(),
                kind: Some(CompletionItemKind::VALUE),
                detail: infos.first().map(|i| i.source_file.clone()),
                insert_text: Some(bare.to_string()),
                filter_text: Some(bare.to_lowercase()),
                ..Default::default()
            })
            .collect();
        return respond(id, items);
    }

    // style="..." — property names, keyword values, CSS variables, and animation names
    if in_style_attribute(text, pos) {
        match style_context(text, pos) {
            StyleContext::PropertyName { prefix } => {
                let prefix_lower = prefix.to_lowercase();
                let items = css_property_completions()
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
                return respond(id, items);
            }
            StyleContext::PropertyValue { property, prefix } => {
                let prefix_lower = prefix.to_lowercase();
                // CSS variable completions inside any value (e.g. `color: var(--`)
                if prefix.starts_with("--") {
                    let items = maps
                        .vars
                        .iter()
                        .filter(|(name, _)| name.to_lowercase().starts_with(&prefix_lower))
                        .map(|(name, defs)| CompletionItem {
                            label: name.clone(),
                            kind: Some(CompletionItemKind::VARIABLE),
                            detail: defs.first().map(|d| d.value.clone()),
                            insert_text: Some(name.clone()),
                            filter_text: Some(name.to_lowercase()),
                            ..Default::default()
                        })
                        .collect();
                    return respond(id, items);
                }
                // animation-name → offer @keyframes names
                if property == "animation-name" {
                    let items = state
                        .keyframes_map
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
                    return respond(id, items);
                }
                // Keyword value completions for known properties
                let values = css_value_completions(&property);
                if !values.is_empty() {
                    let items = values
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
                    return respond(id, items);
                }
            }
            StyleContext::None => {}
        }
    }

    Response::new_ok(id, json!(null))
}

fn respond(id: RequestId, mut items: Vec<CompletionItem>) -> Response {
    items.sort_by(|a, b| a.label.cmp(&b.label));
    Response::new_ok(
        id,
        serde_json::to_value(CompletionResponse::Array(items)).unwrap_or(json!(null)),
    )
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
