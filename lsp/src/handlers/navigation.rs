use lsp_server::{RequestId, Response};
use lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, Location, ReferenceParams, Url};
use serde_json::json;

use super::attr_key_at;
use crate::cursor::{cursor_context, in_style_attribute, style_context, word_at_ctx, StyleContext};
use crate::model::line_start;
use crate::server::{DocMaps, ServerState};
use crate::workspace::workspace_html_refs;

pub fn definition(id: RequestId, params: GotoDefinitionParams, state: &ServerState, maps: &DocMaps) -> Response {
    let uri = &params.text_document_position_params.text_document.uri;
    let pos = params.text_document_position_params.position;

    let text = match state.documents.get(uri) {
        Some(t) => t,
        None => return Response::new_ok(id, json!(null)),
    };

    // style="animation-name: <cursor>" — jump to @keyframes definition
    if in_style_attribute(text, pos) {
        if let StyleContext::PropertyValue { property, .. } = style_context(text, pos) {
            if property == "animation-name" {
                let name = cursor_context(text, pos).and_then(|(_, line, col)| word_at_ctx(line, col));
                let loc = name
                    .and_then(|n| state.keyframes_map.get(&n))
                    .and_then(|info| {
                        Some(Location {
                            uri: Url::from_file_path(&info.source_path).ok()?,
                            range: line_start(info.definition_line),
                        })
                    });
                if let Some(loc) = loc {
                    return Response::new_ok(
                        id,
                        serde_json::to_value(GotoDefinitionResponse::Scalar(loc)).unwrap_or(json!(null)),
                    );
                }
            }
        }
        return Response::new_ok(id, json!(null));
    }

    let lookup_key = match attr_key_at(text, pos) {
        Some(k) => k,
        None => return Response::new_ok(id, json!(null)),
    };

    let infos = match maps.classes.get(&lookup_key) {
        Some(v) if !v.is_empty() => v,
        _ => return Response::new_ok(id, json!(null)),
    };

    let mut locations: Vec<Location> = infos
        .iter()
        .filter_map(|info| {
            Some(Location {
                uri: Url::from_file_path(&info.source_path).ok()?,
                range: line_start(info.definition_line),
            })
        })
        .collect();

    let result = match locations.len() {
        0 => json!(null),
        1 => serde_json::to_value(GotoDefinitionResponse::Scalar(locations.remove(0)))
            .unwrap_or(json!(null)),
        _ => serde_json::to_value(GotoDefinitionResponse::Array(locations))
            .unwrap_or(json!(null)),
    };
    Response::new_ok(id, result)
}

pub fn references(id: RequestId, params: ReferenceParams, state: &ServerState) -> Response {
    let uri = &params.text_document_position.text_document.uri;
    let pos = params.text_document_position.position;

    let text = match state.documents.get(uri) {
        Some(t) => t,
        None => return Response::new_ok(id, json!([])),
    };

    let (lookup_key, root) = match (attr_key_at(text, pos), state.root.as_deref()) {
        (Some(k), Some(r)) => (k, r),
        _ => return Response::new_ok(id, json!([])),
    };

    // A fresh walk rather than the usage index: references and rename need exact
    // token positions in every file, and they run on demand, not per keystroke.
    let locations = workspace_html_refs(&lookup_key, root, &state.documents);
    Response::new_ok(id, serde_json::to_value(locations).unwrap_or(json!([])))
}
