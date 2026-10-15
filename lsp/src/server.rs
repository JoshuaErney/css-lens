// ---------------------------------------------------------------------------
// Server state and request / notification routing
// ---------------------------------------------------------------------------

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_server::{ErrorCode, Notification, Request, RequestId, Response};
use lsp_types::{
    CodeActionParams, CodeLensParams, CompletionParams, Diagnostic, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentSymbolParams,
    FileChangeType, GotoDefinitionParams, HoverParams, ReferenceParams, RenameParams, Url, WorkspaceSymbolParams,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::css::{is_css_path, reload_css_file, reload_keyframes_file, scan_directory, scan_keyframes};
use crate::diagnostics::{all_html_diagnostics, diagnostics_for_css_duplicates, diagnostics_for_unused, publish_diagnostics};
use crate::handlers::{code_action, completion, hover, navigation, rename, symbols};
use crate::html::{build_document_index, build_scoped_index, is_html_uri, reachable_css_paths};
use crate::model::{CssIndex, DocumentMap, KeyframesMap};
use crate::workspace::{has_extension, is_workspace_file, WorkspaceIndex, HTML_EXTENSIONS, JS_EXTENSIONS};

#[cfg(test)]
mod tests;

pub struct ServerState {
    pub root: Option<PathBuf>,
    /// Selectors and custom properties from every workspace stylesheet.
    pub css: CssIndex,
    pub keyframes_map: KeyframesMap,
    pub documents: DocumentMap,
    pub index: WorkspaceIndex,
}

/// Selectors and custom properties visible from the requesting document. For
/// an HTML file these are scoped to its linked stylesheets plus inline
/// `<style>` blocks; otherwise they are the global index.
pub type DocMaps<'a> = Cow<'a, CssIndex>;

fn with_params<P: DeserializeOwned>(
    id: RequestId,
    params: Value,
    handler: impl FnOnce(RequestId, P) -> Response,
) -> Response {
    match serde_json::from_value::<P>(params) {
        Ok(p) => handler(id, p),
        Err(e) => Response::new_err(id, ErrorCode::InvalidParams as i32, e.to_string()),
    }
}

impl ServerState {
    pub fn new(root: Option<PathBuf>) -> Self {
        let mut css = CssIndex::default();
        let mut keyframes_map = KeyframesMap::new();
        let mut index = WorkspaceIndex::default();
        if let Some(ref root) = root {
            scan_directory(root, &mut css);
            keyframes_map = scan_keyframes(root);
            index = WorkspaceIndex::build(root);
        }
        ServerState { root, css, keyframes_map, documents: DocumentMap::new(), index }
    }

    pub fn handle_request(&self, req: Request) -> Response {
        let Request { id, method, params } = req;
        match method.as_str() {
            "textDocument/completion" => with_params(id, params, |id, p: CompletionParams| {
                let maps = self.doc_maps(&p.text_document_position.text_document.uri);
                completion::completion(id, p, self, &maps)
            }),
            "textDocument/hover" => with_params(id, params, |id, p: HoverParams| {
                let maps = self.doc_maps(&p.text_document_position_params.text_document.uri);
                hover::hover(id, p, self, &maps)
            }),
            "textDocument/definition" => with_params(id, params, |id, p: GotoDefinitionParams| {
                let maps = self.doc_maps(&p.text_document_position_params.text_document.uri);
                navigation::definition(id, p, self, &maps)
            }),
            "textDocument/references" => with_params(id, params, |id, p: ReferenceParams| {
                navigation::references(id, p, self)
            }),
            "textDocument/rename" => with_params(id, params, |id, p: RenameParams| rename::rename(id, p, self)),
            "textDocument/codeAction" => with_params(id, params, |id, p: CodeActionParams| {
                code_action::code_action(id, p, self)
            }),
            "textDocument/documentSymbol" => with_params(id, params, |id, p: DocumentSymbolParams| {
                symbols::document_symbol(id, p, self)
            }),
            "textDocument/codeLens" => with_params(id, params, |id, p: CodeLensParams| {
                symbols::code_lens(id, p, self)
            }),
            "workspace/symbol" => with_params(id, params, |id, p: WorkspaceSymbolParams| {
                symbols::workspace_symbol(id, p, self)
            }),
            _ => Response::new_err(id, ErrorCode::MethodNotFound as i32, format!("method not found: {method}")),
        }
    }

    pub fn handle_notification(&mut self, method: &str, params: Value) -> Vec<Notification> {
        let mut out: Vec<Notification> = Vec::new();

        match method {
            "textDocument/didOpen" => {
                if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(params) {
                    self.document_changed(p.text_document.uri, p.text_document.text, &mut out);
                }
            }
            "textDocument/didChange" => {
                if let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(params) {
                    if let Some(last) = p.content_changes.into_iter().last() {
                        self.document_changed(p.text_document.uri, last.text, &mut out);
                    }
                }
            }
            "textDocument/didClose" => {
                if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(params) {
                    let uri = p.text_document.uri;
                    self.documents.remove(&uri);
                    // Without a buffer, the file's usage falls back to its contents on disk.
                    if let Some(path) = self.indexed_html_path(&uri) {
                        self.index.reload_html(&path);
                    }
                    out.push(publish_diagnostics(uri, vec![]));
                }
            }
            "workspace/didChangeWatchedFiles" => {
                if let Ok(p) = serde_json::from_value::<DidChangeWatchedFilesParams>(params) {
                    self.files_changed(p, &mut out);
                }
            }
            _ => {}
        }

        out
    }

    fn doc_maps(&self, uri: &Url) -> DocMaps<'_> {
        let scoped = if is_html_uri(uri) {
            uri.to_file_path().ok().and_then(|path| {
                let text = self.documents.get(uri)?;
                build_document_index(&self.css, &path, text)
            })
        } else {
            None
        };
        scoped.map_or(Cow::Borrowed(&self.css), Cow::Owned)
    }

    /// The path of an HTML document that belongs in the usage index.
    fn indexed_html_path(&self, uri: &Url) -> Option<PathBuf> {
        let root = self.root.as_deref()?;
        if !is_html_uri(uri) {
            return None;
        }
        let path = uri.to_file_path().ok()?;
        is_workspace_file(root, &path).then_some(path)
    }

    fn document_changed(&mut self, uri: Url, text: String, out: &mut Vec<Notification>) {
        if is_html_uri(&uri) {
            out.push(self.html_diagnostics(&uri, &text));
            if let Some(path) = self.indexed_html_path(&uri) {
                self.index.set_html(path, &text);
            }
        }
        self.documents.insert(uri, text);
    }

    fn html_diagnostics(&self, uri: &Url, text: &str) -> Notification {
        let diag_map = uri.to_file_path().ok().and_then(|path| {
            let reachable = reachable_css_paths(&path, text);
            build_scoped_index(&self.css, &reachable)
        });
        let effective = diag_map.as_ref().map_or(&self.css.classes, |scoped| &scoped.classes);
        publish_diagnostics(uri.clone(), all_html_diagnostics(text, uri, effective))
    }

    fn files_changed(&mut self, params: DidChangeWatchedFilesParams, out: &mut Vec<Notification>) {
        let mut css_changes: Vec<(PathBuf, FileChangeType)> = Vec::new();
        let mut js_changed = false;

        for change in params.changes {
            let Ok(path) = change.uri.to_file_path() else { continue };
            if is_css_path(&path) {
                css_changes.push((path, change.typ));
                continue;
            }
            if !self.root.as_deref().is_some_and(|root| is_workspace_file(root, &path)) {
                continue;
            }
            if has_extension(&path, HTML_EXTENSIONS) {
                // An open buffer is authoritative over the file on disk.
                if !self.documents.contains_key(&change.uri) {
                    self.index.reload_html(&path);
                }
            } else if has_extension(&path, JS_EXTENSIONS) {
                self.index.reload_js(&path);
                js_changed = true;
            }
        }

        if !css_changes.is_empty() {
            for (path, typ) in &css_changes {
                reload_css_file(path, *typ, &mut self.css);
                reload_keyframes_file(path, *typ, &mut self.keyframes_map);
            }

            // Refresh HTML diagnostics since the class map changed.
            for (uri, text) in &self.documents {
                if is_html_uri(uri) {
                    out.push(self.html_diagnostics(uri, text));
                }
            }
        }

        if !css_changes.is_empty() || js_changed {
            let changed: Vec<PathBuf> = css_changes.into_iter().map(|(path, _)| path).collect();
            out.extend(self.css_diagnostics(&changed));
        }
    }

    /// Publishes duplicate-selector warnings and unused-selector hints for every
    /// indexed CSS file, plus each path in `also` (so a deleted file is cleared).
    /// A publish replaces a file's whole diagnostic set, so both kinds must be
    /// sent together or one would overwrite the other.
    fn css_diagnostics(&self, also: &[PathBuf]) -> Vec<Notification> {
        let mut by_file: HashMap<String, Vec<Diagnostic>> = HashMap::new();
        for info in self.css.classes.values().flatten() {
            by_file.entry(info.source_path.clone()).or_default();
        }
        for path in also {
            by_file.entry(path.to_string_lossy().into_owned()).or_default();
        }
        for (source_path, diags) in diagnostics_for_css_duplicates(&self.css.classes) {
            by_file.entry(source_path).or_default().extend(diags);
        }
        for (source_path, hints) in diagnostics_for_unused(&self.css.classes, &self.documents, self.index.js_refs()) {
            by_file.entry(source_path).or_default().extend(hints);
        }

        by_file
            .into_iter()
            .filter_map(|(source_path, diags)| {
                let uri = Url::from_file_path(Path::new(&source_path)).ok()?;
                Some(publish_diagnostics(uri, diags))
            })
            .collect()
    }
}
