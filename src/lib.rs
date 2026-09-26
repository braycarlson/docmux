#![no_std]
#![forbid(unsafe_code)]

pub mod adf;
pub mod document;
pub mod docx;
pub mod error;
pub mod html;
pub mod markdown;
pub mod workspace;

mod bytes;
mod entities;
mod inflate;
mod json;
mod xml;
mod zip;

pub use document::{
    Alignment,
    Children,
    DEPTH_MAX,
    Document,
    HEADING_LEVEL_MAX,
    Marks,
    NODE_COUNT_MAX,
    NODE_NONE,
    NODE_ROOT,
    Node,
    NodeKind,
    Span,
    TABLE_COLSPAN_MAX,
    TEXT_BYTES_MAX,
    TaskState,
    TextRun,
    Walk,
    WalkEvent,
};
pub use error::{Error, Result};
pub use workspace::Workspace;

#[cfg(test)]
pub(crate) mod test_support {
    extern crate std;

    use crate::document::Document;
    use crate::workspace::Workspace;
    use std::sync::{Mutex, PoisonError};

    static DOCUMENT: Mutex<Document> = Mutex::new(Document::EMPTY);
    static WORKSPACE: Mutex<Workspace> = Mutex::new(Workspace::EMPTY);

    pub(crate) fn with_document<R, F: FnOnce(&mut Document) -> R>(body: F) -> R {
        let mut guard = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);

        guard.reset();

        assert!(guard.node_count() == 1);
        assert!(guard.text_length() == 1);

        body(&mut guard)
    }

    pub(crate) fn with_workspace<R, F: FnOnce(&mut Workspace, &mut Document) -> R>(body: F) -> R {
        let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);
        let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);

        workspace.reset();
        document.reset();

        assert!(document.node_count() == 1);
        assert!(workspace.part_length == 0);

        body(&mut workspace, &mut document)
    }
}
