use crate::bytes::{decimal_parse_u32, u8_from_u32, u32_from_usize};
use crate::document::Span;
use crate::docx::{
    DOCX_ABSTRACT_COUNT_MAX,
    DOCX_LEVEL_COUNT_MAX,
    DOCX_NUMBERING_COUNT_MAX,
    DOCX_RELATIONSHIP_COUNT_MAX,
    DOCX_STYLE_COUNT_MAX,
    DocxAbstract,
    DocxNumbering,
    DocxRelationship,
    DocxStyle,
};
use crate::error::{Error, Result};
use crate::workspace::{PART_BYTES_MAX, RELATIONSHIPS_BYTES_MAX};
use crate::xml::{Attributes, XMLEvent, XMLReader};

const BASED_ON_CHAIN_MAX: u32 = 16;
const HEADING_LEVEL_MAX: u32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StyleKind {
    Code,
    Heading(u8),
    ListParagraph,
    Normal,
    Quote,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Part<'a> {
    bytes: &'a [u8],
}

impl<'a> Part<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        assert!(bytes.len() <= PART_BYTES_MAX as usize);

        Self { bytes }
    }

    pub(crate) const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    pub(crate) fn span_of(self, sub: &[u8]) -> Span {
        let base = self.bytes.as_ptr() as usize;
        let start = sub.as_ptr() as usize;

        assert!(start >= base);
        assert!(start + sub.len() <= base + self.bytes.len());

        Span { length: u32_from_usize(sub.len()), offset: u32_from_usize(start - base) }
    }

    pub(crate) fn span_bytes(self, span: Span) -> &'a [u8] {
        assert!(span.end() as usize <= self.bytes.len());

        &self.bytes[span.offset as usize..span.end() as usize]
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Element<'a> {
    pub(crate) attributes: &'a [u8],
    pub(crate) name: &'a [u8],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ListLevel {
    pub(crate) level: u32,
    pub(crate) numbering_identifier: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LevelFormat {
    pub(crate) ordered: bool,
    pub(crate) start: u32,
}

pub(crate) fn styles_parse(
    part: Part<'_>,
    styles: &mut [DocxStyle],
    count: &mut u32,
) -> Result<()> {
    assert!(styles.len() == DOCX_STYLE_COUNT_MAX as usize);
    assert!(*count == 0);

    let buffer = part.bytes();
    let mut reader = XMLReader::new(buffer);
    let mut current: Option<usize> = None;

    for _ in 0..buffer.len() {
        match reader.next()? {
            XMLEvent::Finished => break,
            XMLEvent::Start { attributes, name: b"w:style" } => {
                if *count >= DOCX_STYLE_COUNT_MAX {
                    return Err(Error::DocxTableCapacity { name: "styles" });
                }

                let identifier = Attributes::get(attributes, b"w:styleId").unwrap_or(b"");
                let index = *count as usize;

                styles[index] =
                    DocxStyle { identifier: part.span_of(identifier), ..DocxStyle::EMPTY };

                *count += 1;
                current = Some(index);
            }
            XMLEvent::Empty { attributes, name: b"w:name" } => {
                if let (Some(index), Some(value)) = (current, Attributes::get(attributes, b"w:val"))
                {
                    styles[index].name = part.span_of(value);
                }
            }
            XMLEvent::Empty { attributes, name: b"w:basedOn" } => {
                if let (Some(index), Some(value)) = (current, Attributes::get(attributes, b"w:val"))
                {
                    styles[index].based_on = part.span_of(value);
                }
            }
            XMLEvent::End { name: b"w:style" } => current = None,
            XMLEvent::Start { .. }
            | XMLEvent::Empty { .. }
            | XMLEvent::End { .. }
            | XMLEvent::Text { .. } => {}
        }
    }

    assert!(*count <= DOCX_STYLE_COUNT_MAX);

    Ok(())
}

fn style_find<'a>(
    part: Part<'_>,
    styles: &'a [DocxStyle],
    identifier: &[u8],
) -> Option<&'a DocxStyle> {
    assert!(styles.len() <= DOCX_STYLE_COUNT_MAX as usize);

    styles.iter().find(|style| part.span_bytes(style.identifier) == identifier)
}

