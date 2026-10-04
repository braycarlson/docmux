use crate::bytes::{
    DECIMAL_DIGIT_COUNT_MAX,
    decimal_format_u32,
    range_from_u32,
    u32_from_usize,
    utf8_validate,
};
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
};
use crate::error::{Error, Result};
use crate::json::{ArrayItems, Object, string_decode, string_raw, u32_parse};
use core::ops::Range;

pub const INPUT_BYTES_MAX: u32 = 64 << 20;
const DATE_BYTES: usize = 10;
const FRAME_COUNT_MAX: u32 = DEPTH_MAX as u32 * 2 + 2;
const MILLISECONDS_PER_DAY: u64 = 86_400_000;
const TIMESTAMP_DIGIT_COUNT_MAX: u32 = 20;
const YEAR_MAX: u64 = 9999;

#[derive(Clone, Debug)]
struct Frame {
    code: bool,
    items: ArrayItems,
    node: u32,
}

impl Frame {
    const EMPTY: Self = Self { code: false, items: ArrayItems::EMPTY, node: NODE_NONE };
}

#[derive(Debug)]
struct Reader<'a> {
    frame_count: u32,
    frames: [Frame; FRAME_COUNT_MAX as usize],
    source: &'a [u8],
}

pub fn read(source: &[u8], document: &mut Document) -> Result<()> {
    if source.len() > INPUT_BYTES_MAX as usize {
        return Err(Error::InputCapacity { capacity_bytes: INPUT_BYTES_MAX });
    }

    utf8_validate(source)?;
    document.reset();

    assert!(document.node_count() == 1);

    let root = 0..u32_from_usize(source.len());

    let kind = Object { range: root.clone(), source }
        .field(b"type")?
        .ok_or(Error::ADFMalformed { offset: 0 })?;

    if string_raw(source, kind)? != b"doc" {
        return Err(Error::ADFMalformed { offset: 0 });
    }

    let mut reader =
        Reader { frame_count: 0, frames: [Frame::EMPTY; FRAME_COUNT_MAX as usize], source };

    if let Some(content) = (Object { range: root, source }).field(b"content")? {
        reader.frame_push(content, NODE_ROOT, false)?;
    }

    for _ in 0..=source.len() {
        if reader.frame_count == 0 {
            break;
        }

        let top = (reader.frame_count - 1) as usize;

        let Some(item) = reader.frames[top].items.next(source)? else {
            reader.frame_pop(document);

            continue;
        };

        let node = reader.frames[top].node;
        let code = reader.frames[top].code;

        reader.item(item, node, code, document)?;
    }

    assert!(reader.frame_count == 0);

    Ok(())
}

