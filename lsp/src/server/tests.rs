//! Handler-level tests: real requests and notifications routed through
//! `ServerState` against a temporary workspace on disk.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use lsp_server::{ErrorCode, Notification, Request, RequestId};
use lsp_types::{
    Diagnostic, DiagnosticSeverity, FileChangeType, FileEvent, PublishDiagnosticsParams, Url, WorkspaceEdit,
};
use serde_json::{json, Value};

use super::ServerState;

struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "css-lens-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        TempWorkspace { root }
    }

    fn write(&self, name: &str, content: &str) -> &Self {
        fs::write(self.root.join(name), content).unwrap();
        self
    }

    fn uri(&self, name: &str) -> Url {
        Url::from_file_path(self.root.join(name)).unwrap()
    }

    fn server(&self) -> ServerState {
        ServerState::new(Some(self.root.clone()))
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

// ── Protocol helpers ─────────────────────────────────────────────────────────

fn try_request(state: &ServerState, method: &str, params: Value) -> lsp_server::Response {
    state.handle_request(Request { id: RequestId::from(1), method: method.to_string(), params })
}

fn request(state: &ServerState, method: &str, params: Value) -> Value {
    let resp = try_request(state, method, params);
    assert!(resp.error.is_none(), "{method} failed: {:?}", resp.error);
    resp.result.unwrap_or(Value::Null)
}

fn at(uri: &Url, line: u32, character: u32) -> Value {
    json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } })
}

fn open(state: &mut ServerState, uri: &Url, text: &str) -> Vec<Notification> {
    state.handle_notification(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "html", "version": 1, "text": text } }),
    )
}

fn change(state: &mut ServerState, uri: &Url, text: &str) -> Vec<Notification> {
    state.handle_notification(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": uri, "version": 2 }, "contentChanges": [{ "text": text }] }),
    )
}

fn close(state: &mut ServerState, uri: &Url) -> Vec<Notification> {
    state.handle_notification("textDocument/didClose", json!({ "textDocument": { "uri": uri } }))
}

fn watched(state: &mut ServerState, uri: &Url, typ: FileChangeType) -> Vec<Notification> {
    let event = FileEvent { uri: uri.clone(), typ };
    state.handle_notification("workspace/didChangeWatchedFiles", json!({ "changes": [event] }))
}

/// The last diagnostics published for `uri` in `out`.
fn published(out: &[Notification], uri: &Url) -> Option<Vec<Diagnostic>> {
    out.iter()
        .rev()
        .filter(|n| n.method == "textDocument/publishDiagnostics")
        .filter_map(|n| serde_json::from_value::<PublishDiagnosticsParams>(n.params.clone()).ok())
        .find(|p| &p.uri == uri)
        .map(|p| p.diagnostics)
}

fn messages(diags: &[Diagnostic]) -> Vec<&str> {
    let mut msgs: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
    msgs.sort();
    msgs
}

fn labels(result: &Value) -> Vec<String> {
    result.as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_string()).collect()
}

fn lens_titles(state: &ServerState, uri: &Url) -> Vec<String> {
    let result = request(state, "textDocument/codeLens", json!({ "textDocument": { "uri": uri } }));
    result.as_array().unwrap().iter().map(|l| l["command"]["title"].as_str().unwrap().to_string()).collect()
}

fn edits_for(edit: &WorkspaceEdit, uri: &Url) -> Vec<(u32, u32, u32, String)> {
    let mut edits: Vec<_> = edit.changes.as_ref().unwrap().get(uri).map_or(vec![], |edits| {
        edits
            .iter()
            .map(|e| (e.range.start.line, e.range.start.character, e.range.end.character, e.new_text.clone()))
            .collect()
    });
    edits.sort();
    edits
}

// ── Diagnostics ──────────────────────────────────────────────────────────────

#[test]
fn diagnostics_are_scoped_to_linked_stylesheets() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".a { color: red; }").write("b.css", ".b { color: blue; }");
    let mut state = ws.server();
    let page = ws.uri("page.html");

    let out = open(&mut state, &page, "<link rel=\"stylesheet\" href=\"a.css\">\n<div class=\"a b\"></div>");
    let diags = published(&out, &page).unwrap();
    assert_eq!(messages(&diags), ["Unknown CSS class 'b'"]);
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(diags[0].range.start.line, 1);
}

#[test]
fn pages_without_links_see_every_stylesheet() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".a { color: red; }").write("b.css", ".b { color: blue; }");
    let mut state = ws.server();
    let page = ws.uri("page.html");

    let out = open(&mut state, &page, r#"<div class="a b" id="nope"></div>"#);
    assert_eq!(messages(&published(&out, &page).unwrap()), ["Unknown CSS id 'nope'"]);
}