fn contains_ignore_case<const N: usize>(haystack: &[u8], needle: &[u8; N]) -> bool {
    assert!(N >= 1);

    haystack.len() >= N
        && (0..=haystack.len() - N)
            .any(|index| haystack[index..index + N].eq_ignore_ascii_case(needle))
}

fn heading_level_from_name(name: &[u8]) -> Option<u8> {
    if name.eq_ignore_ascii_case(b"title") {
        return Some(1);
    }

    let spaced = name.len() > 8 && name[..8].eq_ignore_ascii_case(b"heading ");
    let joined = name.len() > 7 && name[..7].eq_ignore_ascii_case(b"heading");

    let rest = if spaced {
        &name[8..]
    } else if joined {
        &name[7..]
    } else {
        return None;
    };

    let level = decimal_parse_u32(rest.trim_ascii())?;
    let clamped = if (1..=HEADING_LEVEL_MAX).contains(&level) { level } else { HEADING_LEVEL_MAX };

    Some(u8_from_u32(clamped))
}

fn style_kind_single(part: Part<'_>, style: &DocxStyle) -> Option<StyleKind> {
    assert!(style.identifier.end() as usize <= part.bytes().len());

    let identifier = part.span_bytes(style.identifier);
    let name = part.span_bytes(style.name);

    if let Some(level) =
        heading_level_from_name(name).or_else(|| heading_level_from_name(identifier))
    {
        return Some(StyleKind::Heading(level));
    }

    let quote = contains_ignore_case(name, b"quot") || contains_ignore_case(identifier, b"quot");

    if quote {
        return Some(StyleKind::Quote);
    }

    let code = contains_ignore_case(name, b"code")
        || contains_ignore_case(identifier, b"code")
        || contains_ignore_case(name, b"preformatted")
        || contains_ignore_case(identifier, b"preformatted")
        || contains_ignore_case(name, b"source")
        || contains_ignore_case(identifier, b"source")
        || contains_ignore_case(name, b"verbatim")
        || contains_ignore_case(identifier, b"verbatim");

    if code {
        return Some(StyleKind::Code);
    }

    let list_paragraph = name.eq_ignore_ascii_case(b"list paragraph")
        || identifier.eq_ignore_ascii_case(b"ListParagraph");

    if list_paragraph {
        return Some(StyleKind::ListParagraph);
    }

    None
}

pub(crate) fn style_kind(part: Part<'_>, styles: &[DocxStyle], identifier: &[u8]) -> StyleKind {
    assert!(styles.len() <= DOCX_STYLE_COUNT_MAX as usize);

    let mut current = identifier;

    for _ in 0..BASED_ON_CHAIN_MAX {
        let Some(style) = style_find(part, styles, current) else {
            return StyleKind::Normal;
        };

        if let Some(kind) = style_kind_single(part, style) {
            return kind;
        }

        if style.based_on.is_empty() {
            return StyleKind::Normal;
        }

        current = part.span_bytes(style.based_on);
    }

    StyleKind::Normal
}

#[derive(Clone, Copy, Debug)]
struct NumberingCursor {
    abstract_index: Option<usize>,
    level: u32,
    numbering_index: Option<usize>,
}

pub(crate) struct NumberingTables<'a> {
    pub(crate) abstract_count: &'a mut u32,
    pub(crate) abstracts: &'a mut [DocxAbstract],
    pub(crate) numbering_count: &'a mut u32,
    pub(crate) numberings: &'a mut [DocxNumbering],
}