impl Reader<'_> {
    fn frame_push(&mut self, content: Range<u32>, node: u32, code: bool) -> Result<()> {
        assert!(content.end as usize <= self.source.len());
        assert!(node < NODE_COUNT_MAX);
        assert!(self.frame_count <= FRAME_COUNT_MAX);

        if self.frame_count >= FRAME_COUNT_MAX {
            return Err(Error::DepthExceeded { depth_max: DEPTH_MAX });
        }

        let items = ArrayItems::new(self.source, content)?;
        self.frames[self.frame_count as usize] = Frame { code, items, node };
        self.frame_count += 1;

        Ok(())
    }

    fn frame_pop(&mut self, document: &mut Document) {
        assert!(self.frame_count > 0);
        assert!(document.node_count() >= 1);

        let top = (self.frame_count - 1) as usize;
        let node = self.frames[top].node;
        self.frames[top] = Frame::EMPTY;
        self.frame_count -= 1;

        match document.node(node).kind {
            NodeKind::Table { .. } => table_finish(document, node),
            NodeKind::TableRow { .. } => row_finish(document, node),
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
            | NodeKind::TableCell { .. }
            | NodeKind::Text { .. }
            | NodeKind::ThematicBreak => {}
        }
    }

    fn malformed(&self, item: &Range<u32>) -> Error {
        assert!(item.start as usize <= self.source.len());

        Error::ADFMalformed { offset: item.start }
    }

    fn field(&self, object: &Range<u32>, key: &[u8]) -> Result<Option<Range<u32>>> {
        assert!(object.end as usize <= self.source.len());

        Object { range: object.clone(), source: self.source }.field(key)
    }

    fn attribute(&self, item: &Range<u32>, key: &[u8]) -> Result<Option<Range<u32>>> {
        assert!(!key.is_empty());

        self.field(item, b"attrs")?.map_or(Ok(None), |attributes| self.field(&attributes, key))
    }

    fn attribute_u32(&self, item: &Range<u32>, key: &[u8], default: u32) -> Result<u32> {
        assert!(!key.is_empty());

        self.attribute(item, key)?
            .map_or(Ok(default), |range| u32_parse(self.source, self.unquoted(range)))
    }

    fn unquoted(&self, range: Range<u32>) -> Range<u32> {
        assert!(range.end as usize <= self.source.len());

        let quoted = self.source[range_from_u32(&range)].first() == Some(&b'"');
        let inner = if quoted { range.start + 1..range.end - 1 } else { range };

        assert!(inner.end as usize <= self.source.len());

        inner
    }

    fn attribute_text(
        &self,
        item: &Range<u32>,
        key: &[u8],
        document: &mut Document,
    ) -> Result<Span> {
        assert!(!key.is_empty());

        match self.attribute(item, key)? {
            Some(range) if self.source[range_from_u32(&range)].first() == Some(&b'"') => {
                let mut span = Span { length: 0, offset: document.text_length() };

                string_decode_into(self.source, range, document, &mut span)?;

                Ok(span)
            }
            _ => Ok(Span::EMPTY),
        }
    }

    fn item(
        &mut self,
        item: Range<u32>,
        parent: u32,
        code: bool,
        document: &mut Document,
    ) -> Result<()> {
        assert!(item.end as usize <= self.source.len());
        assert!(parent < document.node_count());

        let kind_range = self.field(&item, b"type")?.ok_or_else(|| self.malformed(&item))?;
        let kind = string_raw(self.source, kind_range)?;
        let content = self.field(&item, b"content")?;

        match kind {
            b"paragraph" => self.block(content, parent, NodeKind::Paragraph, document),
            b"heading" => {
                let level_raw = self.attribute_u32(&item, b"level", 1)?;

                let level = u8::try_from(level_raw.clamp(1, u32::from(HEADING_LEVEL_MAX)))
                    .unwrap_or(HEADING_LEVEL_MAX);

                self.block(content, parent, NodeKind::Heading { level }, document)
            }
            b"blockquote" | b"panel" | b"expand" | b"nestedExpand" => {
                self.quote(item, content, parent, document)
            }
            b"bulletList" | b"taskList" | b"decisionList" => {
                self.list(item, content, parent, false, document)
            }
            b"orderedList" => self.list(item, content, parent, true, document),
            b"listItem" => {
                self.block(content, parent, NodeKind::ListItem { task: TaskState::None }, document)
            }
            b"taskItem" | b"decisionItem" => self.task_item(item, content, parent, document),
            b"codeBlock" => self.code_block(item, content, parent, document),
            b"rule" => {
                document.node_append(parent, NodeKind::ThematicBreak)?;

                Ok(())
            }
            b"table" => self.block(content, parent, NodeKind::Table { column_count: 0 }, document),
            b"tableRow" => {
                self.block(content, parent, NodeKind::TableRow { header: false }, document)
            }
            b"tableHeader" | b"tableCell" => {
                self.cell(item, content, parent, kind == b"tableHeader", document)
            }
            b"text" => self.text(item, parent, code, document),
            b"hardBreak" => {
                inline_append(parent, NodeKind::HardBreak, document)?;

                Ok(())
            }
            b"mention" | b"emoji" | b"status" | b"placeholder" => {
                self.inline_attribute_text(item, parent, document)
            }
            b"date" => self.date(item, parent, document),
            b"inlineCard" | b"blockCard" | b"embedCard" => self.card(item, parent, document),
            b"media" | b"mediaInline" => self.media(item, parent, document),
            _ => self.transparent(content, parent, code),
        }
    }

    fn list(
        &mut self,
        item: Range<u32>,
        content: Option<Range<u32>>,
        parent: u32,
        ordered: bool,
        document: &mut Document,
    ) -> Result<()> {
        assert!(item.end as usize <= self.source.len());
        assert!(content.as_ref().is_none_or(|content| content.end as usize <= self.source.len()));
        assert!(parent < document.node_count());

        let start = if ordered { self.attribute_u32(&item, b"order", 1)? } else { 1 };

        self.block(content, parent, NodeKind::List { ordered, start, tight: true }, document)
    }

    fn block(
        &mut self,
        content: Option<Range<u32>>,
        parent: u32,
        kind: NodeKind,
        document: &mut Document,
    ) -> Result<()> {
        assert!(parent < document.node_count());
        assert!(content.as_ref().is_none_or(|content| content.end as usize <= self.source.len()));
        assert!(kind.is_block());

        let node = document.node_append(parent, kind)?;

        content.map_or(Ok(()), |content| self.frame_push(content, node, false))
    }

    fn transparent(&mut self, content: Option<Range<u32>>, parent: u32, code: bool) -> Result<()> {
        assert!(parent < NODE_COUNT_MAX);
        assert!(content.as_ref().is_none_or(|content| content.end as usize <= self.source.len()));
        assert!(self.frame_count <= FRAME_COUNT_MAX);

        content.map_or(Ok(()), |content| self.frame_push(content, parent, code))
    }

    fn quote(
        &mut self,
        item: Range<u32>,
        content: Option<Range<u32>>,
        parent: u32,
        document: &mut Document,
    ) -> Result<()> {
        assert!(item.end as usize <= self.source.len());
        assert!(content.as_ref().is_none_or(|content| content.end as usize <= self.source.len()));
        assert!(parent < document.node_count());

        let node = document.node_append(parent, NodeKind::BlockQuote)?;
        let title = self.attribute_text(&item, b"title", document)?;

        if !title.is_empty() {
            let paragraph = document.node_append(node, NodeKind::Paragraph)?;
            let kind = NodeKind::Text { href: Span::EMPTY, marks: Marks::STRONG, text: title };

            document.node_append(paragraph, kind)?;
        }

        content.map_or(Ok(()), |content| self.frame_push(content, node, false))
    }

    fn task_item(
        &mut self,
        item: Range<u32>,
        content: Option<Range<u32>>,
        parent: u32,
        document: &mut Document,
    ) -> Result<()> {
        assert!(item.end as usize <= self.source.len());
        assert!(content.as_ref().is_none_or(|content| content.end as usize <= self.source.len()));
        assert!(parent < document.node_count());

        let task = match self.attribute(&item, b"state")? {
            Some(state) => {
                if string_raw(self.source, state)? == b"DONE" {
                    TaskState::Checked
                } else {
                    TaskState::Unchecked
                }
            }
            None => TaskState::None,
        };

        let node = document.node_append(parent, NodeKind::ListItem { task })?;
        let paragraph = document.node_append(node, NodeKind::Paragraph)?;

        content.map_or(Ok(()), |content| self.frame_push(content, paragraph, false))
    }

    fn code_block(
        &mut self,
        item: Range<u32>,
        content: Option<Range<u32>>,
        parent: u32,
        document: &mut Document,
    ) -> Result<()> {
        assert!(item.end as usize <= self.source.len());
        assert!(content.as_ref().is_none_or(|content| content.end as usize <= self.source.len()));
        assert!(parent < document.node_count());

        let language = self.attribute_text(&item, b"language", document)?;
        let node = document.node_append(parent, NodeKind::CodeBlock { language })?;

        content.map_or(Ok(()), |content| self.frame_push(content, node, true))
    }

    fn cell(
        &mut self,
        item: Range<u32>,
        content: Option<Range<u32>>,
        parent: u32,
        header: bool,
        document: &mut Document,
    ) -> Result<()> {
        assert!(item.end as usize <= self.source.len());
        assert!(content.as_ref().is_none_or(|content| content.end as usize <= self.source.len()));
        assert!(parent < document.node_count());

        let colspan = self.attribute_u32(&item, b"colspan", 1)?.clamp(1, TABLE_COLSPAN_MAX);
        let kind = NodeKind::TableCell { alignment: Alignment::None, colspan, header };

        self.block(content, parent, kind, document)
    }

    fn text(
        &self,
        item: Range<u32>,
        parent: u32,
        code: bool,
        document: &mut Document,
    ) -> Result<()> {
        assert!(item.end as usize <= self.source.len());
        assert!(parent < document.node_count());

        let (marks, href) =
            if code { (Marks::NONE, Span::EMPTY) } else { self.marks(&item, document)? };

        let text_range = self.field(&item, b"text")?.ok_or_else(|| self.malformed(&item))?;
        let mut text = Span { length: 0, offset: document.text_length() };

        string_decode_into(self.source, text_range, document, &mut text)?;

        if text.is_empty() {
            return Ok(());
        }

        text_place(parent, TextRun { href, marks, text }, document)
    }

    fn marks(&self, item: &Range<u32>, document: &mut Document) -> Result<(Marks, Span)> {
        assert!(item.end as usize <= self.source.len());

        let mut marks = Marks::NONE;
        let mut href = Span::EMPTY;

        let Some(array) = self.field(item, b"marks")? else {
            return Ok((marks, href));
        };

        let mark_count_max = array.len();
        let mut items = ArrayItems::new(self.source, array)?;

        for _ in 0..mark_count_max {
            let Some(mark) = items.next(self.source)? else {
                break;
            };

            let kind = self.field(&mark, b"type")?.ok_or_else(|| self.malformed(&mark))?;

            match string_raw(self.source, kind)? {
                b"strong" => marks = marks.union(Marks::STRONG),
                b"em" => marks = marks.union(Marks::EMPHASIS),
                b"strike" => marks = marks.union(Marks::STRIKETHROUGH),
                b"code" => marks = marks.union(Marks::CODE),
                b"link" => href = self.attribute_text(&mark, b"href", document)?,
                _ => {}
            }
        }

        assert!(href.end() <= document.text_length());

        Ok((marks, href))
    }

    fn inline_attribute_text(
        &self,
        item: Range<u32>,
        parent: u32,
        document: &mut Document,
    ) -> Result<()> {
        assert!(item.end as usize <= self.source.len());
        assert!(parent < document.node_count());

        let mut text = self.attribute_text(&item, b"text", document)?;

        if text.is_empty() {
            text = self.attribute_text(&item, b"shortName", document)?;
        }

        if text.is_empty() {
            return Ok(());
        }

        text_place(parent, TextRun { href: Span::EMPTY, marks: Marks::NONE, text }, document)
    }

    fn date(&self, item: Range<u32>, parent: u32, document: &mut Document) -> Result<()> {
        assert!(item.end as usize <= self.source.len());
        assert!(parent < document.node_count());

        let Some(timestamp) = self.attribute(&item, b"timestamp")? else {
            return Ok(());
        };

        let raw = &self.source[range_from_u32(&timestamp)];
        let digits = if raw.first() == Some(&b'"') { &raw[1..raw.len() - 1] } else { raw };
        let mut milliseconds: u64 = 0;

        for &digit in digits.iter().take(TIMESTAMP_DIGIT_COUNT_MAX as usize) {
            if !digit.is_ascii_digit() {
                return Ok(());
            }

            milliseconds = milliseconds.saturating_mul(10).saturating_add(u64::from(digit - b'0'));
        }

        let mut formatted = [0u8; DATE_BYTES];

        date_format(milliseconds / MILLISECONDS_PER_DAY, &mut formatted);

        let text = document.text_append(&formatted)?;

        text_place(parent, TextRun { href: Span::EMPTY, marks: Marks::NONE, text }, document)
    }

    fn card(&self, item: Range<u32>, parent: u32, document: &mut Document) -> Result<()> {
        assert!(item.end as usize <= self.source.len());
        assert!(parent < document.node_count());

        let url = self.attribute_text(&item, b"url", document)?;

        if url.is_empty() {
            return Ok(());
        }

        text_place(parent, TextRun { href: url, marks: Marks::NONE, text: url }, document)
    }

    fn media(&self, item: Range<u32>, parent: u32, document: &mut Document) -> Result<()> {
        assert!(item.end as usize <= self.source.len());
        assert!(parent < document.node_count());

        let mut alt = self.attribute_text(&item, b"alt", document)?;
        let mut url = self.attribute_text(&item, b"url", document)?;

        if url.is_empty() {
            url = self.attribute_text(&item, b"id", document)?;
        }

        if alt.is_empty() {
            alt = document.text_append(b"media")?;
        }

        inline_append(parent, NodeKind::Image { alt, url }, document)?;

        Ok(())
    }
}

