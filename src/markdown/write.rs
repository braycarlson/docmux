use crate::bytes::{
    DECIMAL_DIGIT_COUNT_MAX,
    Sink,
    decimal_format_u32,
    u16_from_usize,
    u32_from_usize,
};
use crate::document::{
    Alignment,
    DEPTH_MAX,
    Document,
    HEADING_LEVEL_MAX,
    Marks,
    NODE_NONE,
    NODE_ROOT,
    Node,
    NodeKind,
    Span,
    TaskState,
    Walk,
    WalkEvent,
};
use crate::error::Result;
use crate::markdown::block::COLUMN_COUNT_MAX;

const LIST_MARKER_BYTES_MAX: usize = DECIMAL_DIGIT_COUNT_MAX + 6;
const OPEN_MARK_COUNT_MAX: u32 = 4;
const PREFIX_BYTES_MAX: usize = 256;
const SEGMENT_COUNT_MAX: u32 = DEPTH_MAX as u32 + 1;
const WHITESPACE_PENDING_MAX: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenMark {
    Emphasis,
    Link(Span),
    Strikethrough,
    Strong,
}

impl OpenMark {
    fn wanted(self, marks: Marks, href: Span) -> bool {
        assert!(!marks.contains(Marks::CODE));

        match self {
            Self::Emphasis => marks.contains(Marks::EMPHASIS),
            Self::Link(open_href) => open_href == href,
            Self::Strikethrough => marks.contains(Marks::STRIKETHROUGH),
            Self::Strong => marks.contains(Marks::STRONG),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PrefixSegment {
    length: u16,
    marker_pending: bool,
    settled_length: u16,
    start: u16,
}

impl PrefixSegment {
    const EMPTY: Self = Self { length: 0, marker_pending: false, settled_length: 0, start: 0 };
}

#[derive(Clone, Copy, Debug)]
struct TextContext {
    heading: bool,
    in_table: bool,
}

impl TextContext {
    const CELL: Self = Self { heading: false, in_table: true };
    const HEADING: Self = Self { heading: true, in_table: false };
    const PARAGRAPH: Self = Self { heading: false, in_table: false };
}

#[derive(Debug)]
struct Writer<'a, 'b, 'c> {
    block_count: [u32; SEGMENT_COUNT_MAX as usize],
    block_start: bool,
    document: &'c Document,
    list_bullet: [u8; SEGMENT_COUNT_MAX as usize],
    list_index: [u32; SEGMENT_COUNT_MAX as usize],
    open_count: u32,
    open_marks: [OpenMark; OPEN_MARK_COUNT_MAX as usize],
    prefix: [u8; PREFIX_BYTES_MAX],
    prefix_length: u32,
    segment_count: u32,
    segments: [PrefixSegment; SEGMENT_COUNT_MAX as usize],
    sink: &'b mut Sink<'a>,
    whitespace_pending: u32,
}

pub fn write(document: &Document, output: &mut [u8]) -> Result<u32> {
    assert!(document.node_count() >= 1);
    assert!(document.node(NODE_ROOT).kind == NodeKind::Document);

    let mut sink = Sink::new(output);

    let mut writer = Writer {
        block_count: [0; SEGMENT_COUNT_MAX as usize],
        block_start: true,
        document,
        list_bullet: [0; SEGMENT_COUNT_MAX as usize],
        list_index: [0; SEGMENT_COUNT_MAX as usize],
        open_count: 0,
        open_marks: [OpenMark::Strong; OPEN_MARK_COUNT_MAX as usize],
        prefix: [0; PREFIX_BYTES_MAX],
        prefix_length: 0,
        segment_count: 0,
        segments: [PrefixSegment::EMPTY; SEGMENT_COUNT_MAX as usize],
        sink: &mut sink,
        whitespace_pending: 0,
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

    assert!(writer.segment_count == 0);
    assert!(writer.open_count == 0);

    Ok(sink.length())
}

impl Writer<'_, '_, '_> {
    fn enter(&mut self, index: u32, walk: &mut Walk) -> Result<()> {
        assert!(index < self.document.node_count());

        let node = *self.document.node(index);
        let depth = usize::from(node.depth);
        let separated = self.block_separated_is(node);

        if separated {
            self.block_separate(depth)?;
        }

        self.block_count[depth + 1] = 0;

        match node.kind {
            NodeKind::Document | NodeKind::TableRow { .. } | NodeKind::TableCell { .. } => {}
            NodeKind::Paragraph => {
                self.line_begin()?;
                self.inline_children(index)?;
                self.sink.write(b"\n")?;
                walk.skip_children(self.document);
            }
            NodeKind::Heading { level } => {
                assert!(level >= 1);
                assert!(level <= HEADING_LEVEL_MAX);

                self.line_begin()?;
                self.sink.write_repeat(b'#', usize::from(level))?;
                self.sink.write(b" ")?;
                self.inline_children(index)?;
                self.sink.write(b"\n")?;
                walk.skip_children(self.document);
            }
            NodeKind::BlockQuote => self.segment_push(b"> ", false, 2),
            NodeKind::List { ordered, start, .. } => self.list_enter(depth, ordered, start),
            NodeKind::ListItem { task } => self.list_item_enter(index, task),
            NodeKind::CodeBlock { language } => {
                self.code_block(index, language)?;
                walk.skip_children(self.document);
            }
            NodeKind::ThematicBreak => {
                self.line_begin()?;
                self.sink.write(if self.segment_count > 0 { b"___\n" } else { b"---\n" })?;
            }
            NodeKind::Table { .. } => {
                self.table(index)?;
                walk.skip_children(self.document);
            }
            NodeKind::HTMLBlock { text } => {
                self.code_lines(self.document.span_bytes(text))?;
            }

            NodeKind::Text { .. }
            | NodeKind::HardBreak
            | NodeKind::HTMLInline { .. }
            | NodeKind::Image { .. } => {
                unreachable!("inline nodes are rendered by their block parent");
            }
            NodeKind::Unused => unreachable!("unused node reached by walk"),
        }

        if separated {
            self.block_count[depth] += 1;
        }

        Ok(())
    }

    fn leave(&mut self, index: u32) -> Result<()> {
        assert!(index < self.document.node_count());

        let node = *self.document.node(index);

        match node.kind {
            NodeKind::BlockQuote | NodeKind::ListItem { .. } => {
                if node.child_first == NODE_NONE {
                    self.line_blank()?;
                }

                self.segment_pop();
            }

            NodeKind::Document
            | NodeKind::Paragraph
            | NodeKind::Heading { .. }
            | NodeKind::List { .. }
            | NodeKind::CodeBlock { .. }
            | NodeKind::ThematicBreak
            | NodeKind::Table { .. }
            | NodeKind::TableRow { .. }
            | NodeKind::TableCell { .. }
            | NodeKind::HTMLBlock { .. }
            | NodeKind::Text { .. }
            | NodeKind::HardBreak
            | NodeKind::HTMLInline { .. }
            | NodeKind::Image { .. } => {}
            NodeKind::Unused => unreachable!("unused node reached by walk"),
        }

        Ok(())
    }

    fn block_separated_is(&self, node: Node) -> bool {
        assert!(node.parent < self.document.node_count());

        let parent_kind = self.document.node(node.parent).kind;

        let in_tight_item = matches!(parent_kind, NodeKind::ListItem { .. })
            && self.list_is_tight(self.document.node(node.parent).parent);

        let tight_item =
            matches!(node.kind, NodeKind::ListItem { .. }) && self.list_is_tight(node.parent);

        node.kind.is_block()
            && !in_tight_item
            && !tight_item
            && !matches!(node.kind, NodeKind::TableRow { .. } | NodeKind::TableCell { .. })
    }

    fn list_is_tight(&self, list: u32) -> bool {
        assert!(list < self.document.node_count());
        assert!(matches!(self.document.node(list).kind, NodeKind::List { .. }));

        matches!(self.document.node(list).kind, NodeKind::List { tight: true, .. })
    }

    fn list_bullet_pick(&self, depth: usize, ordered: bool) -> u8 {
        assert!(depth < SEGMENT_COUNT_MAX as usize);

        let previous = self.list_bullet[depth];

        assert!(matches!(previous, 0 | b'-' | b'*' | b'.' | b')'));

        if ordered {
            if previous == b'.' { b')' } else { b'.' }
        } else if previous == b'-' {
            b'*'
        } else {
            b'-'
        }
    }

    fn list_enter(&mut self, depth: usize, ordered: bool, start: u32) {
        assert!(depth < SEGMENT_COUNT_MAX as usize);

        self.list_index[depth] = start;
        self.list_bullet[depth] = self.list_bullet_pick(depth, ordered);

        assert!(self.list_bullet[depth] != 0);
    }

    fn list_item_enter(&mut self, index: u32, task: TaskState) {
        assert!(index < self.document.node_count());

        let node = *self.document.node(index);
        let list_depth = usize::from(self.document.node(node.parent).depth);

        let ordered =
            matches!(self.document.node(node.parent).kind, NodeKind::List { ordered: true, .. });

        let bullet = self.list_bullet[list_depth];

        assert!(bullet != 0);

        let mut marker = [0u8; LIST_MARKER_BYTES_MAX];
        let mut marker_length = 0usize;

        if ordered {
            let mut digits = [0u8; DECIMAL_DIGIT_COUNT_MAX];
            let digits_length = decimal_format_u32(self.list_index[list_depth], &mut digits);

            marker[..digits_length].copy_from_slice(&digits[..digits_length]);
            marker_length = digits_length;
            self.list_index[list_depth] += 1;
        }

        marker[marker_length] = bullet;
        marker[marker_length + 1] = b' ';
        marker_length += 2;

        let task_marker: &[u8] = match task {
            TaskState::None => b"",
            TaskState::Unchecked => b"[ ] ",
            TaskState::Checked => b"[x] ",
        };

        marker[marker_length..marker_length + task_marker.len()].copy_from_slice(task_marker);
        marker_length += task_marker.len();

        self.segment_push(&marker[..marker_length], true, marker_length - task_marker.len());
    }

    fn block_separate(&mut self, depth: usize) -> Result<()> {
        assert!(depth < SEGMENT_COUNT_MAX as usize);
        assert!(self.open_count == 0);

        if self.block_count[depth] > 0 {
            self.line_blank()?;
        }

        Ok(())
    }

    fn segment_push(&mut self, bytes: &[u8], marker_pending: bool, settled_length: usize) {
        assert!(self.segment_count < SEGMENT_COUNT_MAX);
        assert!(self.prefix_length as usize + bytes.len() <= PREFIX_BYTES_MAX);
        assert!(settled_length <= bytes.len());

        let start = self.prefix_length as usize;

        self.prefix[start..start + bytes.len()].copy_from_slice(bytes);
        self.prefix_length += u32_from_usize(bytes.len());

        self.segments[self.segment_count as usize] = PrefixSegment {
            length: u16_from_usize(bytes.len()),
            marker_pending,
            settled_length: u16_from_usize(settled_length),
            start: u16_from_usize(start),
        };

        self.segment_count += 1;

        assert!(self.prefix_length as usize <= PREFIX_BYTES_MAX);
    }

    fn segment_pop(&mut self) {
        assert!(self.segment_count > 0);

        self.segment_count -= 1;
        self.prefix_length = u32::from(self.segments[self.segment_count as usize].start);

        assert!(self.prefix_length as usize <= PREFIX_BYTES_MAX);
    }

    fn line_begin(&mut self) -> Result<()> {
        assert!(self.prefix_length as usize <= PREFIX_BYTES_MAX);
        assert!(self.segment_count <= SEGMENT_COUNT_MAX);

        self.sink.write(&self.prefix[..self.prefix_length as usize])?;
        self.markers_settle();

        Ok(())
    }

    fn line_blank(&mut self) -> Result<()> {
        assert!(self.prefix_length as usize <= PREFIX_BYTES_MAX);
        assert!(self.open_count == 0);

        let end = self.prefix[..self.prefix_length as usize].trim_ascii_end().len();

        self.sink.write(&self.prefix[..end])?;
        self.sink.write(b"\n")?;
        self.markers_settle();

        Ok(())
    }

    fn markers_settle(&mut self) {
        assert!(self.segment_count <= SEGMENT_COUNT_MAX);

        for index in 0..self.segment_count as usize {
            let segment = self.segments[index];

            if !segment.marker_pending {
                continue;
            }

            let start = usize::from(segment.start);
            let length = usize::from(segment.length);
            let settled = usize::from(segment.settled_length);

            self.prefix[start..start + settled].fill(b' ');
            self.prefix.copy_within(start + length..self.prefix_length as usize, start + settled);
            self.prefix_length -= u32_from_usize(length - settled);
            self.segments[index].length = u16_from_usize(settled);
            self.segments[index].marker_pending = false;

            for later in &mut self.segments[index + 1..self.segment_count as usize] {
                later.start -= u16_from_usize(length - settled);
            }
        }

        assert!(
            self.segments[..self.segment_count as usize]
                .iter()
                .all(|segment| !segment.marker_pending)
        );
    }

    fn fence_length(&self, index: u32) -> usize {
        assert!(index < self.document.node_count());

        let mut fence_length = 3usize;
        let mut children = self.document.children(index);

        for _ in 0..self.document.node_count() {
            let Some(child) = children.next(self.document) else {
                break;
            };

            if let NodeKind::Text { text, .. } = self.document.node(child).kind {
                let run = backtick_run_longest(self.document.span_bytes(text));
                fence_length = fence_length.max(run + 1);
            }
        }

        assert!(fence_length >= 3);

        fence_length
    }

    fn code_block(&mut self, index: u32, language: Span) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(language.end() <= self.document.text_length());

        let fence_length = self.fence_length(index);

        self.line_begin()?;
        self.sink.write_repeat(b'`', fence_length)?;
        self.sink.write(self.document.span_bytes(language))?;
        self.sink.write(b"\n")?;

        let document = self.document;
        let mut children = document.children(index);

        for _ in 0..document.node_count() {
            let Some(child) = children.next(document) else {
                break;
            };

            if let NodeKind::Text { text, .. } = document.node(child).kind {
                self.code_lines(document.span_bytes(text))?;
            }
        }

        self.line_begin()?;
        self.sink.write_repeat(b'`', fence_length)?;

        self.sink.write(b"\n")
    }

    fn code_lines(&mut self, text: &[u8]) -> Result<()> {
        assert!(self.open_count == 0);

        let before = self.sink.length();
        let mut lines = text.split(|&byte| byte == b'\n');

        for _ in 0..=text.len() {
            let Some(line) = lines.next() else {
                break;
            };

            self.line_begin()?;
            self.sink.write(line)?;
            self.sink.write(b"\n")?;
        }

        assert!(self.sink.length() > before);

        Ok(())
    }

    fn table(&mut self, index: u32) -> Result<()> {
        assert!(index < self.document.node_count());

        let document = self.document;

        let NodeKind::Table { column_count: declared } = document.node(index).kind else {
            unreachable!("table writer called on a non-table node");
        };

        let mut rows = document.children(index);
        let mut alignments = [Alignment::None; COLUMN_COUNT_MAX as usize];
        let column_count = declared.clamp(1, COLUMN_COUNT_MAX);

        assert!(column_count >= 1);

        for row_index in 0..document.node_count() {
            let Some(row) = rows.next(document) else {
                break;
            };

            let column = self.table_row(row, &mut alignments[..column_count as usize])?;

            for _ in column..column_count {
                self.sink.write(b" |")?;
            }

            self.sink.write(b"\n")?;

            if row_index == 0 {
                self.table_delimiter_row(&alignments[..column_count as usize])?;
            }
        }

        Ok(())
    }

    fn table_row(&mut self, row: u32, alignments: &mut [Alignment]) -> Result<u32> {
        assert!(row < self.document.node_count());
        assert!(alignments.len() <= COLUMN_COUNT_MAX as usize);

        let document = self.document;
        let mut cells = document.children(row);
        let mut column = 0_u32;

        self.line_begin()?;
        self.sink.write(b"|")?;

        for _ in 0..alignments.len() {
            let Some(cell) = cells.next(document) else {
                break;
            };

            let NodeKind::TableCell { alignment, colspan, .. } = document.node(cell).kind else {
                continue;
            };

            if let Some(slot) = alignments.get_mut(column as usize) {
                *slot = alignment;
            }

            self.sink.write(b" ")?;
            self.cell_content(cell)?;
            self.sink.write(b" |")?;

            let span = colspan.max(1);

            for _ in 1..span {
                self.sink.write(b" |")?;
            }

            column += span;
        }

        Ok(column)
    }

    fn table_delimiter_row(&mut self, alignments: &[Alignment]) -> Result<()> {
        assert!(!alignments.is_empty());
        assert!(alignments.len() <= COLUMN_COUNT_MAX as usize);

        self.line_begin()?;
        self.sink.write(b"|")?;

        for &alignment in alignments {
            let cell: &[u8] = match alignment {
                Alignment::None => b" --- |",
                Alignment::Left => b" :-- |",
                Alignment::Center => b" :-: |",
                Alignment::Right => b" --: |",
            };

            self.sink.write(cell)?;
        }

        self.sink.write(b"\n")
    }

    fn cell_content(&mut self, cell: u32) -> Result<()> {
        assert!(cell < self.document.node_count());
        assert!(matches!(self.document.node(cell).kind, NodeKind::TableCell { .. }));

        let document = self.document;
        let mut walk = Walk::new(document, cell);
        let mut block_index = 0u32;
        self.block_start = true;

        for _ in 0..document.node_count() * 2 {
            let Some(event) = walk.next(document) else {
                break;
            };

            let WalkEvent::Enter(child) = event else {
                continue;
            };

            if child == cell {
                continue;
            }

            let kind = document.node(child).kind;

            if kind.is_inline() {
                self.inline_node(child, TextContext::CELL)?;
            } else if kind.is_block() {
                if block_index > 0 {
                    self.marks_close_all()?;
                    self.whitespace_pending = 1;
                }

                block_index += 1;
            }
        }

        self.inline_finish()
    }

    fn inline_children(&mut self, parent: u32) -> Result<()> {
        assert!(parent < self.document.node_count());
        assert!(self.open_count == 0);

        let document = self.document;

        let context = if matches!(document.node(parent).kind, NodeKind::Heading { .. }) {
            TextContext::HEADING
        } else {
            TextContext::PARAGRAPH
        };

        let mut children = document.children(parent);
        self.block_start = true;

        for _ in 0..document.node_count() {
            let Some(child) = children.next(document) else {
                break;
            };

            self.inline_node(child, context)?;
        }

        self.inline_finish()
    }

    fn inline_node(&mut self, index: u32, context: TextContext) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(self.document.node(index).kind.is_inline());

        let document = self.document;

        match document.node(index).kind {
            NodeKind::Text { href, marks, text } => {
                self.text(document.span_bytes(text), marks, href, context)
            }
            NodeKind::HardBreak => {
                self.whitespace_pending = 0;
                self.block_start = false;

                if context.in_table {
                    self.sink.write(b" ")
                } else {
                    self.sink.write(b"\\\n")?;

                    self.line_begin()
                }
            }
            NodeKind::HTMLInline { text } => {
                self.whitespace_flush()?;
                self.block_start = false;

                self.sink.write(document.span_bytes(text))
            }
            NodeKind::Image { alt, url } => {
                self.marks_close_all()?;
                self.whitespace_flush()?;
                self.block_start = false;

                self.sink.write(b"![")?;
                text_escape(document.span_bytes(alt), self.sink, TextContext::CELL)?;
                self.sink.write(b"](")?;
                url_escape(document.span_bytes(url), self.sink)?;

                self.sink.write(b")")
            }

            NodeKind::Document
            | NodeKind::Paragraph
            | NodeKind::Heading { .. }
            | NodeKind::BlockQuote
            | NodeKind::List { .. }
            | NodeKind::ListItem { .. }
            | NodeKind::CodeBlock { .. }
            | NodeKind::ThematicBreak
            | NodeKind::Table { .. }
            | NodeKind::TableRow { .. }
            | NodeKind::TableCell { .. }
            | NodeKind::HTMLBlock { .. } => Ok(()),
            NodeKind::Unused => unreachable!("unused node reached by walk"),
        }
    }

