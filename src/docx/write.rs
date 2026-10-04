use crate::bytes::{Sink, u32_from_usize};
use crate::document::{
    Alignment,
    DEPTH_MAX,
    Document,
    HEADING_LEVEL_MAX,
    Marks,
    NODE_COUNT_MAX,
    NODE_NONE,
    NODE_ROOT,
    NodeKind,
    Span,
    TABLE_COLSPAN_MAX,
    TaskState,
    TextRun,
    Walk,
    WalkEvent,
};
use crate::docx::DOCX_LEVEL_COUNT_MAX;
use crate::error::{Error, Result};
use crate::workspace::{LINK_COUNT_MAX, Workspace};
use crate::xml::{escape_attribute, escape_text};
use crate::zip::ZipWriter;

const CELL_STACK_MAX: u32 = DEPTH_MAX as u32 + 1;
const INDENT_TWIPS: u32 = 720;
const HYPERLINK_RELATIONSHIP_IDENTIFIER_FIRST: u32 = 3;
const NUMBERING_IDENTIFIER_BULLET: u32 = 1;

const CONTENT_TYPES: &[u8] = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
    "<Default Extension=\"rels\" ",
    "ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>",
    "<Default Extension=\"xml\" ContentType=\"application/xml\"/>",
    "<Override PartName=\"/word/document.xml\" ContentType=\"application/",
    "vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>",
    "<Override PartName=\"/word/styles.xml\" ContentType=\"application/",
    "vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>",
    "<Override PartName=\"/word/numbering.xml\" ContentType=\"application/",
    "vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/></Types>",
)
.as_bytes();

const PACKAGE_RELATIONSHIPS: &[u8] = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/",
    "relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>",
)
.as_bytes();

const DOCUMENT_HEAD: &[u8] = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" ",
    "xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body>",
)
.as_bytes();

const DOCUMENT_TAIL: &[u8] = concat!(
    "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" ",
    "w:bottom=\"1440\" w:left=\"1440\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/>",
    "</w:sectPr></w:body></w:document>",
)
.as_bytes();

