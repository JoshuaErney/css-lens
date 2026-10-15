// ---------------------------------------------------------------------------
// LSP request handlers
// ---------------------------------------------------------------------------

pub mod code_action;
pub mod completion;
pub mod hover;
pub mod navigation;
pub mod rename;
pub mod symbols;

use lsp_types::Position;

use crate::cursor::{in_class_attribute, in_id_attribute, word_at};

/// The class_map key under the cursor in an HTML `class=""` attribute (bare
/// name) or `id=""` attribute (`#`-prefixed name).
pub fn attr_key_at(text: &str, pos: Position) -> Option<String> {
    if in_class_attribute(text, pos) {
        word_at(text, pos)
    } else if in_id_attribute(text, pos) {
        word_at(text, pos).map(|w| format!("#{w}"))
    } else {
        None
    }
}
