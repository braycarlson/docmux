use crate::bytes::{decimal_parse_u32, u32_from_usize, utf8_validate};
use crate::document::{
    Alignment,
    DEPTH_MAX,
    Document,
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
use crate::docx::parts::{
    Element,
    LevelFormat,
    ListLevel,
    NumberingTables,
    Part,
    StyleKind,
    numbering_level,
    numbering_parse,
    office_document_target,
    relationship_target,
    relationships_parse,
    style_kind,
    styles_parse,
};
use crate::docx::{
    DOCX_LEVEL_COUNT_MAX,
    DOCX_STYLE_COUNT_MAX,
    DocxAbstract,
    DocxNumbering,
    DocxRelationship,
    DocxStyle,
};
use crate::error::{Error, Result};
use crate::workspace::{PART_BYTES_MAX, Workspace};
use crate::xml::{Attributes, XMLEvent, XMLReader, decode};
use crate::zip::ZipArchive;

const FRAME_COUNT_MAX: u32 = DEPTH_MAX as u32;
const PART_NAME_LENGTH_MAX: usize = 128;

const TASK_GLYPHS: [(&[u8], TaskState); 3] = [
    ("\u{2610} ".as_bytes(), TaskState::Unchecked),
    ("\u{2611} ".as_bytes(), TaskState::Checked),
    ("\u{2612} ".as_bytes(), TaskState::Checked),
];

#[derive(Clone, Copy, Debug)]
struct ListState {
    item: u32,
    node: u32,
    numbering_identifier: u32,
}

impl ListState {
    const EMPTY: Self = Self { item: NODE_NONE, node: NODE_NONE, numbering_identifier: 0 };
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    code_open: u32,
    container: u32,
    header_pending: bool,
    list_depth: u32,
    lists: [ListState; DOCX_LEVEL_COUNT_MAX as usize],
    quote_open: u32,
    row: u32,
    row_index: u32,
    table: u32,
}

impl Frame {
    const fn new(container: u32) -> Self {
        assert!(container < NODE_COUNT_MAX);

        Self {
            code_open: NODE_NONE,
            container,
            header_pending: false,
            list_depth: 0,
            lists: [ListState::EMPTY; DOCX_LEVEL_COUNT_MAX as usize],
            quote_open: NODE_NONE,
            row: NODE_NONE,
            row_index: 0,
            table: NODE_NONE,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct TaskParagraph {
    item: u32,
    paragraph: u32,
}

#[derive(Clone, Copy, Debug)]
struct ParagraphState {
    alignment: Alignment,
    level: u32,
    node: u32,
    numbering_identifier: Option<u32>,
    rule: bool,
    style: StyleKind,
    task_item: u32,
}

impl ParagraphState {
    const EMPTY: Self = Self {
        alignment: Alignment::None,
        level: 0,
        node: NODE_NONE,
        numbering_identifier: None,
        rule: false,
        style: StyleKind::Normal,
        task_item: NODE_NONE,
    };
}

#[derive(Clone, Copy, Debug)]
struct Tables<'a> {
    abstracts: &'a [DocxAbstract],
    numberings: &'a [DocxNumbering],
    relationships: &'a [DocxRelationship],
    relationships_part: Part<'a>,
    styles: &'a [DocxStyle],
    styles_part: Part<'a>,
}

#[derive(Debug)]
struct Reader<'a> {
    cell_colspan: u32,
    frame_count: u32,
    frames: [Frame; FRAME_COUNT_MAX as usize],
    href: Span,
    in_paragraph: bool,
    in_text: bool,
    marks: Marks,
    paragraph: ParagraphState,
    tables: Tables<'a>,
}

#[derive(Clone, Copy, Debug)]
struct PartLengths {
    numbering: u32,
    part: u32,
    relationships: u32,
    styles: u32,
}

pub fn read(bytes: &[u8], workspace: &mut Workspace, document: &mut Document) -> Result<()> {
    document.reset();
    workspace.reset();

    assert!(document.node_count() == 1);

    let archive = ZipArchive::open(bytes)?;
    let lengths = parts_extract(&archive, workspace)?;

    assert!(lengths.part as usize <= workspace.part.len());

    let Workspace {
        docx_abstract_count,
        docx_abstracts,
        docx_numbering_count,
        docx_numberings,
        docx_relationship_count,
        docx_relationships,
        docx_style_count,
        docx_styles,
        numbering,
        part,
        relationships,
        styles,
        ..
    } = workspace;

    let styles_part = Part::new(&styles[..lengths.styles as usize]);
    let relationships_part = Part::new(&relationships[..lengths.relationships as usize]);

    styles_parse(styles_part, docx_styles, docx_style_count)?;

    numbering_parse(
        Part::new(&numbering[..lengths.numbering as usize]),
        NumberingTables {
            abstract_count: docx_abstract_count,
            abstracts: docx_abstracts,
            numbering_count: docx_numbering_count,
            numberings: docx_numberings,
        },
    )?;

    relationships_parse(relationships_part, docx_relationships, docx_relationship_count)?;

    let tables = Tables {
        abstracts: &docx_abstracts[..*docx_abstract_count as usize],
        numberings: &docx_numberings[..*docx_numbering_count as usize],
        relationships: &docx_relationships[..*docx_relationship_count as usize],
        relationships_part,
        styles: &docx_styles[..*docx_style_count as usize],
        styles_part,
    };

    body_parse(&part[..lengths.part as usize], tables, document)
}

fn parts_extract(archive: &ZipArchive<'_>, workspace: &mut Workspace) -> Result<PartLengths> {
    let mut part_name = [0u8; PART_NAME_LENGTH_MAX];
    let part_name_length = document_part_name(archive, workspace, &mut part_name)?;

    assert!(part_name_length >= 1);

    let part =
        part_extract(archive, &part_name[..part_name_length], "document", &mut workspace.part)?
            .ok_or(Error::DocxPartMissing { name: "word/document.xml" })?;

    let styles =
        part_extract(archive, b"word/styles.xml", "styles", &mut workspace.styles)?.unwrap_or(0);

    let numbering =
        part_extract(archive, b"word/numbering.xml", "numbering", &mut workspace.numbering)?
            .unwrap_or(0);

    let mut relationships_name = [0u8; PART_NAME_LENGTH_MAX];

    let relationships_name_length =
        relationships_part_name(&part_name[..part_name_length], &mut relationships_name);

    let relationships = part_extract(
        archive,
        &relationships_name[..relationships_name_length],
        "relationships",
        &mut workspace.relationships,
    )?
    .unwrap_or(0);

    assert!(part as usize <= workspace.part.len());

    Ok(PartLengths { numbering, part, relationships, styles })
}

fn part_extract(
    archive: &ZipArchive<'_>,
    name: &[u8],
    label: &'static str,
    buffer: &mut [u8],
) -> Result<Option<u32>> {
    assert!(!label.is_empty());
    assert!(buffer.len() <= PART_BYTES_MAX as usize);

    if name.is_empty() {
        return Ok(None);
    }

    let Some(entry) = archive.entry_find(name)? else {
        return Ok(None);
    };

    let length = archive.entry_extract(&entry, label, buffer)?;

    utf8_validate(&buffer[..length])?;

    Ok(Some(u32_from_usize(length)))
}

fn document_part_name(
    archive: &ZipArchive<'_>,
    workspace: &mut Workspace,
    out: &mut [u8; PART_NAME_LENGTH_MAX],
) -> Result<usize> {
    const DEFAULT: &[u8] = b"word/document.xml";

    assert!(DEFAULT.len() <= PART_NAME_LENGTH_MAX);

    if archive.entry_find(DEFAULT)?.is_some() {
        out[..DEFAULT.len()].copy_from_slice(DEFAULT);

        return Ok(DEFAULT.len());
    }

    let Some(length) = part_extract(
        archive,
        b"_rels/.rels",
        "package relationships",
        &mut workspace.relationships,
    )?
    else {
        return Err(Error::DocxPartMissing { name: "_rels/.rels" });
    };

    let buffer = &workspace.relationships[..length as usize];

    let Some(target) = office_document_target(buffer) else {
        return Err(Error::DocxPartMissing { name: "word/document.xml" });
    };

    if target.is_empty() {
        return Err(Error::DocxPartMissing { name: "word/document.xml" });
    }

    if target.len() > PART_NAME_LENGTH_MAX {
        return Err(Error::DocxPartMissing { name: "word/document.xml" });
    }

    out[..target.len()].copy_from_slice(target);

    assert!(!target.is_empty());

    Ok(target.len())
}

fn relationships_part_name(part_name: &[u8], out: &mut [u8; PART_NAME_LENGTH_MAX]) -> usize {
    assert!(!part_name.is_empty());
    assert!(part_name.len() <= PART_NAME_LENGTH_MAX);

    let slash = part_name.iter().rposition(|&byte| byte == b'/').map_or(0, |index| index + 1);
    let (directory, file) = part_name.split_at(slash);
    let total = directory.len() + b"_rels/".len() + file.len() + b".rels".len();

    if total > PART_NAME_LENGTH_MAX {
        return 0;
    }

    let mut length = 0usize;

    for piece in [directory, b"_rels/", file, b".rels"] {
        out[length..length + piece.len()].copy_from_slice(piece);
        length += piece.len();
    }

    assert!(length == total);

    length
}

fn body_parse(part: &[u8], tables: Tables<'_>, document: &mut Document) -> Result<()> {
    assert!(part.len() <= PART_BYTES_MAX as usize);
    assert!(tables.styles.len() <= DOCX_STYLE_COUNT_MAX as usize);
    assert!(document.node_count() == 1);

    let mut reader = Reader {
        cell_colspan: 1,
        frame_count: 1,
        frames: [Frame::new(NODE_ROOT); FRAME_COUNT_MAX as usize],
        href: Span::EMPTY,
        in_paragraph: false,
        in_text: false,
        marks: Marks::NONE,
        paragraph: ParagraphState::EMPTY,
        tables,
    };

    let mut xml = XMLReader::new(part);

    for _ in 0..part.len() {
        match xml.next()? {
            XMLEvent::Finished => break,
            XMLEvent::Start { attributes, name } => {
                reader.element_start(Element { attributes, name }, &mut xml, document)?;
            }
            XMLEvent::Empty { attributes, name } => {
                reader.element_empty(Element { attributes, name }, document)?;
            }
            XMLEvent::End { name } => reader.element_end(name, document)?,
            XMLEvent::Text { raw, cdata } => {
                if reader.in_text {
                    reader.text_raw(raw, cdata, document)?;
                }
            }
        }
    }

    if reader.frame_count != 1 {
        return Err(Error::DocxMalformed { offset: u32_from_usize(xml.position()) });
    }

    Ok(())
}

impl Reader<'_> {
    fn frame(&mut self) -> &mut Frame {
        assert!(self.frame_count >= 1);
        assert!(self.frame_count <= FRAME_COUNT_MAX);

        &mut self.frames[(self.frame_count - 1) as usize]
    }

    fn frame_push(&mut self, frame: Frame) -> Result<()> {
        assert!(self.frame_count >= 1);
        assert!(frame.list_depth <= DOCX_LEVEL_COUNT_MAX);

        if self.frame_count >= FRAME_COUNT_MAX {
            return Err(Error::DepthExceeded { depth_max: DEPTH_MAX });
        }

        self.frames[self.frame_count as usize] = frame;
        self.frame_count += 1;

        assert!(self.frame_count <= FRAME_COUNT_MAX);

        Ok(())
    }

    fn element_start(
        &mut self,
        element: Element<'_>,
        xml: &mut XMLReader<'_>,
        document: &mut Document,
    ) -> Result<()> {
        assert!(!element.name.is_empty());
        assert!(document.node_count() >= 1);

        let attributes = element.attributes;

        match element.name {
            b"w:p" => {
                self.paragraph_begin();

                Ok(())
            }
            b"w:pPr" => self.paragraph_properties(xml),
            b"w:r" => {
                self.marks = Marks::NONE;

                Ok(())
            }
            b"w:rPr" => self.run_properties(xml),
            b"w:t" => {
                self.in_text = true;

                Ok(())
            }
            b"w:hyperlink" => self.hyperlink_begin(attributes, document),
            b"w:fldSimple" => self.field_simple_begin(attributes, document),
            b"w:tbl" => self.table_begin(document),
            b"w:tr" => self.row_begin(document),
            b"w:trPr" => self.row_properties(xml),
            b"w:tc" => self.cell_begin(document),
            b"w:tcPr" => self.cell_properties(xml, document),
            b"w:drawing" | b"w:pict" | b"w:object" => self.drawing(xml, document),
            b"mc:Fallback"
            | b"w:txbxContent"
            | b"w:del"
            | b"w:moveFrom"
            | b"w:sdtPr"
            | b"w:instrText"
            | b"w:delText"
            | b"w:sectPr"
            | b"w:footnoteReference"
            | b"w:endnoteReference"
            | b"w:commentRangeStart" => xml.skip_element(),
            _ => Ok(()),
        }
    }

    fn element_empty(&mut self, element: Element<'_>, document: &mut Document) -> Result<()> {
        assert!(!element.name.is_empty());
        assert!(document.node_count() >= 1);

        let attributes = element.attributes;

        match element.name {
            b"w:tab" => self.text_bytes(b"\t", document),
            b"w:noBreakHyphen" => self.text_bytes(b"-", document),
            b"w:br" | b"w:cr" => {
                let page = Attributes::get(attributes, b"w:type") == Some(b"page");

                if page {
                    return Ok(());
                }

                if !self.in_paragraph {
                    return Ok(());
                }

                let paragraph = self.paragraph_node(document)?;

                document.node_append(paragraph, NodeKind::HardBreak)?;

                Ok(())
            }
            b"w:p" => {
                self.paragraph_begin();

                self.paragraph_end(document)
            }
            b"w:tc" => {
                self.cell_begin(document)?;

                self.cell_end()
            }
            b"w:fldSimple" => self.field_simple_begin(attributes, document),
            _ => Ok(()),
        }
    }

    fn element_end(&mut self, name: &[u8], document: &mut Document) -> Result<()> {
        assert!(!name.is_empty());
        assert!(document.node_count() >= 1);

        match name {
            b"w:p" => self.paragraph_end(document),
            b"w:t" => {
                self.in_text = false;

                Ok(())
            }
            b"w:r" => {
                self.marks = Marks::NONE;

                Ok(())
            }
            b"w:hyperlink" | b"w:fldSimple" => {
                self.href = Span::EMPTY;

                Ok(())
            }
            b"w:tbl" => self.table_end(document),
            b"w:tr" => self.row_end(document),
            b"w:tc" => self.cell_end(),
            _ => Ok(()),
        }
    }

    fn paragraph_begin(&mut self) {
        assert!(self.frame_count >= 1);

        self.in_paragraph = true;
        self.paragraph = ParagraphState::EMPTY;
        self.marks = Marks::NONE;

        assert!(self.paragraph.node == NODE_NONE);
    }

    fn paragraph_properties(&mut self, xml: &mut XMLReader<'_>) -> Result<()> {
        assert!(self.in_paragraph);

        let mut depth: u32 = 1;

        for _ in 0..u32::MAX {
            match xml.next()? {
                XMLEvent::Finished => {
                    return Err(Error::DocxMalformed { offset: u32_from_usize(xml.position()) });
                }
                XMLEvent::Start { .. } => depth += 1,
                XMLEvent::End { name } => {
                    depth -= 1;

                    if depth == 0 {
                        assert!(name == b"w:pPr");

                        return Ok(());
                    }
                }
                XMLEvent::Empty { attributes, name } => {
                    self.paragraph_property(Element { attributes, name });
                }
                XMLEvent::Text { .. } => {}
            }
        }

        Err(Error::DocxMalformed { offset: u32_from_usize(xml.position()) })
    }

    fn paragraph_property(&mut self, element: Element<'_>) {
        assert!(!element.name.is_empty());
        assert!(self.in_paragraph);

        let value = Attributes::get(element.attributes, b"w:val");

        match element.name {
            b"w:pStyle" => {
                let identifier = value.unwrap_or(b"");

                self.paragraph.style =
                    style_kind(self.tables.styles_part, self.tables.styles, identifier);
            }
            b"w:numId" => self.paragraph.numbering_identifier = value.and_then(decimal_parse_u32),
            b"w:ilvl" => self.paragraph.level = value.and_then(decimal_parse_u32).unwrap_or(0),
            b"w:jc" => self.paragraph.alignment = alignment_parse(value),
            b"w:bottom" | b"w:top" => self.paragraph.rule = true,
            _ => {}
        }
    }

    fn run_properties(&mut self, xml: &mut XMLReader<'_>) -> Result<()> {
        assert!(xml.position() > 0);
        assert!(self.frame_count >= 1);

        let mut depth: u32 = 1;

        for _ in 0..u32::MAX {
            match xml.next()? {
                XMLEvent::Finished => {
                    return Err(Error::DocxMalformed { offset: u32_from_usize(xml.position()) });
                }
                XMLEvent::Start { .. } => depth += 1,
                XMLEvent::End { .. } => {
                    depth -= 1;

                    if depth == 0 {
                        return Ok(());
                    }
                }
                XMLEvent::Empty { attributes, name } => {
                    self.run_property(Element { attributes, name });
                }
                XMLEvent::Text { .. } => {}
            }
        }

        Err(Error::DocxMalformed { offset: u32_from_usize(xml.position()) })
    }

    fn run_property(&mut self, element: Element<'_>) {
        assert!(!element.name.is_empty());
        assert!(self.frame_count >= 1);

        let value = Attributes::get(element.attributes, b"w:val");

        let mark = match element.name {
            b"w:b" => Marks::STRONG,
            b"w:i" => Marks::EMPHASIS,
            b"w:strike" | b"w:dstrike" => Marks::STRIKETHROUGH,
            b"w:rStyle" => {
                let identifier = value.unwrap_or(b"");
                let kind = style_kind(self.tables.styles_part, self.tables.styles, identifier);

                if kind == StyleKind::Code {
                    self.marks = self.marks.union(Marks::CODE);
                }

                return;
            }
            _ => return,
        };

        if toggle_is_on(value) {
            self.marks = self.marks.union(mark);
        } else {
            self.marks = self.marks.without(mark);
        }
    }

    fn hyperlink_begin(&mut self, attributes: &[u8], document: &mut Document) -> Result<()> {
        assert!(document.node_count() >= 1);

        let Some(identifier) = Attributes::get(attributes, b"r:id") else {
            return Ok(());
        };

        let Some(target) = relationship_target(
            self.tables.relationships_part,
            self.tables.relationships,
            identifier,
        ) else {
            return Ok(());
        };

        let start = document.text_length();

        decode(target, document)?;
        self.href = Span { length: document.text_length() - start, offset: start };

        assert!(self.href.end() == document.text_length());

        Ok(())
    }

    fn field_simple_begin(&mut self, attributes: &[u8], document: &mut Document) -> Result<()> {
        assert!(document.node_count() >= 1);

        let Some(instruction) = Attributes::get(attributes, b"w:instr") else {
            return Ok(());
        };

        let start = document.text_length();

        decode(instruction, document)?;
        let decoded = Span { length: document.text_length() - start, offset: start };
        let bytes = document.span_bytes(decoded);
        let trimmed = bytes.trim_ascii();

        let Some(rest) = trimmed.strip_prefix(b"HYPERLINK") else {
            return Ok(());
        };

        let after_keyword = rest.trim_ascii_start();

        let url = after_keyword.strip_prefix(b"\"").map_or_else(
            || {
                let end = after_keyword
                    .iter()
                    .position(u8::is_ascii_whitespace)
                    .unwrap_or(after_keyword.len());

                &after_keyword[..end]
            },
            |quoted| {
                let end = quoted.iter().position(|&byte| byte == b'"').unwrap_or(quoted.len());

                &quoted[..end]
            },
        );

        if url.is_empty() {
            return Ok(());
        }

        let url_offset = decoded.offset + Part::new(bytes).span_of(url).offset;
        self.href = Span { length: u32_from_usize(url.len()), offset: url_offset };

        assert!(self.href.end() <= document.text_length());

        Ok(())
    }

    fn text_raw(&mut self, raw: &[u8], cdata: bool, document: &mut Document) -> Result<()> {
        assert!(self.in_text);
        assert!(document.node_count() >= 1);

        if !self.in_paragraph {
            return Ok(());
        }

        let paragraph = self.paragraph_node(document)?;
        let start = document.text_length();

        if cdata {
            document.text_append(raw)?;
        } else {
            decode(raw, document)?;
        }

        let text = Span { length: document.text_length() - start, offset: start };

        self.text_place(paragraph, text, document)
    }

    fn text_bytes(&mut self, bytes: &[u8], document: &mut Document) -> Result<()> {
        assert!(!bytes.is_empty());
        assert!(document.node_count() >= 1);

        if !self.in_paragraph {
            return Ok(());
        }

        let paragraph = self.paragraph_node(document)?;
        let text = document.text_append(bytes)?;

        self.text_place(paragraph, text, document)
    }

    fn text_place(&self, paragraph: u32, text: Span, document: &mut Document) -> Result<()> {
        assert!(self.in_paragraph);
        assert!(paragraph < document.node_count());
        assert!(text.end() <= document.text_length());

        let marks = if self.paragraph.style == StyleKind::Code { Marks::NONE } else { self.marks };
        let href = if self.paragraph.style == StyleKind::Code { Span::EMPTY } else { self.href };

        document.text_node_place(paragraph, TextRun { href, marks, text })
    }

    fn paragraph_node(&mut self, document: &mut Document) -> Result<u32> {
        assert!(self.in_paragraph);

        if self.paragraph.node != NODE_NONE {
            return Ok(self.paragraph.node);
        }

        let node = match self.paragraph.style {
            StyleKind::Heading(level) => self.heading_open(level, document)?,
            StyleKind::Code => self.code_open(document)?,
            StyleKind::Quote => self.quote_paragraph_open(document)?,
            StyleKind::ListParagraph | StyleKind::Normal => self.plain_paragraph_open(document)?,
        };

        self.paragraph.node = node;

        self.cell_alignment_apply(document);

        assert!(node != NODE_NONE);

        Ok(node)
    }

    fn cell_alignment_apply(&mut self, document: &mut Document) {
        assert!(self.in_paragraph);
        assert!(self.paragraph.node != NODE_NONE);

        if self.paragraph.alignment == Alignment::None {
            return;
        }

        let cell = self.frame().container;

        if let NodeKind::TableCell { alignment: Alignment::None, colspan, header } =
            document.node(cell).kind
        {
            let alignment = self.paragraph.alignment;
            document.node_mut(cell).kind = NodeKind::TableCell { alignment, colspan, header };
        }
    }

    fn blocks_close(&mut self) {
        assert!(self.frame_count >= 1);

        let frame = self.frame();
        frame.code_open = NODE_NONE;
        frame.quote_open = NODE_NONE;
        frame.list_depth = 0;

        assert!(frame.list_depth == 0);
    }

    fn heading_open(&mut self, level: u8, document: &mut Document) -> Result<u32> {
        assert!(level >= 1);
        assert!(level <= 6);

        self.blocks_close();

        let container = self.frame().container;

        document.node_append(container, NodeKind::Heading { level })
    }

    fn code_open(&mut self, document: &mut Document) -> Result<u32> {
        assert!(self.in_paragraph);

        let code_open = self.frame().code_open;

        if code_open != NODE_NONE {
            let last = document.node(code_open).child_last;

            let NodeKind::Text { text, .. } = document.node(last).kind else {
                unreachable!("a code block's only child is its text");
            };

            let mut tail = if text.end() == document.text_length() {
                text
            } else {
                document.text_copy_to_tail(text)?
            };

            document.text_extend(&mut tail, b"\n")?;

            document.node_mut(last).kind =
                NodeKind::Text { href: Span::EMPTY, marks: Marks::NONE, text: tail };

            return Ok(code_open);
        }

        self.blocks_close();

        let container = self.frame().container;

        let node =
            document.node_append(container, NodeKind::CodeBlock { language: Span::EMPTY })?;

        let text = Span { length: 0, offset: document.text_length() };

        document
            .node_append(node, NodeKind::Text { href: Span::EMPTY, marks: Marks::NONE, text })?;

        self.frame().code_open = node;

        Ok(node)
    }

    fn quote_paragraph_open(&mut self, document: &mut Document) -> Result<u32> {
        assert!(self.in_paragraph);
        assert!(self.paragraph.style == StyleKind::Quote);

        let mut quote = self.frame().quote_open;

        if quote == NODE_NONE {
            self.blocks_close();

            let container = self.frame().container;
            quote = document.node_append(container, NodeKind::BlockQuote)?;
            self.frame().quote_open = quote;
        }

        document.node_append(quote, NodeKind::Paragraph)
    }

    fn plain_paragraph_open(&mut self, document: &mut Document) -> Result<u32> {
        assert!(self.in_paragraph);
        assert!(self.paragraph.node == NODE_NONE);

        if let Some(numbering_identifier) = self.paragraph.numbering_identifier {
            if numbering_identifier != 0 {
                return self.list_paragraph_open(numbering_identifier, document);
            }
        }

        let continuation =
            self.paragraph.style == StyleKind::ListParagraph && self.frame().list_depth > 0;

        if continuation {
            let depth = self.frame().list_depth as usize;
            let item = self.frame().lists[depth - 1].item;

            return document.node_append(item, NodeKind::Paragraph);
        }

        self.blocks_close();

        let container = self.frame().container;

        document.node_append(container, NodeKind::Paragraph)
    }

    fn list_paragraph_open(
        &mut self,
        numbering_identifier: u32,
        document: &mut Document,
    ) -> Result<u32> {
        assert!(numbering_identifier != 0);
        assert!(self.in_paragraph);

        let level = self.paragraph.level.min(DOCX_LEVEL_COUNT_MAX - 1);
        let key = ListLevel { level, numbering_identifier };
        let format = numbering_level(self.tables.abstracts, self.tables.numberings, key);
        let frame = self.frame();
        frame.code_open = NODE_NONE;
        frame.quote_open = NODE_NONE;

        if frame.list_depth > level + 1 {
            frame.list_depth = level + 1;
        }

        if frame.list_depth == level + 1 {
            let top = frame.lists[level as usize];

            let same_kind = if let NodeKind::List { ordered, .. } = document.node(top.node).kind {
                ordered == format.ordered
            } else {
                false
            };

            let continues = top.numbering_identifier == numbering_identifier && same_kind;

            if !continues {
                frame.list_depth = level;
            }
        }

        for _ in frame.list_depth..=level {
            list_open_at_depth(frame, format, key, document)?;
        }

        let list = frame.lists[level as usize].node;
        let item = document.node_append(list, NodeKind::ListItem { task: TaskState::None })?;
        frame.lists[level as usize].item = item;
        self.paragraph.task_item = item;

        document.node_append(item, NodeKind::Paragraph)
    }

    fn paragraph_end(&mut self, document: &mut Document) -> Result<()> {
        assert!(document.node_count() >= 1);

        self.in_paragraph = false;
        self.in_text = false;

        let paragraph = self.paragraph.node;

        if paragraph != NODE_NONE {
            let item = self.paragraph.task_item;

            if item != NODE_NONE {
                task_glyph_strip(document, TaskParagraph { item, paragraph });
            }
        }

        if paragraph == NODE_NONE {
            if self.paragraph.rule {
                self.blocks_close();

                let container = self.frame().container;

                document.node_append(container, NodeKind::ThematicBreak)?;
            }
        }

        assert!(!self.in_paragraph);

        Ok(())
    }

    fn table_begin(&mut self, document: &mut Document) -> Result<()> {
        assert!(document.node_count() >= 1);
        assert!(self.frame_count >= 1);

        self.blocks_close();

        let container = self.frame().container;
        let table = document.node_append(container, NodeKind::Table { column_count: 0 })?;
        let mut frame = Frame::new(container);
        frame.table = table;

        self.frame_push(frame)
    }

    fn table_end(&mut self, document: &mut Document) -> Result<()> {
        assert!(document.node_count() >= 1);

        let frame = *self.frame();

        if frame.table == NODE_NONE {
            return Err(Error::DocxMalformed { offset: 0 });
        }

        self.frame_count -= 1;

        table_column_count_set(document, frame.table);

        assert!(self.frame_count >= 1);

        Ok(())
    }

    fn row_begin(&mut self, document: &mut Document) -> Result<()> {
        assert!(document.node_count() >= 1);

        let frame = self.frame();

        if frame.table == NODE_NONE {
            return Err(Error::DocxMalformed { offset: 0 });
        }

        let row = document.node_append(frame.table, NodeKind::TableRow { header: false })?;
        frame.row = row;
        frame.header_pending = false;

        assert!(frame.row != NODE_NONE);

        Ok(())
    }

    fn row_properties(&mut self, xml: &mut XMLReader<'_>) -> Result<()> {
        assert!(xml.position() > 0);
        assert!(self.frame_count >= 1);

        let mut depth: u32 = 1;

        for _ in 0..u32::MAX {
            match xml.next()? {
                XMLEvent::Finished => {
                    return Err(Error::DocxMalformed { offset: u32_from_usize(xml.position()) });
                }
                XMLEvent::Start { .. } => depth += 1,
                XMLEvent::End { .. } => {
                    depth -= 1;

                    if depth == 0 {
                        return Ok(());
                    }
                }
                XMLEvent::Empty { attributes, name: b"w:tblHeader" } => {
                    if toggle_is_on(Attributes::get(attributes, b"w:val")) {
                        self.frame().header_pending = true;
                    }
                }
                XMLEvent::Empty { .. } | XMLEvent::Text { .. } => {}
            }
        }

        Err(Error::DocxMalformed { offset: u32_from_usize(xml.position()) })
    }

    fn row_end(&mut self, document: &mut Document) -> Result<()> {
        assert!(document.node_count() >= 1);

        let frame = *self.frame();

        if frame.row == NODE_NONE {
            return Err(Error::DocxMalformed { offset: 0 });
        }

        let header =
            frame.header_pending || (frame.row_index == 0 && row_is_all_bold(document, frame.row));

        if header {
            row_header_set(document, frame.row);
        }

        let frame_current = self.frame();
        frame_current.row = NODE_NONE;
        frame_current.row_index += 1;

        assert!(frame_current.row_index >= 1);

        Ok(())
    }

    fn cell_begin(&mut self, document: &mut Document) -> Result<()> {
        assert!(document.node_count() >= 1);
        assert!(self.frame_count >= 1);

        let frame = *self.frame();

        if frame.row == NODE_NONE {
            return Err(Error::DocxMalformed { offset: 0 });
        }

        let kind = NodeKind::TableCell { alignment: Alignment::None, colspan: 1, header: false };
        let cell = document.node_append(frame.row, kind)?;
        self.cell_colspan = 1;

        self.frame_push(Frame::new(cell))
    }

    fn cell_colspan_apply(&mut self, document: &mut Document) {
        assert!(self.cell_colspan >= 1);
        assert!(self.frame_count >= 1);

        let cell = self.frame().container;

        if let NodeKind::TableCell { alignment, header, .. } = document.node(cell).kind {
            let colspan = self.cell_colspan;
            document.node_mut(cell).kind = NodeKind::TableCell { alignment, colspan, header };
        }
    }

    fn cell_properties(&mut self, xml: &mut XMLReader<'_>, document: &mut Document) -> Result<()> {
        assert!(document.node_count() >= 1);
        assert!(self.frame_count >= 1);

        let mut depth: u32 = 1;

        for _ in 0..u32::MAX {
            match xml.next()? {
                XMLEvent::Finished => {
                    return Err(Error::DocxMalformed { offset: u32_from_usize(xml.position()) });
                }
                XMLEvent::Start { .. } => depth += 1,
                XMLEvent::End { .. } => {
                    depth -= 1;

                    if depth == 0 {
                        return Ok(());
                    }
                }
                XMLEvent::Empty { attributes, name: b"w:gridSpan" } => {
                    let span = Attributes::get(attributes, b"w:val").and_then(decimal_parse_u32);
                    self.cell_colspan = span.unwrap_or(1).clamp(1, TABLE_COLSPAN_MAX);

                    self.cell_colspan_apply(document);
                }
                XMLEvent::Empty { .. } | XMLEvent::Text { .. } => {}
            }
        }

        Err(Error::DocxMalformed { offset: u32_from_usize(xml.position()) })
    }

    fn cell_end(&mut self) -> Result<()> {
        assert!(self.frame_count >= 1);

        let frame = *self.frame();

        if frame.table != NODE_NONE {
            return Err(Error::DocxMalformed { offset: 0 });
        }

        if self.frame_count < 2 {
            return Err(Error::DocxMalformed { offset: 0 });
        }

        self.frame_count -= 1;
        self.cell_colspan = 1;

        assert!(self.frame_count >= 1);

        Ok(())
    }

    fn drawing(&mut self, xml: &mut XMLReader<'_>, document: &mut Document) -> Result<()> {
        assert!(document.node_count() >= 1);
        assert!(xml.position() > 0);

        let mut depth: u32 = 1;
        let mut alt = Span::EMPTY;

        for _ in 0..u32::MAX {
            match xml.next()? {
                XMLEvent::Finished => {
                    return Err(Error::DocxMalformed { offset: u32_from_usize(xml.position()) });
                }
                XMLEvent::Start { attributes, name } => {
                    depth += 1;

                    if name == b"wp:docPr" {
                        if alt.is_empty() {
                            alt = description_decode(attributes, document)?;
                        }
                    }
                }
                XMLEvent::Empty { attributes, name } => {
                    if name == b"wp:docPr" {
                        if alt.is_empty() {
                            alt = description_decode(attributes, document)?;
                        }
                    }
                }
                XMLEvent::End { .. } => {
                    depth -= 1;

                    if depth == 0 {
                        break;
                    }
                }
                XMLEvent::Text { .. } => {}
            }
        }

        if !self.in_paragraph {
            return Ok(());
        }

        if alt.is_empty() {
            alt = document.text_append(b"image")?;
        }

        let paragraph = self.paragraph_node(document)?;

        document.node_append(paragraph, NodeKind::Image { alt, url: Span::EMPTY })?;

        Ok(())
    }
}

fn list_open_at_depth(
    frame: &mut Frame,
    format: LevelFormat,
    key: ListLevel,
    document: &mut Document,
) -> Result<()> {
    assert!(frame.list_depth <= key.level);
    assert!(key.level < DOCX_LEVEL_COUNT_MAX);
    assert!(format.start >= 1);

    let level = key.level;
    let numbering_identifier = key.numbering_identifier;
    let depth = frame.list_depth as usize;
    let parent = if depth == 0 { frame.container } else { frame.lists[depth - 1].item };
    let list_kind = NodeKind::List { ordered: format.ordered, start: format.start, tight: true };
    let node = document.node_append(parent, list_kind)?;
    frame.lists[depth] = ListState { item: NODE_NONE, node, numbering_identifier };
    frame.list_depth += 1;

    if frame.list_depth <= level {
        let item = document.node_append(node, NodeKind::ListItem { task: TaskState::None })?;
        frame.lists[depth].item = item;
    }

    assert!(frame.list_depth as usize == depth + 1);

    Ok(())
}

fn description_decode(attributes: &[u8], document: &mut Document) -> Result<Span> {
    assert!(document.node_count() >= 1);

    let raw = Attributes::get(attributes, b"descr")
        .filter(|value| !value.trim_ascii().is_empty())
        .or_else(|| Attributes::get(attributes, b"title"))
        .or_else(|| Attributes::get(attributes, b"name"))
        .unwrap_or(b"");

    let start = document.text_length();

    decode(raw, document)?;

    assert!(document.text_length() >= start);

    Ok(Span { length: document.text_length() - start, offset: start })
}

fn alignment_parse(value: Option<&[u8]>) -> Alignment {
    match value {
        Some(b"center") => Alignment::Center,
        Some(b"right" | b"end") => Alignment::Right,
        Some(b"left" | b"start") => Alignment::Left,
        _ => Alignment::None,
    }
}

fn toggle_is_on(value: Option<&[u8]>) -> bool {
    !matches!(value, Some(b"0" | b"false" | b"off"))
}

fn task_glyph_strip(document: &mut Document, task: TaskParagraph) {
    assert!(task.paragraph < document.node_count());
    assert!(task.item < document.node_count());

    let paragraph = task.paragraph;
    let item = task.item;
    let first = document.node(paragraph).child_first;

    if first == NODE_NONE {
        return;
    }

    let NodeKind::Text { href, marks, text } = document.node(first).kind else {
        return;
    };

    for (glyph, state) in TASK_GLYPHS {
        if document.span_bytes(text).starts_with(glyph) {
            let glyph_length = u32_from_usize(glyph.len());

            let stripped =
                Span { length: text.length - glyph_length, offset: text.offset + glyph_length };

            document.node_mut(first).kind = NodeKind::Text { href, marks, text: stripped };
            document.node_mut(item).kind = NodeKind::ListItem { task: state };

            return;
        }
    }
}

fn table_column_count_set(document: &mut Document, table: u32) {
    assert!(table < document.node_count());

    let row_first = document.node(table).child_first;
    let mut column_count = 0u32;

    if row_first != NODE_NONE {
        let mut cells = document.children(row_first);

        for _ in 0..document.node_count() {
            let Some(cell) = cells.next(document) else {
                break;
            };

            if let NodeKind::TableCell { colspan, .. } = document.node(cell).kind {
                assert!(colspan <= TABLE_COLSPAN_MAX);

                column_count += colspan.max(1);
            }
        }
    }

    document.node_mut(table).kind = NodeKind::Table { column_count };

    assert!(matches!(document.node(table).kind, NodeKind::Table { .. }));
}

fn row_is_all_bold(document: &Document, row: u32) -> bool {
    assert!(row < document.node_count());
    assert!(matches!(document.node(row).kind, NodeKind::TableRow { .. }));

    let mut walk = Walk::new(document, row);
    let mut any = false;

    for _ in 0..document.node_count() * 2 {
        let Some(event) = walk.next(document) else {
            break;
        };

        if let WalkEvent::Enter(index) = event {
            if let NodeKind::Text { marks, .. } = document.node(index).kind {
                if !marks.contains(Marks::STRONG) {
                    return false;
                }

                any = true;
            }
        }
    }

    any
}

fn row_header_set(document: &mut Document, row: u32) {
    assert!(row < document.node_count());

    document.node_mut(row).kind = NodeKind::TableRow { header: true };

    let mut walk = Walk::new(document, row);

    for _ in 0..document.node_count() * 2 {
        let Some(event) = walk.next(document) else {
            break;
        };

        let WalkEvent::Enter(index) = event else {
            continue;
        };

        match document.node(index).kind {
            NodeKind::TableCell { alignment, colspan, .. } => {
                document.node_mut(index).kind =
                    NodeKind::TableCell { alignment, colspan, header: true };
            }
            NodeKind::Text { href, marks, text } => {
                let kind = NodeKind::Text { href, marks: marks.without(Marks::STRONG), text };
                document.node_mut(index).kind = kind;
            }

            NodeKind::Unused
            | NodeKind::BlockQuote
            | NodeKind::CodeBlock { .. }
            | NodeKind::Document
            | NodeKind::HardBreak
            | NodeKind::Heading { .. }
            | NodeKind::HTMLBlock { .. }
            | NodeKind::HTMLInline { .. }
            | NodeKind::Image { .. }
            | NodeKind::List { .. }
            | NodeKind::ListItem { .. }
            | NodeKind::Paragraph
            | NodeKind::Table { .. }
            | NodeKind::TableRow { .. }
            | NodeKind::ThematicBreak => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{alignment_parse, toggle_is_on};
    use crate::document::Alignment;

    #[test]
    fn toggles() {
        assert!(toggle_is_on(None));
        assert!(toggle_is_on(Some(b"1")));
        assert!(toggle_is_on(Some(b"true")));
        assert!(!toggle_is_on(Some(b"0")));
        assert!(!toggle_is_on(Some(b"false")));
    }

    #[test]
    fn alignments() {
        assert_eq!(alignment_parse(Some(b"center")), Alignment::Center);
        assert_eq!(alignment_parse(Some(b"end")), Alignment::Right);
        assert_eq!(alignment_parse(Some(b"start")), Alignment::Left);
        assert_eq!(alignment_parse(Some(b"both")), Alignment::None);
        assert_eq!(alignment_parse(None), Alignment::None);
    }
}