const STYLES: &[u8] = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    "<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\" ",
    "w:cs=\"Calibri\" w:eastAsia=\"Calibri\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/>",
    "<w:lang w:val=\"en-US\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr>",
    "<w:spacing w:after=\"160\" w:line=\"259\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault>",
    "</w:docDefaults>",
    "<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\">",
    "<w:name w:val=\"Normal\"/><w:qFormat/></w:style>",
    "<w:style w:type=\"character\" w:default=\"1\" w:styleId=\"DefaultParagraphFont\">",
    "<w:name w:val=\"Default Paragraph Font\"/><w:uiPriority w:val=\"1\"/><w:semiHidden/>",
    "</w:style>",
    "<w:style w:type=\"table\" w:default=\"1\" w:styleId=\"TableNormal\">",
    "<w:name w:val=\"Normal Table\"/><w:semiHidden/><w:tblPr>",
    "<w:tblInd w:w=\"0\" w:type=\"dxa\"/><w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/>",
    "<w:left w:w=\"108\" w:type=\"dxa\"/><w:bottom w:w=\"0\" w:type=\"dxa\"/>",
    "<w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr></w:style>",
    "<w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/>",
    "<w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/>",
    "<w:spacing w:before=\"240\" w:after=\"80\"/><w:outlineLvl w:val=\"0\"/></w:pPr>",
    "<w:rPr><w:b/><w:bCs/><w:sz w:val=\"40\"/><w:szCs w:val=\"40\"/></w:rPr></w:style>",
    "<w:style w:type=\"paragraph\" w:styleId=\"Heading2\"><w:name w:val=\"heading 2\"/>",
    "<w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/>",
    "<w:spacing w:before=\"160\" w:after=\"80\"/><w:outlineLvl w:val=\"1\"/></w:pPr>",
    "<w:rPr><w:b/><w:bCs/><w:sz w:val=\"32\"/><w:szCs w:val=\"32\"/></w:rPr></w:style>",
    "<w:style w:type=\"paragraph\" w:styleId=\"Heading3\"><w:name w:val=\"heading 3\"/>",
    "<w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/>",
    "<w:spacing w:before=\"160\" w:after=\"80\"/><w:outlineLvl w:val=\"2\"/></w:pPr>",
    "<w:rPr><w:b/><w:bCs/><w:sz w:val=\"28\"/><w:szCs w:val=\"28\"/></w:rPr></w:style>",
    "<w:style w:type=\"paragraph\" w:styleId=\"Heading4\"><w:name w:val=\"heading 4\"/>",
    "<w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/>",
    "<w:spacing w:before=\"80\" w:after=\"40\"/><w:outlineLvl w:val=\"3\"/></w:pPr>",
    "<w:rPr><w:b/><w:bCs/><w:i/><w:iCs/><w:sz w:val=\"24\"/><w:szCs w:val=\"24\"/></w:rPr>",
    "</w:style>",
    "<w:style w:type=\"paragraph\" w:styleId=\"Heading5\"><w:name w:val=\"heading 5\"/>",
    "<w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/>",
    "<w:spacing w:before=\"80\" w:after=\"40\"/><w:outlineLvl w:val=\"4\"/></w:pPr>",
    "<w:rPr><w:b/><w:bCs/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:style>",
    "<w:style w:type=\"paragraph\" w:styleId=\"Heading6\"><w:name w:val=\"heading 6\"/>",
    "<w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/>",
    "<w:spacing w:before=\"80\" w:after=\"40\"/><w:outlineLvl w:val=\"5\"/></w:pPr>",
    "<w:rPr><w:b/><w:bCs/><w:i/><w:iCs/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr>",
    "</w:style>",
    "<w:style w:type=\"paragraph\" w:styleId=\"ListParagraph\">",
    "<w:name w:val=\"List Paragraph\"/><w:basedOn w:val=\"Normal\"/>",
    "<w:uiPriority w:val=\"34\"/><w:qFormat/><w:pPr><w:ind w:left=\"720\"/>",
    "<w:contextualSpacing/></w:pPr></w:style>",
    "<w:style w:type=\"paragraph\" w:styleId=\"Quote\"><w:name w:val=\"Quote\"/>",
    "<w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:pBdr>",
    "<w:left w:val=\"single\" w:sz=\"18\" w:space=\"8\" w:color=\"BFBFBF\"/></w:pBdr>",
    "<w:ind w:left=\"720\"/></w:pPr><w:rPr><w:i/><w:iCs/><w:color w:val=\"404040\"/>",
    "</w:rPr></w:style>",
    "<w:style w:type=\"paragraph\" w:styleId=\"CodeBlock\"><w:name w:val=\"Code Block\"/>",
    "<w:basedOn w:val=\"Normal\"/><w:qFormat/><w:pPr>",
    "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F2F2F2\"/>",
    "<w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/><w:contextualSpacing/>",
    "</w:pPr><w:rPr><w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\" w:cs=\"Consolas\"/>",
    "<w:sz w:val=\"20\"/><w:szCs w:val=\"20\"/></w:rPr></w:style>",
    "<w:style w:type=\"character\" w:styleId=\"CodeChar\"><w:name w:val=\"Code Char\"/>",
    "<w:basedOn w:val=\"DefaultParagraphFont\"/><w:qFormat/><w:rPr>",
    "<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\" w:cs=\"Consolas\"/>",
    "<w:sz w:val=\"20\"/><w:szCs w:val=\"20\"/>",
    "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F2F2F2\"/></w:rPr></w:style>",
    "<w:style w:type=\"character\" w:styleId=\"Hyperlink\"><w:name w:val=\"Hyperlink\"/>",
    "<w:basedOn w:val=\"DefaultParagraphFont\"/><w:uiPriority w:val=\"99\"/><w:rPr>",
    "<w:color w:val=\"0563C1\"/><w:u w:val=\"single\"/></w:rPr></w:style>",
    "<w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:name w:val=\"Table Grid\"/>",
    "<w:basedOn w:val=\"TableNormal\"/><w:uiPriority w:val=\"39\"/><w:pPr>",
    "<w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr><w:tblPr>",
    "<w:tblBorders><w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>",
    "<w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>",
    "<w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>",
    "<w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>",
    "<w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>",
    "<w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/></w:tblBorders>",
    "</w:tblPr></w:style></w:styles>",
)
.as_bytes();

const NUMBERING_HEAD: &[u8] = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
)
.as_bytes();

const RELATIONSHIPS_HEAD: &[u8] = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
    "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/",
    "relationships/styles\" Target=\"styles.xml\"/>",
    "<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/",
    "relationships/numbering\" Target=\"numbering.xml\"/>",
)
.as_bytes();

