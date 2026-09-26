use crate::document::{NODE_COUNT_MAX, Span};
use crate::docx::{
    DOCX_ABSTRACT_COUNT_MAX,
    DOCX_NUMBERING_COUNT_MAX,
    DOCX_RELATIONSHIP_COUNT_MAX,
    DOCX_STYLE_COUNT_MAX,
    DocxAbstract,
    DocxNumbering,
    DocxRelationship,
    DocxStyle,
};
use crate::markdown::inline::{INLINE_TOKEN_COUNT_MAX, InlineToken};
use core::fmt;

pub const DEFINITION_COUNT_MAX: u32 = 1024;
pub const PENDING_INLINE_COUNT_MAX: u32 = NODE_COUNT_MAX;
pub const LINK_COUNT_MAX: u32 = 4096;
pub const NUMBERING_BYTES_MAX: u32 = 1 << 20;
pub const PART_BYTES_MAX: u32 = 8 << 20;
pub const RELATIONSHIPS_BYTES_MAX: u32 = 512 << 10;
pub const STYLES_BYTES_MAX: u32 = 2 << 20;

#[derive(Clone, Copy, Debug, Default)]
pub struct LinkDefinition {
    pub label: Span,
    pub url: Span,
}

impl LinkDefinition {
    pub const EMPTY: Self = Self { label: Span::EMPTY, url: Span::EMPTY };
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PendingInline {
    pub node: u32,
    pub text: Span,
}

impl PendingInline {
    pub const EMPTY: Self = Self { node: 0, text: Span::EMPTY };
}

pub struct Workspace {
    pub(crate) definition_count: u32,
    pub(crate) definitions: [LinkDefinition; DEFINITION_COUNT_MAX as usize],
    pub(crate) docx_abstract_count: u32,
    pub(crate) docx_abstracts: [DocxAbstract; DOCX_ABSTRACT_COUNT_MAX as usize],
    pub(crate) docx_numbering_count: u32,
    pub(crate) docx_numberings: [DocxNumbering; DOCX_NUMBERING_COUNT_MAX as usize],
    pub(crate) docx_relationship_count: u32,
    pub(crate) docx_relationships: [DocxRelationship; DOCX_RELATIONSHIP_COUNT_MAX as usize],
    pub(crate) docx_style_count: u32,
    pub(crate) docx_styles: [DocxStyle; DOCX_STYLE_COUNT_MAX as usize],
    pub(crate) inline_tokens: [InlineToken; INLINE_TOKEN_COUNT_MAX as usize],
    pub(crate) link_count: u32,
    pub(crate) links: [Span; LINK_COUNT_MAX as usize],
    pub(crate) numbering: [u8; NUMBERING_BYTES_MAX as usize],
    pub(crate) part: [u8; PART_BYTES_MAX as usize],
    pub(crate) part_length: u32,
    pub(crate) pending_inline_count: u32,
    pub(crate) pending_inlines: [PendingInline; PENDING_INLINE_COUNT_MAX as usize],
    pub(crate) relationships: [u8; RELATIONSHIPS_BYTES_MAX as usize],
    pub(crate) styles: [u8; STYLES_BYTES_MAX as usize],
}

impl Workspace {
    pub const EMPTY: Self = Self {
        definition_count: 0,
        definitions: [LinkDefinition::EMPTY; DEFINITION_COUNT_MAX as usize],
        docx_abstract_count: 0,
        docx_abstracts: [DocxAbstract::EMPTY; DOCX_ABSTRACT_COUNT_MAX as usize],
        docx_numbering_count: 0,
        docx_numberings: [DocxNumbering::EMPTY; DOCX_NUMBERING_COUNT_MAX as usize],
        docx_relationship_count: 0,
        docx_relationships: [DocxRelationship::EMPTY; DOCX_RELATIONSHIP_COUNT_MAX as usize],
        docx_style_count: 0,
        docx_styles: [DocxStyle::EMPTY; DOCX_STYLE_COUNT_MAX as usize],
        inline_tokens: [InlineToken::EMPTY; INLINE_TOKEN_COUNT_MAX as usize],
        link_count: 0,
        links: [Span::EMPTY; LINK_COUNT_MAX as usize],
        numbering: [0; NUMBERING_BYTES_MAX as usize],
        part: [0; PART_BYTES_MAX as usize],
        part_length: 0,
        pending_inline_count: 0,
        pending_inlines: [PendingInline::EMPTY; PENDING_INLINE_COUNT_MAX as usize],
        relationships: [0; RELATIONSHIPS_BYTES_MAX as usize],
        styles: [0; STYLES_BYTES_MAX as usize],
    };

    pub fn reset(&mut self) {
        self.definition_count = 0;
        self.docx_abstract_count = 0;
        self.docx_numbering_count = 0;
        self.docx_relationship_count = 0;
        self.docx_style_count = 0;
        self.link_count = 0;
        self.part_length = 0;
        self.pending_inline_count = 0;

        assert!(self.definition_count == 0);
        assert!(self.pending_inline_count == 0);
    }
}

impl fmt::Debug for Workspace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Workspace")
            .field("definition_count", &self.definition_count)
            .field("link_count", &self.link_count)
            .field("part_length", &self.part_length)
            .finish_non_exhaustive()
    }
}
