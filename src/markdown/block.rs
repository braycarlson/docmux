use crate::bytes::{decimal_parse_u32, u8_from_usize, u32_from_usize};
use crate::document::{
    Alignment,
    DEPTH_MAX,
    Document,
    Marks,
    NODE_NONE,
    NODE_ROOT,
    NodeKind,
    Span,
    TaskState,
};
use crate::error::{Error, Result};
use crate::markdown::Options;
use crate::markdown::inline;
use crate::workspace::{
    DEFINITION_COUNT_MAX,
    LinkDefinition,
    PART_BYTES_MAX,
    PENDING_INLINE_COUNT_MAX,
    PendingInline,
    Workspace,
};
use core::ops::Range;

pub(crate) const COLUMN_COUNT_MAX: u32 = 64;
const CODE_INDENT: u32 = 4;
const _: () = assert!(COLUMN_COUNT_MAX >= 1);
const HTML_BLOCK_KIND_BLANK_TERMINATED_FIRST: u8 = 6;
const LIST_START_DIGIT_COUNT_MAX: usize = 9;
const TAB_STOP: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContainerKind {
    BlockQuote,
    List { bullet: u8, ordered: bool },
    ListItem { blank_pending: bool, closed_by_blank: bool, content_indent: u32 },
}

#[derive(Clone, Copy, Debug)]
struct Container {
    kind: ContainerKind,
    node: u32,
}

impl Container {
    const EMPTY: Self = Self { kind: ContainerKind::BlockQuote, node: NODE_ROOT };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leaf {
    FencedCode,
    HTMLBlock,
    IndentedCode,
    None,
    Paragraph,
    Table,
}

const HTML_BLOCK_TAGS: [&[u8]; 62] = [
    b"address",
    b"article",
    b"aside",
    b"base",
    b"basefont",
    b"blockquote",
    b"body",
    b"caption",
    b"center",
    b"col",
    b"colgroup",
    b"dd",
    b"details",
    b"dialog",
    b"dir",
    b"div",
    b"dl",
    b"dt",
    b"fieldset",
    b"figcaption",
    b"figure",
    b"footer",
    b"form",
    b"frame",
    b"frameset",
    b"h1",
    b"h2",
    b"h3",
    b"h4",
    b"h5",
    b"h6",
    b"head",
    b"header",
    b"hr",
    b"html",
    b"iframe",
    b"legend",
    b"li",
    b"link",
    b"main",
    b"menu",
    b"menuitem",
    b"nav",
    b"noframes",
    b"ol",
    b"optgroup",
    b"option",
    b"p",
    b"param",
    b"search",
    b"section",
    b"summary",
    b"table",
    b"tbody",
    b"td",
    b"tfoot",
    b"th",
    b"thead",
    b"title",
    b"tr",
    b"track",
    b"ul",
];
const HTML_RAW_TEXT_TAGS: [&[u8]; 4] = [b"pre", b"script", b"style", b"textarea"];

#[derive(Clone, Copy, Debug)]
struct ListMarker {
    bullet: u8,
    content_indent: u32,
    indent: u32,
    marker_length: usize,
    ordered: bool,
    start: u32,
}

#[derive(Clone, Copy, Debug)]
struct Fence {
    character: u8,
    length: usize,
}

#[derive(Clone, Copy, Debug)]
struct DefinitionBytes<'a> {
    destination: &'a [u8],
    label: &'a [u8],
}

#[derive(Clone, Debug)]
struct Definition {
    consumed: u32,
    destination: Range<usize>,
    label: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
struct Cursor<'a> {
    column: u32,
    line: &'a [u8],
    partial: u32,
    position: usize,
}

impl<'a> Cursor<'a> {
    const fn new(line: &'a [u8]) -> Self {
        assert!(line.len() <= PART_BYTES_MAX as usize);

        Self { column: 0, line, partial: 0, position: 0 }
    }

    fn tab_remaining(&self) -> u32 {
        assert!(self.partial < TAB_STOP);

        if self.partial > 0 { TAB_STOP - self.column % TAB_STOP } else { 0 }
    }

    fn indent(&self) -> u32 {
        assert!(self.position <= self.line.len());

        let mut column = self.column + self.tab_remaining();
        let start = self.position + usize::from(self.partial > 0);

        for &byte in &self.line[start.min(self.line.len())..] {
            match byte {
                b' ' => column += 1,
                b'\t' => column += TAB_STOP - column % TAB_STOP,
                _ => break,
            }
        }

        let indent = column - self.column;

        assert!(self.column + indent == column);

        indent
    }

    fn indent_consume(&mut self, columns: u32) {
        assert!(self.position <= self.line.len());

        let target = self.column + columns;

        for _ in 0..=self.line.len() {
            if self.column >= target {
                break;
            }

            if self.position >= self.line.len() {
                break;
            }

            match self.line[self.position] {
                b' ' => {
                    self.column += 1;
                    self.position += 1;
                }
                b'\t' => {
                    let tab_end = self.column + (TAB_STOP - self.column % TAB_STOP);

                    if tab_end <= target {
                        self.column = tab_end;
                        self.position += 1;
                        self.partial = 0;
                    } else {
                        self.partial += target - self.column;
                        self.column = target;
                    }
                }
                _ => break,
            }
        }

        assert!(self.column <= target);
    }

    fn advance(&mut self, bytes: usize) {
        assert!(self.partial == 0);
        assert!(self.position + bytes <= self.line.len());

        self.position += bytes;
        self.column += u32_from_usize(bytes);

        assert!(self.position <= self.line.len());
    }

    fn is_blank(&self) -> bool {
        assert!(self.position <= self.line.len());

        self.rest().iter().all(|&byte| byte == b' ' || byte == b'\t')
    }

    fn rest(&self) -> &'a [u8] {
        assert!(self.position <= self.line.len());

        &self.line[(self.position + usize::from(self.partial > 0)).min(self.line.len())..]
    }

    fn rest_after_indent(&self) -> &'a [u8] {
        let rest = self.rest();
        let skipped = rest.iter().take_while(|&&byte| byte == b' ' || byte == b'\t').count();

        assert!(skipped <= rest.len());

        &rest[skipped..]
    }
}

#[derive(Debug)]
pub(crate) struct BlockParser {
    alignments: [Alignment; COLUMN_COUNT_MAX as usize],
    code_text: Span,
    column_count: u32,
    container_count: u32,
    containers: [Container; DEPTH_MAX as usize],
    fence_character: u8,
    fence_indent: u32,
    fence_length: usize,
    html_kind: u8,
    leaf: Leaf,
    leaf_node: u32,
    options: Options,
    paragraph_length: usize,
    paragraph_line_count: u32,
    paragraph_start: usize,
}