const HYPERLINK_RELATIONSHIP_TYPE: &[u8] = concat!(
    "\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink\" ",
    "Target=\"",
)
.as_bytes();

const TABLE_HEAD: &[u8] = concat!(
    "<w:tbl><w:tblPr><w:tblStyle w:val=\"TableGrid\"/><w:tblW w:w=\"0\" w:type=\"auto\"/>",
    "<w:tblLook w:val=\"04A0\" w:firstRow=\"1\" w:lastRow=\"0\" w:firstColumn=\"1\" ",
    "w:lastColumn=\"0\" w:noHBand=\"0\" w:noVBand=\"1\"/></w:tblPr><w:tblGrid>",
)
.as_bytes();

const RULE_BORDER: &[u8] =
    b"<w:pBdr><w:bottom w:val=\"single\" w:sz=\"6\" w:space=\"1\" w:color=\"auto\"/></w:pBdr>";

const BULLET_GLYPHS: [&[u8]; 3] =
    ["\u{2022}".as_bytes(), "\u{25E6}".as_bytes(), "\u{25AA}".as_bytes()];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParagraphStyle {
    Code,
    Heading(u8),
    Normal,
    Rule,
}

#[derive(Clone, Copy, Debug)]
struct TableCellStyle {
    alignment: Alignment,
    colspan: u32,
    header: bool,
}

#[derive(Clone, Copy, Debug)]
struct CellState {
    alignment: Alignment,
    header: bool,
    paragraph_implicit: bool,
}

impl CellState {
    const EMPTY: Self =
        Self { alignment: Alignment::None, header: false, paragraph_implicit: false };
}

#[derive(Clone, Copy, Debug)]
struct NumberingInstance {
    identifier: u32,
    level: u32,
    start: u32,
}

#[derive(Debug)]
struct Writer<'a, 'b, 'c, 'd> {
    cell_count: u32,
    cells: [CellState; CELL_STACK_MAX as usize],
    document: &'c Document,
    href_open: Span,
    hyperlink_open: bool,
    item_marker_pending: [bool; CELL_STACK_MAX as usize],
    item_task_pending: [TaskState; CELL_STACK_MAX as usize],
    link_count: &'d mut u32,
    links: &'d mut [Span],
    list_depth: u32,
    list_numbering_identifier: [u32; CELL_STACK_MAX as usize],
    ordered_count: u32,
    quote_depth: u32,
    sink: &'b mut Sink<'a>,
}

pub fn write(document: &Document, workspace: &mut Workspace, output: &mut [u8]) -> Result<u32> {
    assert!(document.node_count() >= 1);
    assert!(document.node(NODE_ROOT).kind == NodeKind::Document);

    workspace.reset();

    let mut sink = Sink::new(output);
    let mut zip = ZipWriter::new(&mut sink);

    zip.entry_begin(b"[Content_Types].xml")?;
    zip.sink().write(CONTENT_TYPES)?;
    zip.entry_end();

    zip.entry_begin(b"_rels/.rels")?;
    zip.sink().write(PACKAGE_RELATIONSHIPS)?;
    zip.entry_end();

    zip.entry_begin(b"word/document.xml")?;
    document_part_write(document, workspace, zip.sink())?;
    zip.entry_end();

    zip.entry_begin(b"word/styles.xml")?;
    zip.sink().write(STYLES)?;
    zip.entry_end();

    zip.entry_begin(b"word/numbering.xml")?;
    numbering_part_write(document, zip.sink())?;
    zip.entry_end();

    zip.entry_begin(b"word/_rels/document.xml.rels")?;
    relationships_part_write(document, workspace, zip.sink())?;
    zip.entry_end();

    zip.finish()?;

    assert!(sink.length() > 0);

    Ok(sink.length())
}