pub(crate) fn numbering_parse(part: Part<'_>, tables: NumberingTables<'_>) -> Result<()> {
    assert!(*tables.abstract_count == 0);
    assert!(*tables.numbering_count == 0);

    let NumberingTables { abstract_count, abstracts, numbering_count, numberings } = tables;
    let buffer = part.bytes();
    let mut reader = XMLReader::new(buffer);
    let mut cursor = NumberingCursor { abstract_index: None, level: 0, numbering_index: None };

    for _ in 0..buffer.len() {
        let element = match reader.next()? {
            XMLEvent::Finished => break,
            XMLEvent::Start { attributes, name } | XMLEvent::Empty { attributes, name } => {
                Element { attributes, name }
            }
            XMLEvent::End { name: b"w:abstractNum" } => {
                cursor.abstract_index = None;

                continue;
            }
            XMLEvent::End { name: b"w:num" } => {
                cursor.numbering_index = None;

                continue;
            }
            XMLEvent::End { .. } | XMLEvent::Text { .. } => continue,
        };

        abstract_event(&mut cursor, element, abstracts, abstract_count)?;
        instance_event(&mut cursor, element, numberings, numbering_count)?;
    }

    assert!(*abstract_count <= DOCX_ABSTRACT_COUNT_MAX);
    assert!(*numbering_count <= DOCX_NUMBERING_COUNT_MAX);

    Ok(())
}

fn abstract_event(
    cursor: &mut NumberingCursor,
    element: Element<'_>,
    abstracts: &mut [DocxAbstract],
    abstract_count: &mut u32,
) -> Result<()> {
    assert!(*abstract_count as usize <= abstracts.len());
    assert!(!element.name.is_empty());

    let value = Attributes::get(element.attributes, b"w:val");
    let level_ok = cursor.level < DOCX_LEVEL_COUNT_MAX;
    let level = cursor.level as usize;

    match element.name {
        b"w:abstractNum" => {
            if *abstract_count >= DOCX_ABSTRACT_COUNT_MAX {
                return Err(Error::DocxTableCapacity { name: "abstract numbering" });
            }

            let identifier =
                Attributes::get(element.attributes, b"w:abstractNumId").and_then(decimal_parse_u32);

            let index = *abstract_count as usize;

            assert!(index < abstracts.len());

            abstracts[index] =
                DocxAbstract { identifier: identifier.unwrap_or(u32::MAX), ..DocxAbstract::EMPTY };

            *abstract_count += 1;
            cursor.abstract_index = Some(index);
        }
        b"w:lvl" => {
            let parsed = Attributes::get(element.attributes, b"w:ilvl").and_then(decimal_parse_u32);
            cursor.level = parsed.unwrap_or(0);
        }
        b"w:start" => {
            if let (Some(index), Some(start), true) =
                (cursor.abstract_index, value.and_then(decimal_parse_u32), level_ok)
            {
                abstracts[index].levels[level].start = start;
            }
        }
        b"w:numFmt" => {
            if let (Some(index), Some(format), true) = (cursor.abstract_index, value, level_ok) {
                let ordered = format != b"bullet" && format != b"none";
                abstracts[index].levels[level].ordered = ordered;
            }
        }
        _ => {}
    }

    Ok(())
}

fn instance_event(
    cursor: &mut NumberingCursor,
    element: Element<'_>,
    numberings: &mut [DocxNumbering],
    numbering_count: &mut u32,
) -> Result<()> {
    assert!(*numbering_count as usize <= numberings.len());
    assert!(!element.name.is_empty());

    let value = Attributes::get(element.attributes, b"w:val").and_then(decimal_parse_u32);
    let level = cursor.level as usize;

    match element.name {
        b"w:num" => {
            if *numbering_count >= DOCX_NUMBERING_COUNT_MAX {
                return Err(Error::DocxTableCapacity { name: "numbering" });
            }

            let identifier =
                Attributes::get(element.attributes, b"w:numId").and_then(decimal_parse_u32);

            let index = *numbering_count as usize;

            assert!(index < numberings.len());

            numberings[index] = DocxNumbering {
                identifier: identifier.unwrap_or(u32::MAX),
                ..DocxNumbering::EMPTY
            };

            *numbering_count += 1;

            cursor.numbering_index = Some(index);
        }
        b"w:abstractNumId" => {
            if let (Some(index), Some(abstract_identifier)) = (cursor.numbering_index, value) {
                numberings[index].abstract_identifier = abstract_identifier;
            }
        }
        b"w:lvlOverride" => {
            let parsed = Attributes::get(element.attributes, b"w:ilvl").and_then(decimal_parse_u32);
            cursor.level = parsed.unwrap_or(0);
        }
        b"w:startOverride" => {
            let level_ok = cursor.level < DOCX_LEVEL_COUNT_MAX;

            if let (Some(index), Some(start), true) = (cursor.numbering_index, value, level_ok) {
                numberings[index].start_overrides[level] = start;
            }
        }
        _ => {}
    }

    Ok(())
}