impl BlockParser {
    pub(crate) const fn new(options: Options) -> Self {
        Self {
            alignments: [Alignment::None; COLUMN_COUNT_MAX as usize],
            code_text: Span::EMPTY,
            column_count: 0,
            container_count: 0,
            containers: [Container::EMPTY; DEPTH_MAX as usize],
            fence_character: 0,
            fence_indent: 0,
            fence_length: 0,
            html_kind: 0,
            leaf: Leaf::None,
            leaf_node: NODE_ROOT,
            options,
            paragraph_length: 0,
            paragraph_line_count: 0,
            paragraph_start: 0,
        }
    }

    pub(crate) fn finish(
        &mut self,
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<()> {
        assert!(self.container_count as usize <= self.containers.len());
        assert!(document.node_count() >= 1);

        self.containers_close_from(0, false, workspace, document)?;

        assert!(self.container_count == 0);
        assert!(self.leaf == Leaf::None);

        let Workspace {
            definition_count,
            definitions,
            inline_tokens,
            part,
            pending_inline_count,
            pending_inlines,
            ..
        } = workspace;

        let definitions = &definitions[..*definition_count as usize];
        let options = self.options;

        for pending in &pending_inlines[..*pending_inline_count as usize] {
            let text = &part[pending.text.offset as usize..pending.text.end() as usize];

            inline::parse(text, options, inline_tokens, definitions, document, pending.node)?;
        }

        Ok(())
    }

    pub(crate) fn line_process(
        &mut self,
        line: &[u8],
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<()> {
        assert!(line.len() <= PART_BYTES_MAX as usize);
        assert!(document.node_count() >= 1);

        let mut cursor = Cursor::new(line);
        let mut matched = self.containers_match(&mut cursor);

        if matched == self.container_count {
            if self.leaf == Leaf::FencedCode {
                return self.fenced_line(&mut cursor, document);
            }

            if self.leaf == Leaf::HTMLBlock {
                return self.html_line(&cursor, document);
            }
        }

        let mut opened = false;

        for _ in 0..DEPTH_MAX {
            if !self.container_open_from_line(&mut cursor, matched, opened, workspace, document)? {
                break;
            }

            matched = self.container_count;
            opened = true;
        }

        if !opened {
            if matched < self.container_count {
                if self.lazy_continuation_is(&cursor) {
                    return self.paragraph_append(cursor.rest_after_indent(), workspace);
                }

                self.containers_close_from(matched, false, workspace, document)?;
            }
        }

        if opened {
            if cursor.is_blank() {
                return Ok(());
            }
        }

        self.list_dangling_pop();

        self.leaf_line(&mut cursor, workspace, document)
    }

    fn container_open_from_line(
        &mut self,
        cursor: &mut Cursor<'_>,
        matched: u32,
        opened: bool,
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<bool> {
        assert!(matched <= self.container_count);
        assert!(document.node_count() >= 1);

        if cursor.indent() >= CODE_INDENT {
            return Ok(false);
        }

        let paragraph_open = self.leaf == Leaf::Paragraph && matched == self.container_count;
        let rest = cursor.rest_after_indent();

        if thematic_break_is(rest) {
            return Ok(false);
        }

        if rest.starts_with(b">") {
            self.containers_close_from(matched, false, workspace, document)?;
            cursor.indent_consume(CODE_INDENT - 1);
            cursor.advance(1);
            cursor.indent_consume(1);
            self.container_open(ContainerKind::BlockQuote, document)?;

            return Ok(true);
        }

        let Some(marker) = list_marker_parse(cursor, paragraph_open && !opened) else {
            return Ok(false);
        };

        let list_kind = ContainerKind::List { bullet: marker.bullet, ordered: marker.ordered };
        let continues = matched >= 1 && self.containers[matched as usize - 1].kind == list_kind;

        self.containers_close_from(matched, continues, workspace, document)?;
        self.list_item_open(cursor, marker, document)?;

        Ok(true)
    }

    fn containers_match(&self, cursor: &mut Cursor<'_>) -> u32 {
        assert!(self.container_count as usize <= self.containers.len());

        let mut matched = 0u32;

        for container in &self.containers[..self.container_count as usize] {
            match container.kind {
                ContainerKind::BlockQuote => {
                    let marker = cursor.indent() < CODE_INDENT
                        && cursor.rest_after_indent().starts_with(b">");

                    if marker {
                        cursor.indent_consume(CODE_INDENT - 1);
                        cursor.advance(1);
                        cursor.indent_consume(1);
                    } else {
                        break;
                    }
                }
                ContainerKind::List { .. } => {}
                ContainerKind::ListItem { closed_by_blank, content_indent, .. } => {
                    let continues = !closed_by_blank
                        && (cursor.is_blank() || cursor.indent() >= content_indent);

                    if continues {
                        cursor.indent_consume(content_indent);
                    } else {
                        break;
                    }
                }
            }

            matched += 1;
        }

        assert!(matched <= self.container_count);

        matched
    }

    fn lazy_continuation_is(&self, cursor: &Cursor<'_>) -> bool {
        assert!(self.container_count as usize <= self.containers.len());

        if self.leaf != Leaf::Paragraph {
            return false;
        }

        if cursor.is_blank() {
            return false;
        }

        if cursor.indent() >= CODE_INDENT {
            return true;
        }

        let rest = cursor.rest_after_indent();

        !(atx_heading_level(rest).is_some()
            || fence_open_parse(rest).is_some()
            || thematic_break_is(rest))
    }

    const fn list_dangling_pop(&mut self) {
        assert!(self.container_count as usize <= self.containers.len());

        let Some(top) = self.container_count.checked_sub(1) else {
            return;
        };

        if let ContainerKind::List { .. } = self.containers[top as usize].kind {
            self.container_count = top;
        }

        assert!(self.container_count <= top + 1);
    }

    fn parent_current(&self) -> u32 {
        assert!(self.container_count as usize <= self.containers.len());

        if self.container_count == 0 {
            NODE_ROOT
        } else {
            self.containers[self.container_count as usize - 1].node
        }
    }

    fn list_mark_loose(&self, item_index: u32, document: &mut Document) {
        assert!(item_index < self.container_count);

        let container = self.containers[item_index as usize];

        let ContainerKind::ListItem { blank_pending, .. } = container.kind else {
            return;
        };

        if !blank_pending {
            return;
        }

        let item = container.node;
        let list = document.node(item).parent;

        assert!(list != NODE_NONE);

        if let NodeKind::List { ordered, start, .. } = document.node(list).kind {
            document.node_mut(list).kind = NodeKind::List { ordered, start, tight: false };
        }
    }

    fn blank_pending_set(&mut self, pending: bool) {
        assert!(self.container_count as usize <= self.containers.len());

        for container in &mut self.containers[..self.container_count as usize] {
            if let ContainerKind::ListItem { closed_by_blank, content_indent, .. } = container.kind
            {
                container.kind = ContainerKind::ListItem {
                    blank_pending: pending,
                    closed_by_blank,
                    content_indent,
                };
            }
        }
    }

    fn block_open_in_item(&mut self, document: &mut Document) {
        assert!(document.node_count() >= 1);

        if let Some(top) = self.container_count.checked_sub(1) {
            self.list_mark_loose(top, document);
        }

        self.blank_pending_set(false);
    }

    fn container_open(&mut self, kind: ContainerKind, document: &mut Document) -> Result<()> {
        assert!(self.leaf == Leaf::None);
        assert!(document.node_count() >= 1);

        self.block_open_in_item(document);

        if self.container_count as usize >= self.containers.len() {
            return Err(Error::DepthExceeded { depth_max: DEPTH_MAX });
        }

        let node_kind = match kind {
            ContainerKind::BlockQuote => NodeKind::BlockQuote,
            ContainerKind::List { ordered, .. } => {
                NodeKind::List { ordered, start: 1, tight: true }
            }
            ContainerKind::ListItem { .. } => NodeKind::ListItem { task: TaskState::None },
        };

        let node = document.node_append(self.parent_current(), node_kind)?;
        self.containers[self.container_count as usize] = Container { kind, node };
        self.container_count += 1;

        assert!(self.container_count as usize <= self.containers.len());

        Ok(())
    }

    fn list_item_open(
        &mut self,
        cursor: &mut Cursor<'_>,
        marker: ListMarker,
        document: &mut Document,
    ) -> Result<()> {
        assert!(marker.content_indent >= marker.indent + u32_from_usize(marker.marker_length));
        assert!(document.node_count() >= 1);

        let list_kind = ContainerKind::List { bullet: marker.bullet, ordered: marker.ordered };

        let top =
            self.container_count.checked_sub(1).map(|index| self.containers[index as usize].kind);

        if let Some(ContainerKind::List { .. }) = top {
            if top != Some(list_kind) {
                self.container_count -= 1;
            }
        }

        let top_after =
            self.container_count.checked_sub(1).map(|index| self.containers[index as usize].kind);

        if top_after != Some(list_kind) {
            self.container_open(list_kind, document)?;

            let list_node = self.parent_current();

            document.node_mut(list_node).kind =
                NodeKind::List { ordered: marker.ordered, start: marker.start, tight: true };
        }

        cursor.indent_consume(marker.indent);
        cursor.advance(marker.marker_length);

        cursor.indent_consume(
            marker.content_indent - marker.indent - u32_from_usize(marker.marker_length),
        );

        let item_kind = ContainerKind::ListItem {
            blank_pending: false,
            closed_by_blank: false,
            content_indent: marker.content_indent,
        };

        self.container_open(item_kind, document)?;

        let task = if self.options.contains(Options::TASK_ITEMS) {
            task_marker_parse(cursor.rest())
        } else {
            TaskState::None
        };

        if task != TaskState::None {
            cursor.advance(3);
            cursor.indent_consume(1);

            let item_node = self.parent_current();
            document.node_mut(item_node).kind = NodeKind::ListItem { task };
        }

        Ok(())
    }

    fn containers_close_from(
        &mut self,
        index: u32,
        list_keep: bool,
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<()> {
        assert!(index <= self.container_count);
        assert!(document.node_count() >= 1);

        self.leaf_close(workspace, document)?;

        if list_keep {
            if index < self.container_count {
                self.list_mark_loose(index, document);
            }
        }

        self.container_count = index;

        if !list_keep {
            self.list_dangling_pop();
        }

        assert!(self.container_count <= index);

        Ok(())
    }

    fn leaf_close(&mut self, workspace: &mut Workspace, document: &mut Document) -> Result<()> {
        assert!(document.node_count() >= 1);

        match self.leaf {
            Leaf::None | Leaf::Table => {}
            Leaf::Paragraph => self.paragraph_close(workspace, document)?,
            Leaf::FencedCode | Leaf::IndentedCode => self.code_close(document)?,
            Leaf::HTMLBlock => self.html_close(document),
        }

        self.leaf = Leaf::None;

        assert!(self.leaf == Leaf::None);

        Ok(())
    }

    fn leaf_line(
        &mut self,
        cursor: &mut Cursor<'_>,
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<()> {
        assert!(self.leaf != Leaf::FencedCode);
        assert!(document.node_count() >= 1);

        if cursor.is_blank() {
            return self.blank_line(document, workspace);
        }

        let indent = cursor.indent();

        if indent >= CODE_INDENT {
            return self.indented_line(cursor, workspace, document);
        }

        let raw_line = *cursor;

        cursor.indent_consume(CODE_INDENT - 1);

        let rest = cursor.rest();

        if self.leaf == Leaf::Table {
            let block_start = atx_heading_level(rest).is_some()
                || fence_open_parse(rest).is_some()
                || thematic_break_is(rest);

            if !block_start {
                return self.table_row_append(rest, workspace, document);
            }
        }

        if matches!(self.leaf, Leaf::IndentedCode | Leaf::Table) {
            self.leaf_close(workspace, document)?;
        }

        self.block_open(rest, &raw_line, indent, workspace, document)
    }

    fn block_open(
        &mut self,
        rest: &[u8],
        raw_line: &Cursor<'_>,
        indent: u32,
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<()> {
        assert!(indent < CODE_INDENT);
        assert!(self.leaf == Leaf::None || self.leaf == Leaf::Paragraph);

        if let Some(level) = atx_heading_level(rest) {
            self.leaf_close(workspace, document)?;

            return self.heading_create(rest, level, workspace, document);
        }

        if let Some(fence) = fence_open_parse(rest) {
            self.leaf_close(workspace, document)?;

            return self.fence_open(rest, fence, indent, document);
        }

        if self.leaf == Leaf::Paragraph {
            if let Some(level) = setext_level(rest) {
                return self.paragraph_to_heading(level, rest, workspace, document);
            }
        }

        if thematic_break_is(rest) {
            self.leaf_close(workspace, document)?;
            self.block_open_in_item(document);
            document.node_append(self.parent_current(), NodeKind::ThematicBreak)?;

            return Ok(());
        }

        if let Some(kind) = html_block_kind(rest, self.leaf == Leaf::Paragraph) {
            self.leaf_close(workspace, document)?;

            return self.html_open(kind, raw_line, document);
        }

        let table_possible = self.options.contains(Options::TABLES)
            && self.leaf == Leaf::Paragraph
            && self.paragraph_line_count == 1;

        if table_possible {
            if self.table_convert(rest, workspace, document)? {
                return Ok(());
            }
        }

        if self.leaf == Leaf::Paragraph {
            return self.paragraph_append(rest, workspace);
        }

        self.paragraph_open(rest, workspace, document)
    }

    fn blank_line(&mut self, document: &mut Document, workspace: &mut Workspace) -> Result<()> {
        assert!(document.node_count() >= 1);
        assert!(self.container_count as usize <= self.containers.len());

        match self.leaf {
            Leaf::IndentedCode => document.text_extend(&mut self.code_text, b"\n")?,
            Leaf::Paragraph | Leaf::Table | Leaf::HTMLBlock => {
                self.leaf_close(workspace, document)?;
            }
            Leaf::None | Leaf::FencedCode => {}
        }

        let Some(top) = self.container_count.checked_sub(1) else {
            return Ok(());
        };

        let top = top as usize;

        if self.containers[top].kind == ContainerKind::BlockQuote {
            return Ok(());
        }

        self.blank_pending_set(true);

        if let ContainerKind::ListItem { blank_pending, content_indent, .. } =
            self.containers[top].kind
        {
            if document.node(self.containers[top].node).child_first == NODE_NONE {
                self.containers[top].kind = ContainerKind::ListItem {
                    blank_pending,
                    closed_by_blank: true,
                    content_indent,
                };
            }
        }

        Ok(())
    }

    fn indented_line(
        &mut self,
        cursor: &mut Cursor<'_>,
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<()> {
        assert!(cursor.indent() >= CODE_INDENT);
        assert!(document.node_count() >= 1);

        match self.leaf {
            Leaf::Paragraph => self.paragraph_append(cursor.rest_after_indent(), workspace),
            Leaf::Table => self.table_row_append(cursor.rest_after_indent(), workspace, document),
            Leaf::IndentedCode => {
                cursor.indent_consume(CODE_INDENT);

                self.code_line_append(cursor, document)
            }
            Leaf::None => {
                cursor.indent_consume(CODE_INDENT);
                self.code_open(Leaf::IndentedCode, Span::EMPTY, document)?;

                self.code_line_append(cursor, document)
            }
            Leaf::FencedCode | Leaf::HTMLBlock => {
                unreachable!("fenced code and html lines are handled before leaf dispatch")
            }
        }
    }

    fn paragraph_open(
        &mut self,
        rest: &[u8],
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<()> {
        assert!(self.leaf == Leaf::None);
        assert!(document.node_count() >= 1);

        self.block_open_in_item(document);
        self.leaf_node = document.node_append(self.parent_current(), NodeKind::Paragraph)?;
        self.leaf = Leaf::Paragraph;
        self.paragraph_start = workspace.part_length as usize;
        self.paragraph_length = 0;
        self.paragraph_line_count = 0;

        self.paragraph_append(rest, workspace)
    }

    fn paragraph_append(&mut self, rest: &[u8], workspace: &mut Workspace) -> Result<()> {
        assert!(self.leaf == Leaf::Paragraph);
        assert!(self.paragraph_start <= workspace.part.len());

        let separator_length = usize::from(self.paragraph_line_count > 0);
        let start = self.paragraph_start + self.paragraph_length;
        let end = start + separator_length + rest.len();

        if end > workspace.part.len() {
            return Err(Error::WorkspaceCapacity { capacity_bytes: PART_BYTES_MAX });
        }

        if separator_length == 1 {
            workspace.part[start] = b'\n';
        }

        workspace.part[start + separator_length..end].copy_from_slice(rest);
        self.paragraph_length = end - self.paragraph_start;
        self.paragraph_line_count += 1;

        assert!(self.paragraph_line_count >= 1);

        Ok(())
    }

    fn paragraph_close(&self, workspace: &mut Workspace, document: &mut Document) -> Result<()> {
        assert!(self.leaf == Leaf::Paragraph);
        assert!(self.leaf_node < document.node_count());

        let start = self.paragraph_start;

        let trimmed_length =
            workspace.part[start..start + self.paragraph_length].trim_ascii_end().len();

        let offset = definitions_take(workspace, document, start..start + trimmed_length)?;
        let remaining = workspace.part[start + offset..start + trimmed_length].trim_ascii_start();
        let remaining_length = remaining.len();
        let remaining_offset = start + trimmed_length - remaining_length;
        workspace.part_length = u32_from_usize(start + trimmed_length);

        if remaining_length == 0 {
            let parent = document.node(self.leaf_node).parent;

            document.node_pop_last(parent);

            return Ok(());
        }

        let text = Span {
            length: u32_from_usize(remaining_length),
            offset: u32_from_usize(remaining_offset),
        };

        pending_push(workspace, self.leaf_node, text)
    }

    fn paragraph_to_heading(
        &mut self,
        level: u8,
        underline: &[u8],
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<()> {
        assert!(self.leaf == Leaf::Paragraph);
        assert!(level >= 1);
        assert!(level <= 2);

        document.node_mut(self.leaf_node).kind = NodeKind::Heading { level };
        let node = self.leaf_node;

        self.leaf_close(workspace, document)?;

        let heading_kept = node < document.node_count()
            && document.node(node).kind == (NodeKind::Heading { level });

        if !heading_kept {
            return self.paragraph_open(underline, workspace, document);
        }

        Ok(())
    }

    fn heading_create(
        &mut self,
        rest: &[u8],
        level: u8,
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<()> {
        assert!(level >= 1);
        assert!(level <= 6);
        assert!(self.leaf == Leaf::None);

        let content = atx_heading_content(rest, level);

        self.block_open_in_item(document);
        let node = document.node_append(self.parent_current(), NodeKind::Heading { level })?;
        let text = part_append(workspace, content)?;

        pending_push(workspace, node, text)
    }

    fn fence_open(
        &mut self,
        rest: &[u8],
        fence: Fence,
        indent: u32,
        document: &mut Document,
    ) -> Result<()> {
        assert!(fence.length >= 3);
        assert!(fence.length <= rest.len());
        assert!(indent < CODE_INDENT);

        let fence_text = rest[fence.length..].trim_ascii();

        let language_raw =
            fence_text.split(|&byte| byte.is_ascii_whitespace()).next().unwrap_or(b"");

        let mut language = Span { length: 0, offset: document.text_length() };

        inline::text_decode(language_raw, document, &mut language)?;

        self.fence_character = fence.character;
        self.fence_length = fence.length;
        self.fence_indent = indent;

        self.code_open(Leaf::FencedCode, language, document)
    }

    fn code_open(&mut self, leaf: Leaf, language: Span, document: &mut Document) -> Result<()> {
        assert!(self.leaf == Leaf::None);
        assert!(leaf == Leaf::FencedCode || leaf == Leaf::IndentedCode);
        assert!(language.end() <= document.text_length());

        self.block_open_in_item(document);
        let kind = NodeKind::CodeBlock { language };
        self.leaf_node = document.node_append(self.parent_current(), kind)?;
        self.leaf = leaf;
        self.code_text = Span { length: 0, offset: document.text_length() };

        Ok(())
    }

    fn fenced_line(&mut self, cursor: &mut Cursor<'_>, document: &mut Document) -> Result<()> {
        assert!(self.leaf == Leaf::FencedCode);
        assert!(self.fence_length >= 3);

        if cursor.indent() < CODE_INDENT {
            let rest = cursor.rest_after_indent();
            let run = rest.iter().take_while(|&&byte| byte == self.fence_character).count();

            if run >= self.fence_length {
                let tail_blank = rest[run..].iter().all(|&byte| byte == b' ' || byte == b'\t');

                if tail_blank {
                    return self.code_close(document);
                }
            }
        }

        cursor.indent_consume(self.fence_indent);

        self.code_line_append(cursor, document)
    }

    fn html_open(&mut self, kind: u8, cursor: &Cursor<'_>, document: &mut Document) -> Result<()> {
        assert!(self.leaf == Leaf::None);
        assert!(kind >= 1);
        assert!(kind <= 7);

        self.block_open_in_item(document);

        let node = document
            .node_append(self.parent_current(), NodeKind::HTMLBlock { text: Span::EMPTY })?;

        self.leaf_node = node;
        self.leaf = Leaf::HTMLBlock;
        self.html_kind = kind;
        self.code_text = Span { length: 0, offset: document.text_length() };

        self.html_line(cursor, document)
    }

    fn html_line(&mut self, cursor: &Cursor<'_>, document: &mut Document) -> Result<()> {
        assert!(self.leaf == Leaf::HTMLBlock);
        assert!(self.html_kind >= 1);

        if self.html_kind >= HTML_BLOCK_KIND_BLANK_TERMINATED_FIRST {
            if cursor.is_blank() {
                self.html_close(document);

                return Ok(());
            }
        }

        self.code_line_append(cursor, document)?;

        if self.html_kind < HTML_BLOCK_KIND_BLANK_TERMINATED_FIRST {
            if html_block_ends(self.html_kind, cursor.rest()) {
                self.html_close(document);
            }
        }

        Ok(())
    }

    fn html_close(&mut self, document: &mut Document) {
        assert!(self.leaf == Leaf::HTMLBlock);
        assert!(self.leaf_node < document.node_count());

        let mut text = self.code_text;

        let trailing =
            document.span_bytes(text).iter().rev().take_while(|&&byte| byte == b'\n').count();

        text.length -= u32_from_usize(trailing);
        document.node_mut(self.leaf_node).kind = NodeKind::HTMLBlock { text };
        self.leaf = Leaf::None;
    }

    fn code_line_append(&mut self, cursor: &Cursor<'_>, document: &mut Document) -> Result<()> {
        assert!(self.leaf != Leaf::None);
        assert!(self.code_text.end() == document.text_length());

        for _ in 0..cursor.tab_remaining() {
            document.text_extend(&mut self.code_text, b" ")?;
        }

        document.text_extend(&mut self.code_text, cursor.rest())?;

        document.text_extend(&mut self.code_text, b"\n")
    }

    fn code_close(&mut self, document: &mut Document) -> Result<()> {
        assert!(self.leaf == Leaf::FencedCode || self.leaf == Leaf::IndentedCode);
        assert!(self.leaf_node < document.node_count());

        let mut text = self.code_text;
        let bytes = document.span_bytes(text);
        let trailing = bytes.iter().rev().take_while(|&&byte| byte == b'\n').count();
        text.length -= u32_from_usize(trailing);

        if !text.is_empty() {
            let kind = NodeKind::Text { href: Span::EMPTY, marks: Marks::NONE, text };

            document.node_append(self.leaf_node, kind)?;
        }

        self.leaf = Leaf::None;

        Ok(())
    }

    fn table_convert(
        &mut self,
        rest: &[u8],
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<bool> {
        assert!(self.leaf == Leaf::Paragraph);
        assert!(self.paragraph_line_count == 1);

        let mut alignments = [Alignment::None; COLUMN_COUNT_MAX as usize];

        let Some(column_count) = delimiter_row_parse(rest, &mut alignments) else {
            return Ok(false);
        };

        let header = self.paragraph_start..self.paragraph_start + self.paragraph_length;

        if row_cell_count(&workspace.part[header.clone()]) != column_count {
            return Ok(false);
        }

        self.alignments = alignments;
        self.column_count = column_count;

        document.node_mut(self.leaf_node).kind =
            NodeKind::Table { column_count: self.column_count };

        self.leaf = Leaf::Table;
        workspace.part_length = u32_from_usize(header.end);

        let mut cells = [Span::EMPTY; COLUMN_COUNT_MAX as usize];
        let cell_count = row_cells_locate(&workspace.part, header, &mut cells);
        let row = document.node_append(self.leaf_node, NodeKind::TableRow { header: true })?;

        for (index, &alignment) in self.alignments[..column_count as usize].iter().enumerate() {
            let kind = NodeKind::TableCell { alignment, colspan: 1, header: true };
            let cell = document.node_append(row, kind)?;

            if index >= cell_count as usize {
                continue;
            }

            let text = cell_append_within(workspace, cells[index])?;

            pending_push(workspace, cell, text)?;
        }

        assert!(self.column_count >= 1);

        Ok(true)
    }

    fn table_row_append(
        &self,
        rest: &[u8],
        workspace: &mut Workspace,
        document: &mut Document,
    ) -> Result<()> {
        assert!(self.leaf == Leaf::Table);
        assert!(self.column_count >= 1);

        let column_count = self.column_count as usize;
        let row = document.node_append(self.leaf_node, NodeKind::TableRow { header: false })?;
        let mut cells = RowCells::new(rest);

        for &alignment in &self.alignments[..column_count] {
            let kind = NodeKind::TableCell { alignment, colspan: 1, header: false };
            let cell = document.node_append(row, kind)?;

            let Some(content) = cells.next() else {
                continue;
            };

            let text = cell_append(workspace, &rest[content])?;

            pending_push(workspace, cell, text)?;
        }

        Ok(())
    }
}

fn part_append(workspace: &mut Workspace, bytes: &[u8]) -> Result<Span> {
    assert!(workspace.part_length as usize <= workspace.part.len());

    let start = workspace.part_length as usize;
    let end = start + bytes.len();

    if end > workspace.part.len() {
        return Err(Error::WorkspaceCapacity { capacity_bytes: PART_BYTES_MAX });
    }

    workspace.part[start..end].copy_from_slice(bytes);
    workspace.part_length = u32_from_usize(end);

    assert!(workspace.part_length as usize == end);

    Ok(Span { length: u32_from_usize(bytes.len()), offset: u32_from_usize(start) })
}

fn cell_unescape(workspace: &mut Workspace, cell: Span) -> Span {
    assert!(cell.end() == workspace.part_length);

    let start = cell.offset as usize;
    let mut length = 0usize;
    let mut escaped = false;

    for index in start..cell.end() as usize {
        let byte = workspace.part[index];

        if escaped {
            if byte == b'|' {
                workspace.part[start + length - 1] = b'|';
                escaped = false;

                continue;
            }
        }

        workspace.part[start + length] = byte;
        length += 1;
        escaped = byte == b'\\' && !escaped;
    }

    workspace.part_length = u32_from_usize(start + length);

    assert!(length <= cell.length as usize);

    Span { length: u32_from_usize(length), offset: cell.offset }
}

fn cell_append(workspace: &mut Workspace, bytes: &[u8]) -> Result<Span> {
    let copied = part_append(workspace, bytes)?;
    let cell = cell_unescape(workspace, copied);

    assert!(cell.length <= copied.length);
    assert!(cell.end() == workspace.part_length);

    Ok(cell)
}

fn cell_append_within(workspace: &mut Workspace, source: Span) -> Result<Span> {
    assert!(source.end() <= workspace.part_length);

    let start = workspace.part_length as usize;
    let end = start + source.length as usize;

    if end > workspace.part.len() {
        return Err(Error::WorkspaceCapacity { capacity_bytes: PART_BYTES_MAX });
    }

    workspace.part.copy_within(source.offset as usize..source.end() as usize, start);
    workspace.part_length = u32_from_usize(end);
    let copied = Span { length: source.length, offset: u32_from_usize(start) };

    assert!(copied.end() == workspace.part_length);

    Ok(cell_unescape(workspace, copied))
}

fn pending_push(workspace: &mut Workspace, node: u32, text: Span) -> Result<()> {
    assert!(node != NODE_ROOT);
    assert!(text.end() <= workspace.part_length);

    if workspace.pending_inline_count >= PENDING_INLINE_COUNT_MAX {
        return Err(Error::NodeCapacity { node_count_max: PENDING_INLINE_COUNT_MAX });
    }

    let index = workspace.pending_inline_count as usize;
    workspace.pending_inlines[index] = PendingInline { node, text };
    workspace.pending_inline_count += 1;

    assert!(workspace.pending_inline_count <= PENDING_INLINE_COUNT_MAX);

    Ok(())
}

fn definitions_take(
    workspace: &mut Workspace,
    document: &mut Document,
    paragraph: Range<usize>,
) -> Result<usize> {
    assert!(paragraph.end <= workspace.part.len());

    let Workspace { definition_count, definitions, part, .. } = workspace;
    let mut offset = 0usize;

    for _ in 0..=paragraph.len() {
        let text = &part[paragraph.start + offset..paragraph.end];

        let Some(definition) = definition_parse(text) else {
            break;
        };

        let mut normalized = [0u8; inline::LABEL_LENGTH_MAX];

        if let Some(label_length) =
            inline::label_normalize(&text[definition.label], &mut normalized)
        {
            let bytes = DefinitionBytes {
                destination: &text[definition.destination],
                label: &normalized[..label_length],
            };

            definition_record(definitions, definition_count, document, bytes)?;
        }

        offset += definition.consumed as usize;
    }

    assert!(offset <= paragraph.len());

    Ok(offset)
}

fn definition_record(
    definitions: &mut [LinkDefinition],
    definition_count: &mut u32,
    document: &mut Document,
    bytes: DefinitionBytes<'_>,
) -> Result<()> {
    assert!(!bytes.label.is_empty());
    assert!(*definition_count as usize <= definitions.len());

    let label = bytes.label;
    let destination = bytes.destination;
    let count = *definition_count as usize;

    let exists = definitions[..count]
        .iter()
        .any(|definition| document.span_bytes(definition.label) == label);

    if exists {
        return Ok(());
    }

    if *definition_count >= DEFINITION_COUNT_MAX {
        return Err(Error::LinkCapacity { link_count_max: DEFINITION_COUNT_MAX });
    }

    let label_span = document.text_append(label)?;
    let mut url = Span { length: 0, offset: document.text_length() };

    inline::text_decode(destination, document, &mut url)?;
    definitions[count] = LinkDefinition { label: label_span, url };
    *definition_count += 1;

    Ok(())
}

fn whitespace_skip_one_newline(text: &[u8], mut position: usize) -> usize {
    assert!(position <= text.len());

    let mut newline_seen = false;

    for &byte in &text[position..] {
        match byte {
            b' ' | b'\t' => position += 1,
            b'\n' if !newline_seen => {
                newline_seen = true;
                position += 1;
            }
            _ => break,
        }
    }

    assert!(position <= text.len());

    position
}

fn line_end_if_blank(text: &[u8], start: usize) -> Option<usize> {
    assert!(start <= text.len());

    let rest = &text[start..];
    let skipped = rest.iter().take_while(|&&byte| byte == b' ' || byte == b'\t').count();

    assert!(skipped <= rest.len());

    match rest.get(skipped) {
        None => Some(text.len()),
        Some(b'\n') => Some(start + skipped + 1),
        Some(_) => None,
    }
}

fn definition_label_end(text: &[u8]) -> Option<usize> {
    if text.first() != Some(&b'[') {
        return None;
    }

    assert!(!text.is_empty());

    let mut escaped = false;
    let mut content = false;

    for (index, &byte) in text.iter().enumerate().skip(1) {
        if index > inline::LABEL_LENGTH_MAX + 1 {
            return None;
        }

        if escaped {
            escaped = false;
            content = true;

            continue;
        }

        match byte {
            b'\\' => escaped = true,
            b']' => {
                assert!(index < text.len());

                return content.then_some(index);
            }
            b'[' => return None,
            _ => {
                if !byte.is_ascii_whitespace() {
                    content = true;
                }
            }
        }
    }

    None
}

fn definition_parse(text: &[u8]) -> Option<Definition> {
    let label_close = definition_label_end(text)?;

    if text.get(label_close + 1) != Some(&b':') {
        return None;
    }

    let destination_start = whitespace_skip_one_newline(text, label_close + 2);
    let destination = inline::destination_scan(text, destination_start)?;

    if destination.inner.is_empty() {
        if text.get(destination_start) != Some(&b'<') {
            return None;
        }
    }

    let after = destination.after as usize;
    let destination_line_end = line_end_if_blank(text, after);
    let title_start = whitespace_skip_one_newline(text, after);

    let title_line_end = if title_start > after {
        inline::title_scan(text, title_start)
            .and_then(|title_end| line_end_if_blank(text, title_end))
    } else {
        None
    };

    let consumed = title_line_end.or(destination_line_end)?;

    assert!(label_close >= 1);
    assert!(consumed <= text.len());

    Some(Definition {
        consumed: u32_from_usize(consumed),
        destination: destination.inner,
        label: 1..label_close,
    })
}

fn html_tag_name_matches(rest: &[u8], names: &[&[u8]]) -> bool {
    assert!(!names.is_empty());

    let name_length = rest.iter().take_while(|&&byte| byte.is_ascii_alphanumeric()).count();
    let name = &rest[..name_length];

    assert!(name_length <= rest.len());

    let followed =
        rest.get(name_length).is_none_or(|&next| matches!(next, b' ' | b'\t' | b'>' | b'/'));

    followed && names.iter().any(|candidate| candidate.eq_ignore_ascii_case(name))
}

fn html_block_kind(rest: &[u8], paragraph_open: bool) -> Option<u8> {
    if rest.first() != Some(&b'<') {
        return None;
    }

    assert!(!rest.is_empty());

    if html_tag_name_matches(&rest[1..], &HTML_RAW_TEXT_TAGS) {
        let after_name = rest.get(1 + raw_text_name_length(&rest[1..]));

        if after_name.is_none_or(|&next| matches!(next, b' ' | b'\t' | b'>')) {
            return Some(1);
        }
    }

    if rest.starts_with(b"<!--") {
        return Some(2);
    }

    if rest.starts_with(b"<?") {
        return Some(3);
    }

    if rest.starts_with(b"<![CDATA[") {
        return Some(5);
    }

    if rest.starts_with(b"<!") {
        if rest.get(2).is_some_and(u8::is_ascii_alphabetic) {
            return Some(4);
        }
    }

    let after_slash = if rest.starts_with(b"</") { 2 } else { 1 };

    if html_tag_name_matches(&rest[after_slash..], &HTML_BLOCK_TAGS) {
        return Some(6);
    }

    if paragraph_open {
        return None;
    }

    let tag_length = inline::html_tag_length(rest, 0)?;

    assert!(tag_length <= rest.len());

    let is_tag = rest.starts_with(b"</") || rest.get(1).is_some_and(u8::is_ascii_alphabetic);
    let raw_text = html_tag_name_matches(&rest[after_slash..], &HTML_RAW_TEXT_TAGS);
    let tail_blank = rest[tag_length..].iter().all(|&byte| byte == b' ' || byte == b'\t');

    (is_tag && !raw_text && tail_blank).then_some(7)
}

fn raw_text_name_length(rest: &[u8]) -> usize {
    let length = rest.iter().take_while(|&&byte| byte.is_ascii_alphanumeric()).count();

    assert!(length <= rest.len());

    length
}

fn contains_ignore_case<const N: usize>(haystack: &[u8], needle: &[u8; N]) -> bool {
    assert!(N >= 1);
    assert!(N <= 16);

    haystack.len() >= N
        && (0..=haystack.len() - N)
            .any(|index| haystack[index..index + N].eq_ignore_ascii_case(needle))
}

fn html_block_ends(kind: u8, line: &[u8]) -> bool {
    assert!(kind >= 1);
    assert!(kind < HTML_BLOCK_KIND_BLANK_TERMINATED_FIRST);

    match kind {
        1 => {
            contains_ignore_case(line, b"</pre>")
                || contains_ignore_case(line, b"</script>")
                || contains_ignore_case(line, b"</style>")
                || contains_ignore_case(line, b"</textarea>")
        }
        2 => contains_ignore_case(line, b"-->"),
        3 => contains_ignore_case(line, b"?>"),
        4 => line.contains(&b'>'),
        _ => contains_ignore_case(line, b"]]>"),
    }
}

fn atx_heading_level(rest: &[u8]) -> Option<u8> {
    let hashes = rest.iter().take_while(|&&byte| byte == b'#').count();

    if hashes == 0 {
        return None;
    }

    if hashes > 6 {
        return None;
    }

    assert!(hashes >= 1);
    assert!(hashes <= 6);

    match rest.get(hashes) {
        None | Some(b' ' | b'\t') => Some(u8_from_usize(hashes)),
        Some(_) => None,
    }
}

fn atx_heading_content(rest: &[u8], level: u8) -> &[u8] {
    assert!(level >= 1);
    assert!(usize::from(level) <= rest.len());

    let content = rest[usize::from(level)..].trim_ascii();
    let closing = content.iter().rev().take_while(|&&byte| byte == b'#').count();

    if closing == content.len() {
        return b"";
    }

    let before_closing = content.len() - closing;
    let closing_detached = closing > 0 && content[before_closing - 1] == b' ';

    if closing_detached { content[..before_closing].trim_ascii_end() } else { content }
}

fn fence_open_parse(rest: &[u8]) -> Option<Fence> {
    let character = *rest.first()?;

    if !matches!(character, b'`' | b'~') {
        return None;
    }

    let length = rest.iter().take_while(|&&byte| byte == character).count();

    if length < 3 {
        return None;
    }

    assert!(length <= rest.len());

    if character == b'`' {
        if rest[length..].contains(&b'`') {
            return None;
        }
    }

    Some(Fence { character, length })
}

fn setext_level(rest: &[u8]) -> Option<u8> {
    let character = *rest.first()?;

    let level = match character {
        b'=' => 1,
        b'-' => 2,
        _ => return None,
    };

    let run = rest.iter().take_while(|&&byte| byte == character).count();
    let tail_blank = rest[run..].iter().all(|&byte| byte == b' ' || byte == b'\t');

    assert!(run >= 1);
    assert!(level >= 1);

    tail_blank.then_some(level)
}

fn thematic_break_is(rest: &[u8]) -> bool {
    let character = match rest.first() {
        Some(&byte) if matches!(byte, b'*' | b'-' | b'_') => byte,
        _ => return false,
    };

    let count = byte_count(rest, character);
    let only = rest.iter().all(|&byte| byte == character || byte == b' ' || byte == b'\t');

    assert!(count >= 1);

    count >= 3 && only
}

fn byte_count(haystack: &[u8], needle: u8) -> u32 {
    assert!(needle != 0);

    let mut count = 0u32;

    for &byte in haystack {
        if byte == needle {
            count += 1;
        }
    }

    assert!(count as usize <= haystack.len());

    count
}

fn list_marker_parse(cursor: &Cursor<'_>, interrupts_paragraph: bool) -> Option<ListMarker> {
    let indent = cursor.indent();

    if indent >= CODE_INDENT {
        return None;
    }

    let rest = cursor.rest_after_indent();

    let (bullet, ordered, start, marker_length) = match rest.first()? {
        b'-' | b'+' | b'*' => (rest[0], false, 1, 1),
        byte if byte.is_ascii_digit() => {
            let digits = rest.iter().take_while(|&&digit| digit.is_ascii_digit()).count();

            if digits > LIST_START_DIGIT_COUNT_MAX {
                return None;
            }

            let delimiter = *rest.get(digits)?;

            if !matches!(delimiter, b'.' | b')') {
                return None;
            }

            (delimiter, true, decimal_parse_u32(&rest[..digits])?, digits + 1)
        }
        _ => return None,
    };

    let after = &rest[marker_length..];
    let spaces = after.iter().take_while(|&&byte| byte == b' ' || byte == b'\t').count();
    let empty = spaces == after.len();

    if !empty {
        if spaces == 0 {
            return None;
        }
    }

    let interrupt_refused = interrupts_paragraph && (empty || (ordered && start != 1));

    if interrupt_refused {
        return None;
    }

    let indent_collapses = empty || spaces >= 5;

    let content_indent = if indent_collapses {
        indent + u32_from_usize(marker_length) + 1
    } else {
        indent + u32_from_usize(marker_length) + u32_from_usize(spaces)
    };

    assert!(marker_length >= 1);
    assert!(content_indent > indent);

    Some(ListMarker { bullet, content_indent, indent, marker_length, ordered, start })
}

fn task_marker_parse(rest: &[u8]) -> TaskState {
    if rest.len() < 3 {
        return TaskState::None;
    }

    assert!(rest.len() >= 3);

    if rest[0] != b'[' {
        return TaskState::None;
    }

    if rest[2] != b']' {
        return TaskState::None;
    }

    let followed = rest.get(3).is_none_or(|&byte| byte == b' ' || byte == b'\t');

    if !followed {
        return TaskState::None;
    }

    match rest[1] {
        b' ' => TaskState::Unchecked,
        b'x' | b'X' => TaskState::Checked,
        _ => TaskState::None,
    }
}

#[derive(Clone, Copy, Debug)]
struct RowCells<'a> {
    done: bool,
    end: usize,
    position: usize,
    row: &'a [u8],
}

impl<'a> RowCells<'a> {
    fn new(row: &'a [u8]) -> Self {
        let leading = row.len() - row.trim_ascii_start().len();
        let trimmed = row.trim_ascii();
        let start = leading + usize::from(trimmed.first() == Some(&b'|'));

        let end_pipe = leading + trimmed.len() > start
            && trimmed[trimmed.len() - 1] == b'|'
            && (trimmed.len() < 2 || trimmed[trimmed.len() - 2] != b'\\');

        let end = leading + trimmed.len() - usize::from(end_pipe);

        assert!(start <= end);
        assert!(end <= row.len());

        Self { done: trimmed.is_empty(), end, position: start, row }
    }

    fn next(&mut self) -> Option<Range<usize>> {
        assert!(self.position <= self.end);

        if self.done {
            return None;
        }

        let rest = &self.row[self.position..self.end];
        let mut escaped = false;
        let mut length = rest.len();

        for (index, &byte) in rest.iter().enumerate() {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'|' {
                length = index;

                break;
            }
        }

        let cell = &rest[..length];
        let leading = cell.len() - cell.trim_ascii_start().len();
        let trimmed_length = cell.trim_ascii().len();
        let cell_start = self.position + leading;
        let range = cell_start..cell_start + trimmed_length;

        if length == rest.len() {
            self.done = true;
        } else {
            self.position += length + 1;
        }

        assert!(range.end <= self.end);

        Some(range)
    }
}

fn row_cell_count(row: &[u8]) -> u32 {
    let mut cells = RowCells::new(row);
    let mut count = 0u32;

    for _ in 0..=row.len() {
        if cells.next().is_none() {
            break;
        }

        count += 1;
    }

    assert!(count as usize <= row.len() + 1);

    count
}

fn row_cells_locate(
    part: &[u8],
    row: Range<usize>,
    cells: &mut [Span; COLUMN_COUNT_MAX as usize],
) -> u32 {
    assert!(row.end <= part.len());

    let bytes = &part[row.clone()];
    let mut iterator = RowCells::new(bytes);
    let mut count = 0u32;

    for _ in 0..=bytes.len() {
        let Some(cell) = iterator.next() else {
            break;
        };

        if count >= COLUMN_COUNT_MAX {
            break;
        }

        cells[count as usize] = Span {
            length: u32_from_usize(cell.len()),
            offset: u32_from_usize(row.start + cell.start),
        };

        count += 1;
    }

    assert!(count <= COLUMN_COUNT_MAX);

    count
}

fn delimiter_row_parse(
    rest: &[u8],
    alignments: &mut [Alignment; COLUMN_COUNT_MAX as usize],
) -> Option<u32> {
    if !rest.contains(&b'-') {
        return None;
    }

    assert!(!rest.is_empty());

    let mut cells = RowCells::new(rest);
    let mut count = 0u32;

    for _ in 0..=rest.len() {
        let Some(range) = cells.next() else {
            break;
        };

        if count >= COLUMN_COUNT_MAX {
            return None;
        }

        let cell = &rest[range];
        let left = cell.first() == Some(&b':');
        let right = cell.len() > 1 && cell[cell.len() - 1] == b':';
        let dashes_start = usize::from(left);
        let dashes_end = if right { cell.len() - 1 } else { cell.len() };

        if dashes_end <= dashes_start {
            return None;
        }

        if !cell[dashes_start..dashes_end].iter().all(|&byte| byte == b'-') {
            return None;
        }

        alignments[count as usize] = match (left, right) {
            (true, true) => Alignment::Center,
            (true, false) => Alignment::Left,
            (false, true) => Alignment::Right,
            (false, false) => Alignment::None,
        };

        count += 1;
    }

    assert!(count <= COLUMN_COUNT_MAX);

    if count == 0 { None } else { Some(count) }
}