fn inline_append(parent: u32, kind: NodeKind, document: &mut Document) -> Result<u32> {
    assert!(parent < document.node_count());
    assert!(kind.is_inline());

    let container = inline_container(parent, document)?;

    document.node_append(container, kind)
}

fn inline_container(parent: u32, document: &mut Document) -> Result<u32> {
    assert!(parent < document.node_count());

    let parent_kind = document.node(parent).kind;

    let inline_ok = matches!(
        parent_kind,
        NodeKind::Paragraph | NodeKind::Heading { .. } | NodeKind::CodeBlock { .. }
    );

    if inline_ok {
        return Ok(parent);
    }

    let last = document.node(parent).child_last;

    if last != NODE_NONE {
        if document.node(last).kind == NodeKind::Paragraph {
            return Ok(last);
        }
    }

    document.node_append(parent, NodeKind::Paragraph)
}

fn text_place(parent: u32, run: TextRun, document: &mut Document) -> Result<()> {
    assert!(parent < document.node_count());
    assert!(run.text.end() <= document.text_length());

    let container = inline_container(parent, document)?;

    document.text_node_place(container, run)
}

fn string_decode_into(
    source: &[u8],
    range: Range<u32>,
    document: &mut Document,
    span: &mut Span,
) -> Result<()> {
    assert!(range.end as usize <= source.len());
    assert!(span.end() == document.text_length());

    string_decode(source, range, document)?;
    span.length = document.text_length() - span.offset;

    assert!(span.end() == document.text_length());

    Ok(())
}