fn document_part_write(
    document: &Document,
    workspace: &mut Workspace,
    sink: &mut Sink<'_>,
) -> Result<()> {
    assert!(document.node_count() >= 1);
    assert!(workspace.link_count == 0);

    let Workspace { links, link_count, .. } = workspace;

    let mut writer = Writer {
        cell_count: 0,
        cells: [CellState::EMPTY; CELL_STACK_MAX as usize],
        document,
        href_open: Span::EMPTY,
        hyperlink_open: false,
        item_marker_pending: [false; CELL_STACK_MAX as usize],
        item_task_pending: [TaskState::None; CELL_STACK_MAX as usize],
        link_count,
        links,
        list_depth: 0,
        list_numbering_identifier: [0; CELL_STACK_MAX as usize],
        ordered_count: 0,
        quote_depth: 0,
        sink,
    };

    let mut walk = Walk::new(document, NODE_ROOT);

    for _ in 0..document.node_count() * 2 {
        let Some(event) = walk.next(document) else {
            break;
        };

        match event {
            WalkEvent::Enter(index) => writer.enter(index, &mut walk)?,
            WalkEvent::Leave(index) => writer.leave(index)?,
        }
    }

    assert!(writer.list_depth == 0);
    assert!(writer.cell_count == 0);

    Ok(())
}

impl Writer<'_, '_, '_, '_> {
    fn enter(&mut self, index: u32, walk: &mut Walk) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(self.cell_count <= CELL_STACK_MAX);

        let node = *self.document.node(index);