#[test]
fn inline_style_blocks_define_classes() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".a { color: red; }");
    let mut state = ws.server();
    let page = ws.uri("page.html");

    let out = open(
        &mut state,
        &page,
        "<link rel=\"stylesheet\" href=\"a.css\">\n<style>.local { color: red; }</style>\n<div class=\"a local local\">",
    );
    assert_eq!(
        messages(&published(&out, &page).unwrap()),
        ["class 'local' is already listed in this attribute"]
    );
}

#[test]
fn css_save_publishes_duplicates_and_unused_hints_together() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".btn { color: red; }\n.btn { color: blue; }\n.unused { color: green; }");
    let mut state = ws.server();
    let css = ws.uri("a.css");
    open(&mut state, &ws.uri("page.html"), r#"<div class="btn">"#);

    let out = watched(&mut state, &css, FileChangeType::CHANGED);
    let diags = published(&out, &css).unwrap();
    assert_eq!(
        messages(&diags),
        ["'.btn' is already defined earlier in this file", "'.unused' is not used in any open HTML file"]
    );
}

#[test]
fn saving_a_css_file_does_not_duplicate_imported_selectors() {
    let ws = TempWorkspace::new();
    ws.write("main.css", "@import \"base.css\";\n.main { color: red; }")
        .write("base.css", ".base { color: blue; }");
    let mut state = ws.server();
    assert_eq!(state.class_map["base"].len(), 1);

    let out = watched(&mut state, &ws.uri("main.css"), FileChangeType::CHANGED);
    assert_eq!(state.class_map["base"].len(), 1);
    assert_eq!(state.class_map["main"].len(), 1);
    assert_eq!(published(&out, &ws.uri("base.css")), Some(vec![]));
}

#[test]
fn deleting_a_css_file_clears_its_selectors_and_diagnostics() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".btn { color: red; }\n.btn { color: blue; }");
    let mut state = ws.server();
    let css = ws.uri("a.css");
    let page = ws.uri("page.html");
    open(&mut state, &page, r#"<div class="btn">"#);

    fs::remove_file(ws.root.join("a.css")).unwrap();
    let out = watched(&mut state, &css, FileChangeType::DELETED);
    assert!(!state.class_map.contains_key("btn"));
    assert_eq!(published(&out, &css), Some(vec![]));
    assert_eq!(messages(&published(&out, &page).unwrap()), ["Unknown CSS class 'btn'"]);
}

#[test]
fn js_references_suppress_unused_hints_and_track_changes() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".btn { color: red; }\n.js-open { color: blue; }")
        .write("app.js", "menu.classList.add(\"js-open\");");
    let mut state = ws.server();
    let css = ws.uri("a.css");
    open(&mut state, &ws.uri("page.html"), r#"<div class="btn">"#);

    let out = watched(&mut state, &css, FileChangeType::CHANGED);
    assert_eq!(published(&out, &css), Some(vec![]));

    ws.write("app.js", "// no class references left");
    let out = watched(&mut state, &ws.uri("app.js"), FileChangeType::CHANGED);
    assert_eq!(
        messages(&published(&out, &css).unwrap()),
        ["'.js-open' is not used in any open HTML file"]
    );
}

// ── Completion, hover, definition ────────────────────────────────────────────

#[test]
fn completion_offers_linked_classes_not_already_used() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".alpha { color: red; }\n.beta { color: red; }")
        .write("b.css", ".gamma { color: red; }");
    let mut state = ws.server();
    let page = ws.uri("page.html");
    open(&mut state, &page, "<link rel=\"stylesheet\" href=\"a.css\">\n<div class=\"alpha \"></div>");

    let result = request(&state, "textDocument/completion", at(&page, 1, 18));
    assert_eq!(labels(&result), ["beta"]);
}

#[test]
fn completion_in_style_attribute_offers_values_and_variables() {
    let ws = TempWorkspace::new();
    ws.write("theme.css", ".theme { --brand: #f00; }");
    let mut state = ws.server();
    let page = ws.uri("page.html");
    open(&mut state, &page, "<div style=\"display: fl\">\n<p style=\"color: var(--b\">");

    let values = labels(&request(&state, "textDocument/completion", at(&page, 0, 23)));
    assert_eq!(values, ["flex", "flow-root"]);
    let vars = labels(&request(&state, "textDocument/completion", at(&page, 1, 24)));
    assert_eq!(vars, ["--brand"]);
}

