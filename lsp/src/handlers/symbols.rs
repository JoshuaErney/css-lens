use lsp_server::{RequestId, Response};
use lsp_types::{
    CodeLens, CodeLensParams, Command, DocumentSymbolParams, DocumentSymbolResponse, Location, SymbolInformation,
    SymbolKind, Url, WorkspaceSymbolParams,
};
use serde_json::json;

use crate::css::is_css_path;
use crate::model::{display_key, line_start};
use crate::server::ServerState;

/// The path of a `.css` document as stored in `ClassInfo::source_path`.
fn css_source_path(uri: &Url) -> Option<String> {
    let path = uri.to_file_path().ok()?;
    is_css_path(&path).then(|| path.to_string_lossy().into_owned())
}

pub fn document_symbol(id: RequestId, params: DocumentSymbolParams, state: &ServerState) -> Response {
    let uri = &params.text_document.uri;
    // Only CSS files get outline symbols from class_map.
    let source_path = match css_source_path(uri) {
        Some(p) => p,
        None => return Response::new_ok(id, json!([])),
    };

    let mut symbols: Vec<SymbolInformation> = Vec::new();
    for (name, infos) in &state.css.classes {
        for info in infos {
            if info.source_path != source_path {
                continue;
            }
            #[allow(deprecated)]
            symbols.push(SymbolInformation {
                name: display_key(name),
                kind: SymbolKind::CLASS,
                location: Location {
                    uri: uri.clone(),
                    range: line_start(info.definition_line),
                },
                tags: None,
                deprecated: None,
                container_name: match (&info.media_query, &info.layer) {
                    (Some(mq), Some(l)) => Some(format!("{mq} • @layer {l}")),
                    (Some(mq), None)    => Some(mq.clone()),
                    (None,     Some(l)) => Some(format!("@layer {l}")),
                    (None,     None)    => None,
                },
            });
        }
    }

    symbols.sort_by_key(|s| s.location.range.start.line);
    Response::new_ok(
        id,
        serde_json::to_value(DocumentSymbolResponse::Flat(symbols)).unwrap_or(json!([])),
    )
}

pub fn workspace_symbol(id: RequestId, params: WorkspaceSymbolParams, state: &ServerState) -> Response {
    let query = params.query.to_lowercase();
    let mut symbols: Vec<SymbolInformation> = Vec::new();

    for (name, infos) in &state.css.classes {
        let display = display_key(name);
        if !query.is_empty() && !display.to_lowercase().contains(&query) {
            continue;
        }
        for info in infos {
            let uri = match Url::from_file_path(&info.source_path) {
                Ok(u) => u,
                Err(_) => continue,
            };
            #[allow(deprecated)]
            symbols.push(SymbolInformation {
                name: display.clone(),
                kind: SymbolKind::CLASS,
                location: Location {
                    uri,
                    range: line_start(info.definition_line),
                },
                tags: None,
                deprecated: None,
                container_name: Some(info.source_file.clone()),
            });
        }
    }

    symbols.sort_by(|a, b| {
        a.name.cmp(&b.name)
            .then(a.container_name.cmp(&b.container_name))
            .then(a.location.range.start.line.cmp(&b.location.range.start.line))
    });
    Response::new_ok(id, serde_json::to_value(symbols).unwrap_or(json!([])))
}

pub fn code_lens(id: RequestId, params: CodeLensParams, state: &ServerState) -> Response {
    let source_path = match css_source_path(&params.text_document.uri) {
        Some(p) => p,
        None => return Response::new_ok(id, json!([])),
    };

    let mut lenses: Vec<CodeLens> = Vec::new();
    for (name, infos) in &state.css.classes {
        for info in infos {
            if info.source_path != source_path {
                continue;
            }
            let title = match state.index.usage_count(name) {
                0 => "unused".to_string(),
                1 => "used 1 time".to_string(),
                n => format!("used {n} times"),
            };
            lenses.push(CodeLens {
                range: line_start(info.definition_line),
                command: Some(Command {
                    title,
                    command: String::new(),
                    arguments: None,
                }),
                data: None,
            });
        }
    }

    lenses.sort_by_key(|l| l.range.start.line);
    Response::new_ok(id, serde_json::to_value(lenses).unwrap_or(json!([])))
}