        match node.kind {
            NodeKind::Document => self.sink.write(DOCUMENT_HEAD),
            NodeKind::Paragraph => self.paragraph_begin(ParagraphStyle::Normal),
            NodeKind::Heading { level } => self.paragraph_begin(ParagraphStyle::Heading(level)),
            NodeKind::BlockQuote => {
                self.quote_depth += 1;

                Ok(())
            }
            NodeKind::List { ordered, .. } => self.list_enter(ordered),
            NodeKind::ListItem { task } => self.list_item_enter(index, task),
            NodeKind::CodeBlock { .. } | NodeKind::HTMLBlock { .. } => {
                self.code_block(index)?;
                walk.skip_children(self.document);

                Ok(())
            }
            NodeKind::ThematicBreak => {
                self.paragraph_begin(ParagraphStyle::Rule)?;

                self.paragraph_end()
            }
            NodeKind::Table { column_count } => self.table_begin(column_count),
            NodeKind::TableRow { header } => {
                self.sink.write(b"<w:tr>")?;

                if header {
                    self.sink.write(b"<w:trPr><w:tblHeader/></w:trPr>")?;
                }

                Ok(())
            }
            NodeKind::TableCell { alignment, colspan, header } => {
                self.cell_enter(index, TableCellStyle { alignment, colspan, header })
            }
            NodeKind::Text { href, marks, text } => self.text(TextRun { href, marks, text }),
            NodeKind::HardBreak => {
                self.hyperlink_close()?;

                self.sink.write(b"<w:r><w:br/></w:r>")
            }
            NodeKind::Image { alt, url } => {
                let label = if alt.is_empty() { url } else { alt };

                self.text(TextRun { href: url, marks: Marks::NONE, text: label })
            }
            NodeKind::HTMLInline { text } => {
                self.text(TextRun { href: Span::EMPTY, marks: Marks::NONE, text })
            }
            NodeKind::Unused => unreachable!("unused node reached by walk"),
        }
    }

    fn leave(&mut self, index: u32) -> Result<()> {
        assert!(index < self.document.node_count());

        let node = *self.document.node(index);

        match node.kind {
            NodeKind::Document => self.sink.write(DOCUMENT_TAIL),
            NodeKind::Paragraph | NodeKind::Heading { .. } => self.paragraph_end(),
            NodeKind::BlockQuote => {
                assert!(self.quote_depth > 0);

                self.quote_depth -= 1;

                Ok(())
            }
            NodeKind::List { .. } => {
                assert!(self.list_depth > 0);

                self.list_depth -= 1;

                Ok(())
            }
            NodeKind::Table { .. } => {
                self.sink.write(b"</w:tbl>")?;

                if node.sibling_next == NODE_NONE {
                    self.paragraph_begin(ParagraphStyle::Normal)?;
                    self.paragraph_end()?;
                }

                Ok(())
            }
            NodeKind::TableRow { .. } => self.sink.write(b"</w:tr>"),
            NodeKind::TableCell { .. } => self.cell_leave(),
            NodeKind::ListItem { .. }
            | NodeKind::CodeBlock { .. }
            | NodeKind::ThematicBreak
            | NodeKind::Text { .. }
            | NodeKind::HardBreak
            | NodeKind::Image { .. }
            | NodeKind::HTMLBlock { .. }
            | NodeKind::HTMLInline { .. } => Ok(()),
            NodeKind::Unused => unreachable!("unused node reached by walk"),
        }
    }

    fn list_enter(&mut self, ordered: bool) -> Result<()> {
        assert!(self.list_depth <= CELL_STACK_MAX);

        if self.list_depth >= u32::from(DEPTH_MAX) {
            return Err(Error::DepthExceeded { depth_max: DEPTH_MAX });
        }

        let numbering_identifier = if ordered {
            self.ordered_count += 1;

            NUMBERING_IDENTIFIER_BULLET + self.ordered_count
        } else {
            NUMBERING_IDENTIFIER_BULLET
        };

        self.list_numbering_identifier[self.list_depth as usize] = numbering_identifier;
        self.list_depth += 1;

        assert!(self.list_depth <= u32::from(DEPTH_MAX));

        Ok(())
    }

    fn list_item_enter(&mut self, index: u32, task: TaskState) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(self.list_depth > 0);

        let level = (self.list_depth - 1) as usize;
        self.item_marker_pending[level] = true;
        self.item_task_pending[level] = task;

        let first = self.document.node(index).child_first;

        let child_first_carries_marker = first != NODE_NONE
            && matches!(
                self.document.node(first).kind,
                NodeKind::Paragraph | NodeKind::Heading { .. } | NodeKind::CodeBlock { .. }
            );

        if child_first_carries_marker {
            return Ok(());
        }

        self.paragraph_begin(ParagraphStyle::Normal)?;

        self.paragraph_end()
    }

    fn paragraph_begin(&mut self, style: ParagraphStyle) -> Result<()> {
        assert!(!self.hyperlink_open);

        self.sink.write(b"<w:p><w:pPr>")?;

        match style {
            ParagraphStyle::Heading(level) => {
                assert!(level >= 1);
                assert!(level <= HEADING_LEVEL_MAX);

                self.sink.write(b"<w:pStyle w:val=\"Heading")?;
                self.sink.write_u32(u32::from(level))?;
                self.sink.write(b"\"/>")?;
            }
            ParagraphStyle::Code => self.sink.write(b"<w:pStyle w:val=\"CodeBlock\"/>")?,
            ParagraphStyle::Normal | ParagraphStyle::Rule => {
                let list_continuation = self.list_depth > 0
                    && !self.item_marker_pending[(self.list_depth - 1) as usize];

                if self.quote_depth > 0 {
                    self.sink.write(b"<w:pStyle w:val=\"Quote\"/>")?;
                } else if list_continuation {
                    self.sink.write(b"<w:pStyle w:val=\"ListParagraph\"/>")?;
                }
            }
        }

        if style == ParagraphStyle::Rule {
            self.sink.write(RULE_BORDER)?;
        }

        let task = self.numbering_write()?;

        self.alignment_write()?;
        self.sink.write(b"</w:pPr>")?;

        self.task_glyph(task)
    }

    fn numbering_write(&mut self) -> Result<TaskState> {
        assert!(self.list_depth <= CELL_STACK_MAX);

        if self.list_depth == 0 {
            if self.quote_depth > 1 {
                self.indent_write(INDENT_TWIPS * self.quote_depth)?;
            }

            return Ok(TaskState::None);
        }

        let level = (self.list_depth - 1) as usize;

        if !self.item_marker_pending[level] {
            self.indent_write(INDENT_TWIPS * (self.list_depth + self.quote_depth))?;

            return Ok(TaskState::None);
        }

        self.sink.write(b"<w:numPr><w:ilvl w:val=\"")?;
        self.sink.write_u32((self.list_depth - 1).min(DOCX_LEVEL_COUNT_MAX - 1))?;
        self.sink.write(b"\"/><w:numId w:val=\"")?;
        self.sink.write_u32(self.list_numbering_identifier[level])?;
        self.sink.write(b"\"/></w:numPr>")?;
        self.item_marker_pending[level] = false;

        let task = self.item_task_pending[level];
        self.item_task_pending[level] = TaskState::None;

        Ok(task)
    }

    fn alignment_write(&mut self) -> Result<()> {
        assert!(self.cell_count <= CELL_STACK_MAX);
        assert!(!self.hyperlink_open);

        if self.cell_count == 0 {
            return Ok(());
        }

        let alignment = self.cells[(self.cell_count - 1) as usize].alignment;

        let value: &[u8] = match alignment {
            Alignment::None => b"",
            Alignment::Left => b"<w:jc w:val=\"left\"/>",
            Alignment::Center => b"<w:jc w:val=\"center\"/>",
            Alignment::Right => b"<w:jc w:val=\"right\"/>",
        };

        self.sink.write(value)
    }

    fn indent_write(&mut self, twips: u32) -> Result<()> {
        assert!(twips > 0);
        assert!(twips.is_multiple_of(INDENT_TWIPS));

        self.sink.write(b"<w:ind w:left=\"")?;
        self.sink.write_u32(twips)?;

        self.sink.write(b"\"/>")
    }

    fn task_glyph(&mut self, task: TaskState) -> Result<()> {
        assert!(!self.hyperlink_open);

        let glyph: &[u8] = match task {
            TaskState::None => return Ok(()),
            TaskState::Unchecked => "\u{2610} ".as_bytes(),
            TaskState::Checked => "\u{2611} ".as_bytes(),
        };

        assert!(!glyph.is_empty());

        self.sink.write(b"<w:r><w:t xml:space=\"preserve\">")?;
        self.sink.write(glyph)?;

        self.sink.write(b"</w:t></w:r>")
    }

    fn paragraph_end(&mut self) -> Result<()> {
        assert!(self.cell_count <= CELL_STACK_MAX);

        self.hyperlink_close()?;

        assert!(!self.hyperlink_open);

        self.sink.write(b"</w:p>")
    }

    fn hyperlink_close(&mut self) -> Result<()> {
        assert!(self.hyperlink_open || self.href_open == Span::EMPTY);

        if self.hyperlink_open {
            self.sink.write(b"</w:hyperlink>")?;
            self.hyperlink_open = false;
            self.href_open = Span::EMPTY;
        }

        assert!(!self.hyperlink_open);

        Ok(())
    }

    fn hyperlink_open(&mut self, href: Span) -> Result<()> {
        assert!(!self.hyperlink_open);
        assert!(href.length > 0);

        if *self.link_count >= LINK_COUNT_MAX {
            return Err(Error::LinkCapacity { link_count_max: LINK_COUNT_MAX });
        }

        let index = *self.link_count;
        self.links[index as usize] = href;
        *self.link_count += 1;

        self.sink.write(b"<w:hyperlink r:id=\"rId")?;
        self.sink.write_u32(HYPERLINK_RELATIONSHIP_IDENTIFIER_FIRST + index)?;
        self.sink.write(b"\">")?;
        self.hyperlink_open = true;
        self.href_open = href;

        Ok(())
    }

    fn text(&mut self, run: TextRun) -> Result<()> {
        assert!(run.text.end() <= self.document.text_length());
        assert!(run.href.end() <= self.document.text_length());

        let TextRun { href, marks, text } = run;

        if text.is_empty() {
            return Ok(());
        }

        if self.hyperlink_open {
            if href != self.href_open {
                self.hyperlink_close()?;
            }
        }

        if href.length > 0 {
            if !self.hyperlink_open {
                self.hyperlink_open(href)?;
            }
        }

        let header = self.cell_count > 0 && self.cells[(self.cell_count - 1) as usize].header;
        let bold = marks.contains(Marks::STRONG) || header;

        self.sink.write(b"<w:r><w:rPr>")?;

        if self.hyperlink_open {
            self.sink.write(b"<w:rStyle w:val=\"Hyperlink\"/>")?;
        } else if marks.contains(Marks::CODE) {
            self.sink.write(b"<w:rStyle w:val=\"CodeChar\"/>")?;
        }

        if bold {
            self.sink.write(b"<w:b/><w:bCs/>")?;
        }

        if marks.contains(Marks::EMPHASIS) {
            self.sink.write(b"<w:i/><w:iCs/>")?;
        }

        if marks.contains(Marks::STRIKETHROUGH) {
            self.sink.write(b"<w:strike/>")?;
        }

        self.sink.write(b"</w:rPr>")?;
        run_body_write(self.document.span_bytes(text), self.sink)?;

        self.sink.write(b"</w:r>")
    }

    fn code_block(&mut self, index: u32) -> Result<()> {
        assert!(index < self.document.node_count());

        let document = self.document;
        let mut children = document.children(index);
        let mut any = false;

        if let NodeKind::HTMLBlock { text } = document.node(index).kind {
            self.code_lines(document.span_bytes(text))?;

            return Ok(());
        }

        for _ in 0..document.node_count() {
            let Some(child) = children.next(document) else {
                break;
            };

            let NodeKind::Text { text, .. } = document.node(child).kind else {
                continue;
            };

            let bytes = document.span_bytes(text);

            self.code_lines(bytes)?;
            any = true;
        }

        if !any {
            self.paragraph_begin(ParagraphStyle::Code)?;
            self.paragraph_end()?;
        }

        Ok(())
    }

    fn code_lines(&mut self, bytes: &[u8]) -> Result<()> {
        assert!(!self.hyperlink_open);

        let before = self.sink.length();
        let mut lines = bytes.split(|&byte| byte == b'\n');

        for _ in 0..=bytes.len() {
            let Some(line) = lines.next() else {
                break;
            };

            self.paragraph_begin(ParagraphStyle::Code)?;
            self.sink.write(b"<w:r>")?;
            run_body_write(line, self.sink)?;
            self.sink.write(b"</w:r>")?;
            self.paragraph_end()?;
        }

        assert!(self.sink.length() > before);

        Ok(())
    }

    fn table_begin(&mut self, column_count: u32) -> Result<()> {
        assert!(column_count <= NODE_COUNT_MAX * TABLE_COLSPAN_MAX);
        assert!(!self.hyperlink_open);
        assert!(self.cell_count <= CELL_STACK_MAX);

        self.sink.write(TABLE_HEAD)?;

        for _ in 0..column_count.max(1) {
            self.sink.write(b"<w:gridCol w:w=\"2000\"/>")?;
        }

        self.sink.write(b"</w:tblGrid>")
    }

    fn cell_enter(&mut self, index: u32, style: TableCellStyle) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(style.colspan >= 1);

        let TableCellStyle { alignment, colspan, header } = style;

        if self.cell_count >= CELL_STACK_MAX {
            return Err(Error::DepthExceeded { depth_max: DEPTH_MAX });
        }

        self.sink.write(b"<w:tc><w:tcPr>")?;

        if colspan > 1 {
            self.sink.write(b"<w:gridSpan w:val=\"")?;
            self.sink.write_u32(colspan)?;
            self.sink.write(b"\"/>")?;
        }

        self.sink.write(b"</w:tcPr>")?;

        let first = self.document.node(index).child_first;
        let paragraph_implicit = first == NODE_NONE || self.document.node(first).kind.is_inline();
        self.cells[self.cell_count as usize] = CellState { alignment, header, paragraph_implicit };
        self.cell_count += 1;

        if paragraph_implicit {
            self.paragraph_begin(ParagraphStyle::Normal)?;
        }

        Ok(())
    }

    fn cell_leave(&mut self) -> Result<()> {
        assert!(self.cell_count > 0);

        let state = self.cells[(self.cell_count - 1) as usize];

        if state.paragraph_implicit {
            self.paragraph_end()?;
        }

        self.cell_count -= 1;

        self.sink.write(b"</w:tc>")
    }
}