#[test]
fn hover_shows_definition_details() {
    let ws = TempWorkspace::new();
    ws.write("a.css", "\n.btn { color: #fff; }");
    let mut state = ws.server();
    let page = ws.uri("page.html");
    open(&mut state, &page, r#"<div class="btn">"#);

    let result = request(&state, "textDocument/hover", at(&page, 0, 13));
    let value = result["contents"]["value"].as_str().unwrap();
    assert!(value.contains("**btn** — a.css:2"), "{value}");
    assert!(value.contains("Specificity: `(0,1,0)`"), "{value}");
    assert!(value.contains("Colors: `#fff`"), "{value}");
}

#[test]
fn definition_jumps_to_selector() {
    let ws = TempWorkspace::new();
    ws.write("a.css", "\n\n#hero { color: red; }");
    let mut state = ws.server();
    let page = ws.uri("page.html");
    open(&mut state, &page, r#"<div id="hero">"#);

    let result = request(&state, "textDocument/definition", at(&page, 0, 11));
    assert_eq!(result["uri"], json!(ws.uri("a.css")));
    assert_eq!(result["range"]["start"]["line"], 2);
}

// ── References and rename ────────────────────────────────────────────────────

#[test]
fn references_prefer_open_buffers_over_disk() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".btn { color: red; }")
        .write("other.html", r#"<p class="btn">"#)
        .write("page.html", "<div></div>");
    let mut state = ws.server();
    let page = ws.uri("page.html");
    open(&mut state, &page, "<div class=\"btn card\">\n<span class=\"btn\">");

    let mut params = at(&page, 0, 13);
    params["context"] = json!({ "includeDeclaration": true });
    let result = request(&state, "textDocument/references", params);
    let mut uris: Vec<String> = result.as_array().unwrap().iter().map(|l| l["uri"].as_str().unwrap().to_string()).collect();
    uris.sort();
    let mut expected = vec![ws.uri("other.html").to_string(), page.to_string(), page.to_string()];
    expected.sort();
    assert_eq!(uris, expected);
}

#[test]
fn rename_from_html_updates_css_and_every_html_file() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".btn { color: red; }\n.btn-primary { color: blue; }")
        .write("other.html", r#"<p class="btn">"#)
        .write("page.html", r#"<div class="btn btn-primary">"#);
    let mut state = ws.server();
    let page = ws.uri("page.html");
    open(&mut state, &page, r#"<div class="btn btn-primary">"#);

    let mut params = at(&page, 0, 13);
    params["newName"] = json!("button");
    let edit: WorkspaceEdit = serde_json::from_value(request(&state, "textDocument/rename", params)).unwrap();

    assert_eq!(edits_for(&edit, &ws.uri("a.css")), [(0, 0, 4, ".button".to_string())]);
    assert_eq!(edits_for(&edit, &page), [(0, 12, 15, "button".to_string())]);
    assert_eq!(edits_for(&edit, &ws.uri("other.html")), [(0, 10, 13, "button".to_string())]);
}

#[test]
fn rename_from_css_selector_definition() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".card { color: red; }").write("page.html", r#"<div class="card">"#);
    let mut state = ws.server();
    let css = ws.uri("a.css");
    open(&mut state, &css, ".card { color: red; }");

    let mut params = at(&css, 0, 2);
    params["newName"] = json!("panel");
    let edit: WorkspaceEdit = serde_json::from_value(request(&state, "textDocument/rename", params)).unwrap();

    assert_eq!(edits_for(&edit, &css), [(0, 0, 5, ".panel".to_string())]);
    assert_eq!(edits_for(&edit, &ws.uri("page.html")), [(0, 12, 16, "panel".to_string())]);
}

#[test]
fn rename_rejects_invalid_identifiers() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".btn { color: red; }");
    let mut state = ws.server();
    let page = ws.uri("page.html");
    open(&mut state, &page, r#"<div class="btn">"#);

    let mut params = at(&page, 0, 13);
    params["newName"] = json!("x{}");
    let resp = try_request(&state, "textDocument/rename", params);
    assert_eq!(resp.error.unwrap().code, ErrorCode::InvalidParams as i32);
}

// ── Code actions ─────────────────────────────────────────────────────────────

