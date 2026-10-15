mod css;
mod cursor;
mod diagnostics;
mod handlers;
mod html;
mod js;
mod model;
mod regexes;
mod server;
mod workspace;

use std::path::PathBuf;

use lsp_server::{Connection, Message, RequestId};
use lsp_types::*;
use serde_json::json;

use crate::server::ServerState;

/// Files the client watches for us. CSS changes re-index selectors; HTML and JS
/// changes keep the usage index current for files that aren't open.
const WATCHED_GLOBS: &[&str] = &["**/*.css", "**/*.html", "**/*.htm", "**/*.js"];

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
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        code_lens_provider: Some(CodeLensOptions { resolve_provider: Some(false) }),
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

    let mut state = ServerState::new(root_path);

    let init_result = InitializeResult {
        capabilities,
        server_info: Some(ServerInfo {
            name: "css-lens-lsp".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    };
    connection.initialize_finish(init_id, serde_json::to_value(init_result)?)?;

    // Ask the client to send workspace/didChangeWatchedFiles for the files we index.
    let register_params = RegistrationParams {
        registrations: vec![Registration {
            id: "css-lens-file-watcher".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: Some(
                serde_json::to_value(DidChangeWatchedFilesRegistrationOptions {
                    watchers: WATCHED_GLOBS
                        .iter()
                        .map(|glob| FileSystemWatcher {
                            glob_pattern: GlobPattern::String((*glob).to_string()),
                            kind: Some(WatchKind::Create | WatchKind::Change | WatchKind::Delete),
                        })
                        .collect(),
                })
                .unwrap_or(json!({})),
            ),
        }],
    };
    connection.sender.send(Message::Request(lsp_server::Request {
        id: RequestId::from(0i32),
        method: "client/registerCapability".to_string(),
        params: serde_json::to_value(register_params).unwrap_or(json!({})),
    }))?;

    while let Ok(msg) = connection.receiver.recv() {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    break;
                }
                connection.sender.send(Message::Response(state.handle_request(req)))?;
            }
            Message::Notification(notif) => {
                for n in state.handle_notification(&notif.method, notif.params) {
                    connection.sender.send(Message::Notification(n))?;
                }
            }
            Message::Response(_) => {}
        }
    }

    // The writer thread only exits once every sender is dropped, so joining
    // while `connection` is still alive would hang forever after `exit`.
    drop(connection);
    io_threads.join()?;
    Ok(())
}