pub(crate) fn numbering_level(
    abstracts: &[DocxAbstract],
    numberings: &[DocxNumbering],
    key: ListLevel,
) -> LevelFormat {
    assert!(abstracts.len() <= DOCX_ABSTRACT_COUNT_MAX as usize);
    assert!(numberings.len() <= DOCX_NUMBERING_COUNT_MAX as usize);

    let level = key.level.min(DOCX_LEVEL_COUNT_MAX - 1) as usize;

    let Some(numbering) =
        numberings.iter().find(|numbering| numbering.identifier == key.numbering_identifier)
    else {
        return LevelFormat { ordered: false, start: 1 };
    };

    let Some(abstract_numbering) =
        abstracts.iter().find(|entry| entry.identifier == numbering.abstract_identifier)
    else {
        return LevelFormat { ordered: false, start: 1 };
    };

    let definition = abstract_numbering.levels[level];
    let start_override = numbering.start_overrides[level];
    let start = if start_override == 0 { definition.start } else { start_override };

    LevelFormat { ordered: definition.ordered, start: start.max(1) }
}

pub(crate) fn relationships_parse(
    part: Part<'_>,
    relationships: &mut [DocxRelationship],
    count: &mut u32,
) -> Result<()> {
    assert!(relationships.len() == DOCX_RELATIONSHIP_COUNT_MAX as usize);
    assert!(*count == 0);

    let buffer = part.bytes();
    let mut reader = XMLReader::new(buffer);

    for _ in 0..buffer.len() {
        let attributes = match reader.next()? {
            XMLEvent::Finished => break,
            XMLEvent::Start { attributes, name: b"Relationship" }
            | XMLEvent::Empty { attributes, name: b"Relationship" } => attributes,
            XMLEvent::Start { .. }
            | XMLEvent::Empty { .. }
            | XMLEvent::End { .. }
            | XMLEvent::Text { .. } => continue,
        };

        let Some(identifier) = Attributes::get(attributes, b"Id") else {
            continue;
        };

        let Some(target) = Attributes::get(attributes, b"Target") else {
            continue;
        };

        if *count >= DOCX_RELATIONSHIP_COUNT_MAX {
            return Err(Error::DocxTableCapacity { name: "relationships" });
        }

        let index = *count as usize;

        relationships[index] =
            DocxRelationship { identifier: part.span_of(identifier), target: part.span_of(target) };

        *count += 1;
    }

    assert!(*count <= DOCX_RELATIONSHIP_COUNT_MAX);

    Ok(())
}

pub(crate) fn relationship_target<'a>(
    part: Part<'a>,
    relationships: &[DocxRelationship],
    identifier: &[u8],
) -> Option<&'a [u8]> {
    assert!(relationships.len() <= DOCX_RELATIONSHIP_COUNT_MAX as usize);

    relationships
        .iter()
        .find(|relationship| part.span_bytes(relationship.identifier) == identifier)
        .map(|relationship| part.span_bytes(relationship.target))
}