fn run_body_write(text: &[u8], sink: &mut Sink<'_>) -> Result<()> {
    let before = sink.length();
    let mut start = 0usize;

    for (index, &byte) in text.iter().enumerate() {
        let element: &[u8] = match byte {
            b'\t' => b"<w:tab/>",
            b'\n' => b"<w:br/>",
            b'\r' => b"",
            _ => continue,
        };

        text_element_write(&text[start..index], sink)?;
        sink.write(element)?;
        start = index + 1;
    }

    assert!(start <= text.len());
    assert!(sink.length() >= before);

    text_element_write(&text[start..], sink)
}

fn text_element_write(text: &[u8], sink: &mut Sink<'_>) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    let before = sink.length();

    sink.write(b"<w:t xml:space=\"preserve\">")?;
    escape_text(text, sink)?;
    sink.write(b"</w:t>")?;

    assert!(sink.length() > before);

    Ok(())
}

fn numbering_part_write(document: &Document, sink: &mut Sink<'_>) -> Result<()> {
    assert!(document.node_count() >= 1);

    sink.write(NUMBERING_HEAD)?;
    abstract_write(sink, 0, false)?;
    abstract_write(sink, 1, true)?;
    sink.write(b"<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>")?;

    let mut walk = Walk::new(document, NODE_ROOT);
    let mut list_depth: u32 = 0;
    let mut ordered_count: u32 = 0;

    for _ in 0..document.node_count() * 2 {
        let Some(event) = walk.next(document) else {
            break;
        };

        match event {
            WalkEvent::Enter(index) => {
                if let NodeKind::List { ordered, start, .. } = document.node(index).kind {
                    if ordered {
                        ordered_count += 1;

                        let instance = NumberingInstance {
                            identifier: NUMBERING_IDENTIFIER_BULLET + ordered_count,
                            level: list_depth,
                            start,
                        };

                        numbering_write(sink, instance)?;
                    }

                    list_depth += 1;
                }
            }
            WalkEvent::Leave(index) => {
                if let NodeKind::List { .. } = document.node(index).kind {
                    list_depth -= 1;
                }
            }
        }
    }

    assert!(list_depth == 0);

    sink.write(b"</w:numbering>")
}