    fn inline_finish(&mut self) -> Result<()> {
        self.marks_close_all()?;
        self.whitespace_pending = 0;

        assert!(self.open_count == 0);
        assert!(self.whitespace_pending == 0);

        Ok(())
    }

    fn text(&mut self, text: &[u8], marks: Marks, href: Span, context: TextContext) -> Result<()> {
        assert!(href.end() <= self.document.text_length());
        assert!(self.open_count <= OPEN_MARK_COUNT_MAX);

        if marks.contains(Marks::CODE) {
            return self.code_text(text, marks, href, context.in_table);
        }

        if text.is_empty() {
            if href != Span::EMPTY {
                self.marks_transition(marks, href)?;
                self.block_start = false;
            }

            return Ok(());
        }

        let leading = text.iter().take_while(|&&byte| byte.is_ascii_whitespace()).count();
        let trailing = text.iter().rev().take_while(|&&byte| byte.is_ascii_whitespace()).count();

        if leading == text.len() {
            self.whitespace_note(leading);

            return Ok(());
        }

        let core = &text[leading..text.len() - trailing];

        self.whitespace_note(leading);
        self.marks_transition(marks, href)?;
        self.whitespace_flush()?;
        text_escape(core, self.sink, context)?;
        self.block_start = false;
        self.whitespace_pending = u32::try_from(trailing).unwrap_or(WHITESPACE_PENDING_MAX);
        self.whitespace_pending = self.whitespace_pending.min(WHITESPACE_PENDING_MAX);

        Ok(())
    }