pub(crate) fn office_document_target(buffer: &[u8]) -> Option<&[u8]> {
    assert!(buffer.len() <= RELATIONSHIPS_BYTES_MAX as usize);

    let mut reader = XMLReader::new(buffer);

    for _ in 0..buffer.len() {
        let attributes = match reader.next().ok()? {
            XMLEvent::Finished => break,
            XMLEvent::Start { attributes, name: b"Relationship" }
            | XMLEvent::Empty { attributes, name: b"Relationship" } => attributes,
            XMLEvent::Start { .. }
            | XMLEvent::Empty { .. }
            | XMLEvent::End { .. }
            | XMLEvent::Text { .. } => continue,
        };

        let Some(kind) = Attributes::get(attributes, b"Type") else {
            continue;
        };

        if !kind.ends_with(b"/officeDocument") {
            continue;
        }

        let target = Attributes::get(attributes, b"Target")?;

        return Some(target.strip_prefix(b"/").unwrap_or(target));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{
        LevelFormat,
        ListLevel,
        NumberingTables,
        Part,
        StyleKind,
        numbering_level,
        numbering_parse,
        style_kind,
        styles_parse,
    };
    use crate::docx::{
        DOCX_ABSTRACT_COUNT_MAX,
        DOCX_NUMBERING_COUNT_MAX,
        DOCX_STYLE_COUNT_MAX,
        DocxAbstract,
        DocxNumbering,
        DocxStyle,
    };

    #[test]
    fn resolves_headings_through_based_on_chain() {
        let buffer = concat!(
            "<w:styles><w:style w:styleId=\"berschrift1\"><w:name w:val=\"heading 1\"/></w:style>",
            "<w:style w:styleId=\"MyStyle\"><w:name w:val=\"Fancy\"/>",
            "<w:basedOn w:val=\"berschrift1\"/></w:style>",
            "<w:style w:styleId=\"Quotations\"><w:name w:val=\"Quotations\"/></w:style>",
            "<w:style w:styleId=\"PreformattedText\"><w:name w:val=\"Preformatted Text\"/>",
            "</w:style></w:styles>",
        )
        .as_bytes();

        let part = Part::new(buffer);
        let mut styles = [DocxStyle::EMPTY; DOCX_STYLE_COUNT_MAX as usize];
        let mut count = 0u32;

        styles_parse(part, &mut styles, &mut count).unwrap();
        assert_eq!(count, 4);
        assert_eq!(style_kind(part, &styles[..4], b"MyStyle"), StyleKind::Heading(1));
        assert_eq!(style_kind(part, &styles[..4], b"Quotations"), StyleKind::Quote);
        assert_eq!(style_kind(part, &styles[..4], b"PreformattedText"), StyleKind::Code);
        assert_eq!(style_kind(part, &styles[..4], b"Missing"), StyleKind::Normal);
    }

    #[test]
    fn resolves_numbering_with_overrides() {
        let buffer = concat!(
            "<w:numbering><w:abstractNum w:abstractNumId=\"7\"><w:lvl w:ilvl=\"0\">",
            "<w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/></w:lvl><w:lvl w:ilvl=\"1\">",
            "<w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/></w:lvl></w:abstractNum>",
            "<w:num w:numId=\"3\"><w:abstractNumId w:val=\"7\"/><w:lvlOverride w:ilvl=\"0\">",
            "<w:startOverride w:val=\"5\"/></w:lvlOverride></w:num></w:numbering>",
        )
        .as_bytes();

        let mut abstracts = [DocxAbstract::EMPTY; DOCX_ABSTRACT_COUNT_MAX as usize];
        let mut numberings = [DocxNumbering::EMPTY; DOCX_NUMBERING_COUNT_MAX as usize];
        let mut abstract_count = 0u32;
        let mut numbering_count = 0u32;

        numbering_parse(
            Part::new(buffer),
            NumberingTables {
                abstract_count: &mut abstract_count,
                abstracts: &mut abstracts,
                numbering_count: &mut numbering_count,
                numberings: &mut numberings,
            },
        )
        .unwrap();

        let first = ListLevel { level: 0, numbering_identifier: 3 };
        let second = ListLevel { level: 1, numbering_identifier: 3 };
        let missing = ListLevel { level: 0, numbering_identifier: 99 };

        assert_eq!(
            numbering_level(&abstracts[..1], &numberings[..1], first),
            LevelFormat { ordered: true, start: 5 }
        );

        assert_eq!(
            numbering_level(&abstracts[..1], &numberings[..1], second),
            LevelFormat { ordered: false, start: 1 }
        );

        assert_eq!(
            numbering_level(&abstracts[..1], &numberings[..1], missing),
            LevelFormat { ordered: false, start: 1 }
        );
    }
}