#[test]
fn create_rule_quick_fix_targets_linked_stylesheet() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".a { color: red; }").write("b.css", ".b { color: blue; }\n");
    let mut state = ws.server();
    let page = ws.uri("page.html");
    let out = open(&mut state, &page, "<link rel=\"stylesheet\" href=\"b.css\">\n<div class=\"missing\">");
    let diags = published(&out, &page).unwrap();

    let result = request(
        &state,
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": page },
            "range": diags[0].range,
            "context": { "diagnostics": diags },
        }),
    );
    let actions = result.as_array().unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["title"], "Create CSS class .missing in b.css");
    let edit: WorkspaceEdit = serde_json::from_value(actions[0]["edit"].clone()).unwrap();
    assert_eq!(edits_for(&edit, &ws.uri("b.css")), [(1, 0, 0, ".missing {\n  \n}\n".to_string())]);
}

#[test]
fn remove_unused_rule_skips_blocks_with_used_co_selectors() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".gone { color: red; }\n.kept, .dead { color: blue; }\n");
    let mut state = ws.server();
    let css = ws.uri("a.css");
    open(&mut state, &ws.uri("page.html"), r#"<div class="kept">"#);
    let hints = published(&watched(&mut state, &css, FileChangeType::CHANGED), &css).unwrap();
    assert_eq!(hints.len(), 2);

    let result = request(
        &state,
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": css },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 1, "character": 0 } },
            "context": { "diagnostics": hints },
        }),
    );
    let titles: Vec<&str> = result.as_array().unwrap().iter().map(|a| a["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Remove unused rule '.gone' from a.css"]);
}

// ── Code lens and the usage index ────────────────────────────────────────────

#[test]
fn code_lens_counts_follow_buffers_and_disk() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".btn { color: red; }")
        .write("page.html", r#"<div class="btn">"#)
        .write("other.html", r#"<p class="btn">"#);
    let mut state = ws.server();
    let css = ws.uri("a.css");
    let page = ws.uri("page.html");
    let other = ws.uri("other.html");
    assert_eq!(lens_titles(&state, &css), ["used 2 times"]);

    // Unsaved edits in an open buffer count instead of the file on disk.
    open(&mut state, &page, r#"<div class="btn">"#);
    change(&mut state, &page, r#"<div class="card">"#);
    assert_eq!(lens_titles(&state, &css), ["used 1 time"]);

    // Closing the buffer falls back to the on-disk contents.
    close(&mut state, &page);
    assert_eq!(lens_titles(&state, &css), ["used 2 times"]);

    // Closed files are refreshed from file-watcher events.
    ws.write("other.html", "<p></p>");
    watched(&mut state, &other, FileChangeType::CHANGED);
    assert_eq!(lens_titles(&state, &css), ["used 1 time"]);

    fs::remove_file(ws.root.join("page.html")).unwrap();
    watched(&mut state, &page, FileChangeType::DELETED);
    assert_eq!(lens_titles(&state, &css), ["unused"]);
}

#[test]
fn watcher_events_for_open_html_files_keep_the_buffer() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".btn { color: red; }").write("page.html", "<p></p>");
    let mut state = ws.server();
    let page = ws.uri("page.html");
    open(&mut state, &page, r#"<div class="btn">"#);

    watched(&mut state, &page, FileChangeType::CHANGED);
    assert_eq!(lens_titles(&state, &ws.uri("a.css")), ["used 1 time"]);
}

// ── Symbols ──────────────────────────────────────────────────────────────────

#[test]
fn workspace_symbol_filters_by_query() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".btn { color: red; }\n#hero { color: red; }\n.card { color: red; }");
    let state = ws.server();

    let names = |query: &str| -> Vec<String> {
        let result = request(&state, "workspace/symbol", json!({ "query": query }));
        result.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(names("bt"), [".btn"]);
    assert_eq!(names(""), ["#hero", ".btn", ".card"]);
}

#[test]
fn document_symbols_list_selectors_in_order() {
    let ws = TempWorkspace::new();
    ws.write("a.css", ".b { color: red; }\n@media (min-width: 1px) {\n  .a { color: red; }\n}");
    let state = ws.server();

    let result = request(&state, "textDocument/documentSymbol", json!({ "textDocument": { "uri": ws.uri("a.css") } }));
    let symbols = result.as_array().unwrap();
    assert_eq!(symbols.len(), 2);
    assert_eq!(symbols[0]["name"], ".b");
    assert_eq!(symbols[1]["name"], ".a");
    assert_eq!(symbols[1]["containerName"], "@media (min-width: 1px)");
}

#[test]
fn unknown_methods_return_method_not_found() {
    let state = ServerState::new(None);
    let resp = try_request(&state, "textDocument/formatting", json!({}));
    assert_eq!(resp.error.unwrap().code, ErrorCode::MethodNotFound as i32);
}
