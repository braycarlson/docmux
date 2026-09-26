mod parts;
mod read;
mod write;

use crate::document::Span;

pub const DOCX_ABSTRACT_COUNT_MAX: u32 = 1024;
pub const DOCX_LEVEL_COUNT_MAX: u32 = 9;
pub const DOCX_NUMBERING_COUNT_MAX: u32 = 4096;
pub const DOCX_RELATIONSHIP_COUNT_MAX: u32 = 4096;
pub const DOCX_STYLE_COUNT_MAX: u32 = 2048;

pub use read::read;
pub use write::write;

#[derive(Clone, Copy, Debug, Default)]
pub struct DocxLevel {
    pub ordered: bool,
    pub start: u32,
}

impl DocxLevel {
    pub const EMPTY: Self = Self { ordered: false, start: 0 };
}

#[derive(Clone, Copy, Debug)]
pub struct DocxAbstract {
    pub identifier: u32,
    pub levels: [DocxLevel; DOCX_LEVEL_COUNT_MAX as usize],
}

impl DocxAbstract {
    pub const EMPTY: Self =
        Self { identifier: 0, levels: [DocxLevel::EMPTY; DOCX_LEVEL_COUNT_MAX as usize] };
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DocxNumbering {
    pub abstract_identifier: u32,
    pub identifier: u32,
    pub start_overrides: [u32; DOCX_LEVEL_COUNT_MAX as usize],
}

impl DocxNumbering {
    pub const EMPTY: Self = Self {
        abstract_identifier: 0,
        identifier: 0,
        start_overrides: [0; DOCX_LEVEL_COUNT_MAX as usize],
    };
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DocxRelationship {
    pub identifier: Span,
    pub target: Span,
}

impl DocxRelationship {
    pub const EMPTY: Self = Self { identifier: Span::EMPTY, target: Span::EMPTY };
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DocxStyle {
    pub based_on: Span,
    pub identifier: Span,
    pub name: Span,
}

impl DocxStyle {
    pub const EMPTY: Self =
        Self { based_on: Span::EMPTY, identifier: Span::EMPTY, name: Span::EMPTY };
}