    fn code_text(&mut self, text: &[u8], marks: Marks, href: Span, in_table: bool) -> Result<()> {
        assert!(marks.contains(Marks::CODE));
        assert!(href.end() <= self.document.text_length());

        self.marks_transition(marks.without(Marks::CODE), href)?;
        self.whitespace_flush()?;
        code_span_write(text, self.sink, in_table)?;
        self.block_start = false;
        self.whitespace_pending = 0;

        Ok(())
    }

    fn whitespace_note(&mut self, count: usize) {
        assert!(self.whitespace_pending <= WHITESPACE_PENDING_MAX);

        if self.block_start {
            return;
        }

        let added = u32::try_from(count).unwrap_or(WHITESPACE_PENDING_MAX);
        self.whitespace_pending = self.whitespace_pending.saturating_add(added);
        self.whitespace_pending = self.whitespace_pending.min(WHITESPACE_PENDING_MAX);

        assert!(self.whitespace_pending <= WHITESPACE_PENDING_MAX);
    }

    fn marks_transition(&mut self, marks: Marks, href: Span) -> Result<()> {
        assert!(self.open_count <= OPEN_MARK_COUNT_MAX);
        assert!(href.end() <= self.document.text_length());

        let wanted = marks.without(Marks::CODE);

        let keep = self.open_marks[..self.open_count as usize]
            .iter()
            .position(|mark| !mark.wanted(wanted, href))
            .map_or(self.open_count, u32_from_usize);

        for index in (keep..self.open_count).rev() {
            match self.open_marks[index as usize] {
                OpenMark::Strikethrough => self.sink.write(b"~~")?,
                OpenMark::Emphasis => self.sink.write(b"*")?,
                OpenMark::Strong => self.sink.write(b"**")?,
                OpenMark::Link(open_href) => {
                    self.sink.write(b"](")?;
                    url_escape(self.document.span_bytes(open_href), self.sink)?;
                    self.sink.write(b")")?;
                }
            }
        }

        self.open_count = keep;

        let missing: [Option<OpenMark>; 4] = [
            if href == Span::EMPTY { None } else { Some(OpenMark::Link(href)) },
            wanted.contains(Marks::STRONG).then_some(OpenMark::Strong),
            wanted.contains(Marks::EMPHASIS).then_some(OpenMark::Emphasis),
            wanted.contains(Marks::STRIKETHROUGH).then_some(OpenMark::Strikethrough),
        ];

        let any_missing = missing
            .iter()
            .flatten()
            .any(|mark| !self.open_marks[..self.open_count as usize].contains(mark));

        if any_missing {
            self.whitespace_flush()?;
        }

        for mark in missing.into_iter().flatten() {
            if self.open_marks[..self.open_count as usize].contains(&mark) {
                continue;
            }

            match mark {
                OpenMark::Link(_) => self.sink.write(b"[")?,
                OpenMark::Strong => self.sink.write(b"**")?,
                OpenMark::Emphasis => self.sink.write(b"*")?,
                OpenMark::Strikethrough => self.sink.write(b"~~")?,
            }

            self.open_marks[self.open_count as usize] = mark;
            self.open_count += 1;
        }

        assert!(self.open_count <= OPEN_MARK_COUNT_MAX);

        Ok(())
    }

