use crate::bytes::Sink;
use crate::document::{
    DEPTH_MAX,
    Document,
    HEADING_LEVEL_MAX,
    Marks,
    NODE_NONE,
    NODE_ROOT,
    NodeKind,
    Span,
    TaskState,
    TextRun,
    Walk,
    WalkEvent,
};
use crate::error::Result;
use crate::json::string_write;

const ARRAY_STACK_MAX: u32 = DEPTH_MAX as u32 * 2 + 4;
const WRAPPER_COUNT_MAX: usize = DEPTH_MAX as usize + 1;

#[derive(Clone, Copy, Debug)]
struct ListStyle {
    ordered: bool,
    start: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wrapper {
    HeadingAsParagraph,
    None,
    Paragraph,
    ParagraphOnly,
    TaskList,
    Transparent,
}

#[derive(Debug)]
struct Writer<'a, 'b, 'c> {
    array_count: u32,
    arrays: [bool; ARRAY_STACK_MAX as usize],
    document: &'c Document,
    sink: &'b mut Sink<'a>,
    strong_forced: u32,
    task_identifier: u32,
    wrappers: [Wrapper; WRAPPER_COUNT_MAX],
}

pub fn write(document: &Document, output: &mut [u8]) -> Result<u32> {
    assert!(document.node_count() >= 1);
    assert!(document.node(NODE_ROOT).kind == NodeKind::Document);

    let mut sink = Sink::new(output);

    let mut writer = Writer {
        array_count: 0,
        arrays: [false; ARRAY_STACK_MAX as usize],
        document,
        sink: &mut sink,
        strong_forced: 0,
        task_identifier: 0,
        wrappers: [Wrapper::None; WRAPPER_COUNT_MAX],
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

    assert!(writer.array_count == 0);
    assert!(writer.strong_forced == 0);

    Ok(sink.length())
}

impl Writer<'_, '_, '_> {
    fn array_open(&mut self) -> Result<()> {
        assert!(self.array_count < ARRAY_STACK_MAX);

        self.sink.write(b"[")?;
        self.arrays[self.array_count as usize] = false;
        self.array_count += 1;

        assert!(self.array_count <= ARRAY_STACK_MAX);

        Ok(())
    }

    fn array_close(&mut self) -> Result<()> {
        assert!(self.array_count > 0);

        self.array_count -= 1;

        assert!(self.array_count < ARRAY_STACK_MAX);

        self.sink.write(b"]")
    }

    fn item_begin(&mut self) -> Result<()> {
        assert!(self.array_count > 0);

        let top = (self.array_count - 1) as usize;

        if self.arrays[top] {
            self.sink.write(b",")?;
        }

        self.arrays[top] = true;

        assert!(self.arrays[top]);

        Ok(())
    }

    fn node_open(&mut self, type_name: &[u8]) -> Result<()> {
        assert!(!type_name.is_empty());
        assert!(type_name.is_ascii());

        self.item_begin()?;
        self.sink.write(b"{\"type\":\"")?;
        self.sink.write(type_name)?;

        self.sink.write(b"\"")
    }

    fn content_open(&mut self) -> Result<()> {
        assert!(self.array_count < ARRAY_STACK_MAX);

        self.sink.write(b",\"content\":")?;
        self.array_open()?;

        assert!(self.array_count > 0);

        Ok(())
    }

    fn content_close(&mut self) -> Result<()> {
        assert!(self.array_count > 0);

        self.array_close()?;

        self.sink.write(b"}")
    }

    fn paragraph_empty(&mut self) -> Result<()> {
        assert!(self.array_count > 0);

        self.node_open(b"paragraph")?;
        self.content_open()?;
        self.content_close()?;

        assert!(self.arrays[(self.array_count - 1) as usize]);

        Ok(())
    }

    fn text_node(&mut self, text: &[u8]) -> Result<()> {
        assert!(!text.is_empty());
        assert!(self.array_count > 0);

        self.node_open(b"text")?;
        self.sink.write(b",\"text\":")?;
        string_write(self.sink, text)?;

        self.sink.write(b"}")
    }

    fn ancestor_is<F: Fn(NodeKind) -> bool>(&self, index: u32, wanted: F) -> bool {
        assert!(index < self.document.node_count());
        assert!(self.document.node(index).depth <= DEPTH_MAX);

        let mut current = self.document.node(index).parent;

        for _ in 0..DEPTH_MAX {
            if current == NODE_ROOT {
                return false;
            }

            if wanted(self.document.node(current).kind) {
                return true;
            }

            current = self.document.node(current).parent;
        }

        false
    }

    fn inside_quote_or_item(&self, index: u32) -> bool {
        assert!(index < self.document.node_count());

        self.ancestor_is(index, |kind| {
            matches!(kind, NodeKind::BlockQuote | NodeKind::ListItem { .. })
        })
    }

    fn subtree_contains_table(&self, index: u32) -> bool {
        assert!(index < self.document.node_count());
        assert!(self.document.node(index).kind == NodeKind::BlockQuote);

        let mut walk = Walk::new(self.document, index);

        for _ in 0..self.document.node_count() * 2 {
            let Some(event) = walk.next(self.document) else {
                break;
            };

            if let WalkEvent::Enter(child) = event {
                let other_table = child != index
                    && matches!(self.document.node(child).kind, NodeKind::Table { .. });

                if other_table {
                    return true;
                }
            }
        }

        false
    }

    fn list_is_task(&self, index: u32) -> bool {
        assert!(index < self.document.node_count());
        assert!(matches!(self.document.node(index).kind, NodeKind::List { .. }));

        let document = self.document;
        let mut items = document.children(index);
        let mut any = false;

        for _ in 0..document.node_count() {
            let Some(item) = items.next(document) else {
                break;
            };

            let NodeKind::ListItem { task } = document.node(item).kind else {
                return false;
            };

            if task == TaskState::None {
                return false;
            }

            let first = document.node(item).child_first;

            if first != NODE_NONE {
                let only_paragraph = document.node(first).kind == NodeKind::Paragraph
                    && document.node(first).sibling_next == NODE_NONE;

                if !only_paragraph {
                    return false;
                }
            }

            any = true;
        }

        any
    }

    fn enter(&mut self, index: u32, walk: &mut Walk) -> Result<()> {
        assert!(index < self.document.node_count());

        let node = *self.document.node(index);
        let depth = usize::from(node.depth);
        let parent_wrapper = if depth == 0 { Wrapper::None } else { self.wrappers[depth - 1] };
        self.wrappers[depth] = Wrapper::None;

        match node.kind {
            NodeKind::Document => {
                self.sink.write(b"{\"version\":1,\"type\":\"doc\"")?;
                self.content_open()?;
            }
            NodeKind::Paragraph => {
                if parent_wrapper == Wrapper::TaskList {
                    self.wrappers[depth] = Wrapper::Transparent;
                } else {
                    self.node_open(b"paragraph")?;
                    self.content_open()?;
                }
            }
            NodeKind::Heading { level } => self.heading_enter(index, level, depth)?,
            NodeKind::BlockQuote => self.block_quote_enter(index, depth)?,
            NodeKind::List { ordered, start, .. } => {
                self.list_enter(index, ListStyle { ordered, start }, depth)?;
            }
            NodeKind::ListItem { task } => self.list_item_enter(index, task, depth)?,
            NodeKind::CodeBlock { language } => {
                self.code_block(index, language)?;
                walk.skip_children(self.document);
            }
            NodeKind::ThematicBreak => self.rule(index)?,
            NodeKind::Table { .. } => self.table_enter(index, depth)?,
            NodeKind::TableRow { .. } => {
                if parent_wrapper == Wrapper::Transparent {
                    self.wrappers[depth] = Wrapper::Transparent;
                } else {
                    self.node_open(b"tableRow")?;
                    self.content_open()?;
                }
            }
            NodeKind::TableCell { .. } => {
                if parent_wrapper == Wrapper::Transparent {
                    self.cell_enter_flat(index, depth)?;
                } else {
                    self.cell_enter(index, depth)?;
                }
            }
            NodeKind::Text { href, marks, text } => self.text(TextRun { href, marks, text })?,
            NodeKind::HardBreak => {
                self.node_open(b"hardBreak")?;
                self.sink.write(b"}")?;
            }
            NodeKind::Image { .. } => self.image(index)?,
            NodeKind::HTMLBlock { text } => {
                self.node_open(b"codeBlock")?;
                self.sink.write(b",\"attrs\":{\"language\":\"html\"}")?;
                self.content_open()?;
                self.text_node(self.document.span_bytes(text))?;
                self.content_close()?;
            }
            NodeKind::HTMLInline { text } => {
                self.text(TextRun { href: Span::EMPTY, marks: Marks::NONE, text })?;
            }
            NodeKind::Unused => unreachable!("unused node reached by walk"),
        }

        Ok(())
    }

    fn leave(&mut self, index: u32) -> Result<()> {
        assert!(index < self.document.node_count());

        let node = *self.document.node(index);
        let depth = usize::from(node.depth);
        let wrapper = self.wrappers[depth];

        match node.kind {
            NodeKind::Document | NodeKind::ListItem { .. } => self.content_close()?,
            NodeKind::Paragraph
            | NodeKind::Table { .. }
            | NodeKind::TableRow { .. }
            | NodeKind::BlockQuote
            | NodeKind::List { .. } => {
                if wrapper != Wrapper::Transparent {
                    self.content_close()?;
                }
            }
            NodeKind::Heading { .. } => {
                if wrapper == Wrapper::HeadingAsParagraph {
                    assert!(self.strong_forced > 0);

                    self.strong_forced -= 1;
                }

                self.content_close()?;
            }
            NodeKind::TableCell { .. } => match wrapper {
                Wrapper::Paragraph => {
                    self.content_close()?;
                    self.content_close()?;
                }
                Wrapper::Transparent => {}
                Wrapper::ParagraphOnly
                | Wrapper::None
                | Wrapper::HeadingAsParagraph
                | Wrapper::TaskList => self.content_close()?,
            },
            NodeKind::CodeBlock { .. }
            | NodeKind::ThematicBreak
            | NodeKind::Text { .. }
            | NodeKind::HardBreak
            | NodeKind::Image { .. }
            | NodeKind::HTMLBlock { .. }
            | NodeKind::HTMLInline { .. } => {}
            NodeKind::Unused => unreachable!("unused node reached by walk"),
        }

        Ok(())
    }

    fn heading_enter(&mut self, index: u32, level: u8, depth: usize) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(level >= 1);
        assert!(level <= HEADING_LEVEL_MAX);
        assert!(depth < WRAPPER_COUNT_MAX);

        if self.inside_quote_or_item(index) {
            self.wrappers[depth] = Wrapper::HeadingAsParagraph;
            self.strong_forced += 1;

            self.node_open(b"paragraph")?;
        } else {
            self.node_open(b"heading")?;
            self.sink.write(b",\"attrs\":{\"level\":")?;
            self.sink.write_u32(u32::from(level))?;
            self.sink.write(b"}")?;
        }

        self.content_open()
    }

    fn block_quote_enter(&mut self, index: u32, depth: usize) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(depth < WRAPPER_COUNT_MAX);

        let transparent = self.inside_quote_or_item(index) || self.subtree_contains_table(index);

        if transparent {
            self.wrappers[depth] = Wrapper::Transparent;

            return Ok(());
        }

        self.node_open(b"blockquote")?;

        self.content_open()
    }

    fn list_enter(&mut self, index: u32, style: ListStyle, depth: usize) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(depth < WRAPPER_COUNT_MAX);

        if self.list_is_task(index) {
            self.wrappers[depth] = Wrapper::TaskList;

            self.node_open(b"taskList")?;
            self.sink.write(b",\"attrs\":{\"localId\":\"task-list-")?;
            self.sink.write_u32(index)?;
            self.sink.write(b"\"}")?;

            return self.content_open();
        }

        if style.ordered {
            self.node_open(b"orderedList")?;
            self.sink.write(b",\"attrs\":{\"order\":")?;
            self.sink.write_u32(style.start)?;
            self.sink.write(b"}")?;
        } else {
            self.node_open(b"bulletList")?;
        }

        self.content_open()
    }

    fn list_item_enter(&mut self, index: u32, task: TaskState, depth: usize) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(depth < WRAPPER_COUNT_MAX);

        let node = *self.document.node(index);
        let list_depth = usize::from(self.document.node(node.parent).depth);

        if self.wrappers[list_depth] == Wrapper::TaskList {
            self.wrappers[depth] = Wrapper::TaskList;
            self.task_identifier += 1;

            self.node_open(b"taskItem")?;
            self.sink.write(b",\"attrs\":{\"localId\":\"task-")?;
            self.sink.write_u32(self.task_identifier)?;
            self.sink.write(b"\",\"state\":\"")?;
            self.sink.write(if task == TaskState::Checked { b"DONE" } else { b"TODO" })?;
            self.sink.write(b"\"}")?;

            return self.content_open();
        }

        self.node_open(b"listItem")?;
        self.content_open()?;

        let child_first_is_block = node.child_first != NODE_NONE
            && matches!(
                self.document.node(node.child_first).kind,
                NodeKind::Paragraph | NodeKind::CodeBlock { .. }
            );

        if !child_first_is_block {
            self.paragraph_empty()?;
        }

        if task != TaskState::None {
            self.task_glyph_pending(task)?;
        }

        Ok(())
    }

    fn task_glyph_pending(&mut self, task: TaskState) -> Result<()> {
        assert!(task != TaskState::None);
        assert!(self.array_count > 0);

        let glyph: &[u8] = if task == TaskState::Checked {
            "\u{2611} ".as_bytes()
        } else {
            "\u{2610} ".as_bytes()
        };

        self.node_open(b"paragraph")?;
        self.content_open()?;
        self.text_node(glyph)?;

        self.content_close()
    }

    fn code_block(&mut self, index: u32, language: Span) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(language.end() <= self.document.text_length());

        let document = self.document;

        self.node_open(b"codeBlock")?;

        if !language.is_empty() {
            self.sink.write(b",\"attrs\":{\"language\":")?;
            string_write(self.sink, document.span_bytes(language))?;
            self.sink.write(b"}")?;
        }

        self.content_open()?;

        let mut children = document.children(index);

        for _ in 0..document.node_count() {
            let Some(child) = children.next(document) else {
                break;
            };

            if let NodeKind::Text { text, .. } = document.node(child).kind {
                if text.is_empty() {
                    continue;
                }

                self.text_node(document.span_bytes(text))?;
            }
        }

        self.content_close()
    }

    fn rule(&mut self, index: u32) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(self.array_count > 0);

        if self.inside_quote_or_item(index) {
            self.node_open(b"paragraph")?;
            self.content_open()?;
            self.text_node("\u{2500}\u{2500}\u{2500}".as_bytes())?;

            return self.content_close();
        }

        self.node_open(b"rule")?;

        self.sink.write(b"}")
    }

    fn table_enter(&mut self, index: u32, depth: usize) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(depth < WRAPPER_COUNT_MAX);

        let nested = self.ancestor_is(index, |kind| matches!(kind, NodeKind::TableCell { .. }));

        if nested {
            self.wrappers[depth] = Wrapper::Transparent;

            return Ok(());
        }

        self.node_open(b"table")?;
        self.sink.write(b",\"attrs\":{\"isNumberColumnEnabled\":false,\"layout\":\"default\"}")?;

        self.content_open()
    }

    fn cell_enter_flat(&mut self, index: u32, depth: usize) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(depth < WRAPPER_COUNT_MAX);

        let node = *self.document.node(index);

        let child_first_inline =
            node.child_first != NODE_NONE && self.document.node(node.child_first).kind.is_inline();

        if child_first_inline {
            self.wrappers[depth] = Wrapper::ParagraphOnly;

            self.node_open(b"paragraph")?;

            return self.content_open();
        }

        self.wrappers[depth] = Wrapper::Transparent;

        Ok(())
    }

    fn cell_enter(&mut self, index: u32, depth: usize) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(depth < WRAPPER_COUNT_MAX);

        let node = *self.document.node(index);

        let NodeKind::TableCell { colspan, header, .. } = node.kind else {
            unreachable!("cell writer called on a non-cell node");
        };

        assert!(colspan >= 1);

        self.node_open(if header { b"tableHeader" } else { b"tableCell" })?;
        self.sink.write(b",\"attrs\":{\"colspan\":")?;
        self.sink.write_u32(colspan)?;
        self.sink.write(b"}")?;
        self.content_open()?;

        let child_first_inline =
            node.child_first != NODE_NONE && self.document.node(node.child_first).kind.is_inline();

        if child_first_inline {
            self.wrappers[depth] = Wrapper::Paragraph;

            self.node_open(b"paragraph")?;
            self.content_open()?;
        } else if node.child_first == NODE_NONE {
            self.paragraph_empty()?;
        }

        Ok(())
    }

    fn text(&mut self, run: TextRun) -> Result<()> {
        assert!(run.text.end() <= self.document.text_length());
        assert!(run.href.end() <= self.document.text_length());

        let TextRun { href, marks, text } = run;
        let shown = if text.is_empty() { href } else { text };

        if shown.is_empty() {
            return Ok(());
        }

        self.node_open(b"text")?;
        self.sink.write(b",\"text\":")?;
        string_write(self.sink, self.document.span_bytes(shown))?;

        let mut effective = marks;

        if self.strong_forced > 0 {
            effective = effective.union(Marks::STRONG);
        }

        if effective.contains(Marks::CODE) {
            effective = Marks::CODE;
        }

        if effective.is_empty() {
            if href == Span::EMPTY {
                return self.sink.write(b"}");
            }
        }

        self.sink.write(b",\"marks\":")?;
        self.array_open()?;

        let pairs: [(Marks, &[u8]); 4] = [
            (Marks::STRONG, b"strong"),
            (Marks::EMPHASIS, b"em"),
            (Marks::STRIKETHROUGH, b"strike"),
            (Marks::CODE, b"code"),
        ];

        for (mark, name) in pairs {
            if effective.contains(mark) {
                self.node_open(name)?;
                self.sink.write(b"}")?;
            }
        }

        if href != Span::EMPTY {
            self.node_open(b"link")?;
            self.sink.write(b",\"attrs\":{\"href\":")?;
            string_write(self.sink, self.document.span_bytes(href))?;
            self.sink.write(b"}}")?;
        }

        self.array_close()?;

        self.sink.write(b"}")
    }

    fn image(&mut self, index: u32) -> Result<()> {
        assert!(index < self.document.node_count());

        let NodeKind::Image { alt, url } = self.document.node(index).kind else {
            unreachable!("image writer called on a non-image node");
        };

        let document = self.document;

        assert!(url.end() <= document.text_length());

        let label = if alt.is_empty() { url } else { alt };

        if label.is_empty() {
            return Ok(());
        }

        self.node_open(b"text")?;
        self.sink.write(b",\"text\":")?;
        string_write(self.sink, document.span_bytes(label))?;

        if !url.is_empty() {
            self.sink.write(b",\"marks\":[{\"type\":\"link\",\"attrs\":{\"href\":")?;
            string_write(self.sink, document.span_bytes(url))?;
            self.sink.write(b"}}]")?;
        }

        self.sink.write(b"}")
    }
}