fn table_finish(document: &mut Document, table: u32) {
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
}

fn row_finish(document: &mut Document, row: u32) {
    assert!(row < document.node_count());

    let mut cells = document.children(row);
    let mut any = false;
    let mut all_header = true;

    for _ in 0..document.node_count() {
        let Some(cell) = cells.next(document) else {
            break;
        };

        if let NodeKind::TableCell { header, .. } = document.node(cell).kind {
            any = true;
            all_header = all_header && header;
        }
    }

    document.node_mut(row).kind = NodeKind::TableRow { header: any && all_header };
}

fn digit_pair_write(value: u64, out: &mut [u8]) {
    assert!(value < 100);
    assert!(out.len() == 2);

    let Ok(narrow) = u8::try_from(value) else { unreachable!("{value} is below 100") };
    out[0] = b'0' + narrow.div_euclid(10);
    out[1] = b'0' + narrow % 10;
}

fn date_format(days: u64, out: &mut [u8; DATE_BYTES]) {
    assert!(days <= u64::MAX.div_euclid(MILLISECONDS_PER_DAY));

    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted % 146_097;

    let year_of_era = (day_of_era - day_of_era.div_euclid(1460) + day_of_era.div_euclid(36_524)
        - day_of_era.div_euclid(146_096))
    .div_euclid(365);

    assert!(year_of_era < 400);

    let day_of_year =
        day_of_era - (365 * year_of_era + year_of_era.div_euclid(4) - year_of_era.div_euclid(100));

    let month_index = (5 * day_of_year + 2).div_euclid(153);
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = (year_of_era + era * 400 + u64::from(month <= 2)).min(YEAR_MAX);

    assert!(month >= 1);
    assert!(month <= 12);

    let Ok(year_narrow) = u32::try_from(year) else { unreachable!("{year} is clamped") };
    let mut digits = [0u8; DECIMAL_DIGIT_COUNT_MAX];
    let year_length = decimal_format_u32(year_narrow, &mut digits);

    out.fill(b'0');
    out[4 - year_length..4].copy_from_slice(&digits[..year_length]);
    out[4] = b'-';

    digit_pair_write(month, &mut out[5..7]);
    out[7] = b'-';

    let day = day_of_year - (153 * month_index + 2).div_euclid(5) + 1;

    digit_pair_write(day, &mut out[8..10]);

    assert!(day >= 1);
    assert!(day <= 31);
}

#[cfg(test)]
mod tests {
    use super::{MILLISECONDS_PER_DAY, date_format};

    #[test]
    fn formats_dates() {
        let mut out = [0u8; 10];

        date_format(0, &mut out);
        assert_eq!(&out, b"1970-01-01");
        date_format(19_723, &mut out);
        assert_eq!(&out, b"2024-01-01");
        date_format(20_454, &mut out);
        assert_eq!(&out, b"2026-01-01");
        date_format(u64::MAX.div_euclid(MILLISECONDS_PER_DAY), &mut out);
        assert!(out.starts_with(b"9999-"));
    }
}