    fn marks_close_all(&mut self) -> Result<()> {
        assert!(self.open_count <= OPEN_MARK_COUNT_MAX);

        if self.open_count == 0 {
            return Ok(());
        }

        self.marks_transition(Marks::NONE, Span::EMPTY)?;

        assert!(self.open_count == 0);

        Ok(())
    }

    fn whitespace_flush(&mut self) -> Result<()> {
        if self.whitespace_pending > 0 {
            self.sink.write(b" ")?;
            self.whitespace_pending = 0;
        }

        assert!(self.whitespace_pending == 0);

        Ok(())
    }
}

fn backtick_run_longest(text: &[u8]) -> usize {
    let mut longest = 0usize;
    let mut run = 0usize;

    for &byte in text {
        if byte == b'`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }

    assert!(longest <= text.len());
    assert!(run <= longest);

    longest
}

const fn code_edge_is(byte: u8) -> bool {
    byte == b'`' || byte == b' '
}

fn code_span_write(code: &[u8], sink: &mut Sink<'_>, in_table: bool) -> Result<()> {
    let fence_length = backtick_run_longest(code) + 1;
    let all_spaces = code.iter().all(|&byte| byte == b' ');

    let pad = !all_spaces
        && (code.first().is_some_and(|&byte| code_edge_is(byte))
            || code.last().is_some_and(|&byte| code_edge_is(byte)));

    assert!(fence_length >= 1);

    sink.write_repeat(b'`', fence_length)?;

    let opened = sink.length();

    if pad {
        sink.write(b" ")?;
    }

    for &byte in code {
        let pipe_in_table = byte == b'|' && in_table;

        if pipe_in_table {
            sink.write(b"\\|")?;
        } else if byte == b'\n' {
            sink.write(b" ")?;
        } else {
            sink.write_byte(byte)?;
        }
    }

    if pad {
        sink.write(b" ")?;
    }

    sink.write_repeat(b'`', fence_length)?;

    assert!(sink.length() >= opened + u32_from_usize(code.len() + fence_length));

    Ok(())
}

fn text_escape(text: &[u8], sink: &mut Sink<'_>, context: TextContext) -> Result<()> {
    let before = sink.length();

    for (index, &byte) in text.iter().enumerate() {
        let escape = match byte {
            b'\\' | b'*' | b'`' | b'[' | b']' | b'~' => true,
            b'_' => index == 0 || !text[index - 1].is_ascii_alphanumeric(),
            b'<' => text.get(index + 1).is_some_and(|&next| {
                next.is_ascii_alphabetic() || matches!(next, b'/' | b'!' | b'?')
            }),
            b'|' => context.in_table,
            b'&' => entity_like(&text[index..]),
            b'!' => text.get(index + 1) == Some(&b'['),
            b'#' => {
                index == 0 || (context.heading && text[index..].iter().all(|&rest| rest == b'#'))
            }
            b'>' | b'+' | b'-' | b'=' => index == 0,
            b'.' | b')' => index > 0 && text[..index].iter().all(u8::is_ascii_digit),
            _ => false,
        };

        if escape {
            sink.write_byte(b'\\')?;
        }

        if byte == b'\n' {
            sink.write_byte(b' ')?;
        } else {
            sink.write_byte(byte)?;
        }
    }

    assert!(sink.length() >= before + u32_from_usize(text.len()));

    Ok(())
}

fn entity_like(rest: &[u8]) -> bool {
    assert!(rest.first() == Some(&b'&'));

    let Some(semicolon) = rest.iter().take(12).position(|&byte| byte == b';') else {
        return false;
    };

    assert!(semicolon >= 1);

    semicolon > 1
        && rest[1..semicolon].iter().all(|&byte| byte.is_ascii_alphanumeric() || byte == b'#')
}

fn url_escape(url: &[u8], sink: &mut Sink<'_>) -> Result<()> {
    let before = sink.length();

    for &byte in url {
        match byte {
            b' ' => sink.write(b"%20")?,
            b'(' => sink.write(b"%28")?,
            b')' => sink.write(b"%29")?,
            b'<' => sink.write(b"%3C")?,
            b'>' => sink.write(b"%3E")?,
            b'\\' => sink.write(b"%5C")?,
            b'\n' | b'\r' | b'\t' => {}
            _ => sink.write_byte(byte)?,
        }
    }

    assert!(sink.length() >= before);

    Ok(())
}