fn abstract_write(sink: &mut Sink<'_>, identifier: u32, ordered: bool) -> Result<()> {
    assert!(identifier <= 1);
    assert!(sink.length() > 0);

    sink.write(b"<w:abstractNum w:abstractNumId=\"")?;
    sink.write_u32(identifier)?;
    sink.write(b"\"><w:multiLevelType w:val=\"hybridMultilevel\"/>")?;

    for level in 0..DOCX_LEVEL_COUNT_MAX {
        sink.write(b"<w:lvl w:ilvl=\"")?;
        sink.write_u32(level)?;
        sink.write(b"\"><w:start w:val=\"1\"/>")?;

        if ordered {
            sink.write(b"<w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%")?;
            sink.write_u32(level + 1)?;
            sink.write(b".\"/>")?;
        } else {
            sink.write(b"<w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"")?;
            sink.write(BULLET_GLYPHS[(level % 3) as usize])?;
            sink.write(b"\"/>")?;
        }

        sink.write(b"<w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"")?;
        sink.write_u32(INDENT_TWIPS * (level + 1))?;
        sink.write(b"\" w:hanging=\"360\"/></w:pPr></w:lvl>")?;
    }

    sink.write(b"</w:abstractNum>")
}

fn numbering_write(sink: &mut Sink<'_>, instance: NumberingInstance) -> Result<()> {
    assert!(instance.identifier > NUMBERING_IDENTIFIER_BULLET);
    assert!(sink.length() > 0);

    sink.write(b"<w:num w:numId=\"")?;
    sink.write_u32(instance.identifier)?;
    sink.write(b"\"><w:abstractNumId w:val=\"1\"/><w:lvlOverride w:ilvl=\"")?;
    sink.write_u32(instance.level.min(DOCX_LEVEL_COUNT_MAX - 1))?;
    sink.write(b"\"><w:startOverride w:val=\"")?;
    sink.write_u32(instance.start.max(1))?;

    sink.write(b"\"/></w:lvlOverride></w:num>")
}

fn relationships_part_write(
    document: &Document,
    workspace: &Workspace,
    sink: &mut Sink<'_>,
) -> Result<()> {
    assert!(workspace.link_count <= LINK_COUNT_MAX);
    assert!(document.node_count() >= 1);

    sink.write(RELATIONSHIPS_HEAD)?;

    for (index, &href) in workspace.links[..workspace.link_count as usize].iter().enumerate() {
        sink.write(b"<Relationship Id=\"rId")?;
        sink.write_u32(HYPERLINK_RELATIONSHIP_IDENTIFIER_FIRST + u32_from_usize(index))?;
        sink.write(HYPERLINK_RELATIONSHIP_TYPE)?;
        target_write(document.span_bytes(href), sink)?;
        sink.write(b"\" TargetMode=\"External\"/>")?;
    }

    sink.write(b"</Relationships>")
}

fn target_write(href: &[u8], sink: &mut Sink<'_>) -> Result<()> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let before = sink.length();

    for &byte in href {
        let safe = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b':'
                    | b'/'
                    | b'?'
                    | b'#'
                    | b'['
                    | b']'
                    | b'@'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b'%',
            );

        if safe {
            escape_attribute(&[byte], sink)?;
        } else {
            sink.write_byte(b'%')?;
            sink.write_byte(HEX[usize::from(byte >> 4u8)])?;
            sink.write_byte(HEX[usize::from(byte & 0x0f)])?;
        }
    }

    assert!(sink.length() >= before + u32_from_usize(href.len()));

    Ok(())
}
