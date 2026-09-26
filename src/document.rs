use crate::error::{Error, Result};
use core::fmt;

pub const DEPTH_MAX: u8 = 32;
pub const HEADING_LEVEL_MAX: u8 = 6;
pub const NODE_COUNT_MAX: u32 = 1 << 16;
pub const NODE_NONE: u32 = 0;
pub const NODE_ROOT: u32 = 0;
pub const TABLE_COLSPAN_MAX: u32 = 64;
pub const TEXT_BYTES_MAX: u32 = 1 << 22;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub length: u32,
    pub offset: u32,
}

impl Span {
    pub const EMPTY: Self = Self { length: 0, offset: 0 };

    #[must_use]
    pub const fn end(self) -> u32 {
        let end = self.offset + self.length;

        assert!(end >= self.offset);

        end
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.length == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRun {
    pub href: Span,
    pub marks: Marks,
    pub text: Span,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    bits: u8,
}

impl Marks {
    pub const CODE: Self = Self { bits: 1 << 3 };
    pub const EMPHASIS: Self = Self { bits: 1 << 1 };
    pub const NONE: Self = Self { bits: 0 };
    pub const STRIKETHROUGH: Self = Self { bits: 1 << 2 };
    pub const STRONG: Self = Self { bits: 1 << 0 };

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.bits & other.bits == other.bits
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self { bits: self.bits | other.bits }
    }

    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self { bits: self.bits & !other.bits }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Alignment {
    Center,
    Left,
    #[default]
    None,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskState {
    Checked,
    #[default]
    None,
    Unchecked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeKind {
    Unused = 0,
    BlockQuote,
    CodeBlock { language: Span },
    Document,
    HardBreak,
    Heading { level: u8 },
    HTMLBlock { text: Span },
    HTMLInline { text: Span },
    Image { alt: Span, url: Span },
    List { ordered: bool, start: u32, tight: bool },
    ListItem { task: TaskState },
    Paragraph,
    Table { column_count: u32 },
    TableCell { alignment: Alignment, colspan: u32, header: bool },
    TableRow { header: bool },
    Text { href: Span, marks: Marks, text: Span },
    ThematicBreak,
}

impl NodeKind {
    #[must_use]
    pub const fn is_block(self) -> bool {
        matches!(
            self,
            Self::BlockQuote
                | Self::CodeBlock { .. }
                | Self::Heading { .. }
                | Self::HTMLBlock { .. }
                | Self::List { .. }
                | Self::ListItem { .. }
                | Self::Paragraph
                | Self::Table { .. }
                | Self::TableCell { .. }
                | Self::TableRow { .. }
                | Self::ThematicBreak,
        )
    }

    #[must_use]
    pub const fn is_inline(self) -> bool {
        matches!(
            self,
            Self::HardBreak | Self::HTMLInline { .. } | Self::Image { .. } | Self::Text { .. }
        )
    }

    const fn is_well_formed(self) -> bool {
        match self {
            Self::Heading { level } => level >= 1 && level <= HEADING_LEVEL_MAX,
            Self::TableCell { colspan, .. } => colspan >= 1,
            Self::Unused | Self::Document => false,
            Self::BlockQuote
            | Self::CodeBlock { .. }
            | Self::HardBreak
            | Self::HTMLBlock { .. }
            | Self::HTMLInline { .. }
            | Self::Image { .. }
            | Self::List { .. }
            | Self::ListItem { .. }
            | Self::Paragraph
            | Self::Table { .. }
            | Self::TableRow { .. }
            | Self::Text { .. }
            | Self::ThematicBreak => true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub child_first: u32,
    pub child_last: u32,
    pub depth: u8,
    pub kind: NodeKind,
    pub parent: u32,
    pub sibling_next: u32,
}

impl Node {
    pub const UNUSED: Self = Self {
        child_first: NODE_NONE,
        child_last: NODE_NONE,
        depth: 0,
        kind: NodeKind::Unused,
        parent: NODE_NONE,
        sibling_next: NODE_NONE,
    };
}

pub struct Document {
    node_count: u32,
    nodes: [Node; NODE_COUNT_MAX as usize],
    text: [u8; TEXT_BYTES_MAX as usize],
    text_length: u32,
}

impl Document {
    pub const EMPTY: Self = Self {
        node_count: 0,
        nodes: [Node::UNUSED; NODE_COUNT_MAX as usize],
        text: [0; TEXT_BYTES_MAX as usize],
        text_length: 0,
    };

    pub fn reset(&mut self) {
        self.node_count = 1;
        self.text_length = 1;
        self.text[0] = 0;
        self.nodes[NODE_ROOT as usize] = Node { kind: NodeKind::Document, ..Node::UNUSED };

        assert!(self.node(NODE_ROOT).kind == NodeKind::Document);
        assert!(self.node(NODE_ROOT).depth == 0);
    }

    #[must_use]
    pub fn children(&self, index: u32) -> Children {
        assert!(index < self.node_count);

        Children { next: self.nodes[index as usize].child_first, remaining: self.node_count }
    }

    #[must_use]
    pub fn node(&self, index: u32) -> &Node {
        assert!(self.node_count >= 1);
        assert!(index < self.node_count);

        &self.nodes[index as usize]
    }

    pub fn node_mut(&mut self, index: u32) -> &mut Node {
        assert!(self.node_count >= 1);
        assert!(index < self.node_count);

        &mut self.nodes[index as usize]
    }

    pub fn node_append(&mut self, parent: u32, kind: NodeKind) -> Result<u32> {
        assert!(self.node_count >= 1);
        assert!(parent < self.node_count);
        assert!(kind.is_well_formed());

        let depth = self.nodes[parent as usize].depth + 1;

        if depth > DEPTH_MAX {
            return Err(Error::DepthExceeded { depth_max: DEPTH_MAX });
        }

        if self.node_count == NODE_COUNT_MAX {
            return Err(Error::NodeCapacity { node_count_max: NODE_COUNT_MAX });
        }

        let index = self.node_count;
        self.node_count += 1;
        self.nodes[index as usize] = Node { depth, kind, parent, ..Node::UNUSED };

        let previous = self.nodes[parent as usize].child_last;

        if previous == NODE_NONE {
            self.nodes[parent as usize].child_first = index;
        } else {
            self.nodes[previous as usize].sibling_next = index;
        }

        self.nodes[parent as usize].child_last = index;

        assert!(index != NODE_NONE);
        assert!(self.nodes[index as usize].sibling_next == NODE_NONE);
        assert!(self.nodes[parent as usize].child_first != NODE_NONE);

        Ok(index)
    }

    #[must_use]
    pub const fn node_count(&self) -> u32 {
        self.node_count
    }

    pub fn node_pop_last(&mut self, parent: u32) {
        assert!(self.node_count >= 2);
        assert!(parent < self.node_count);

        let index = self.node_count - 1;

        assert!(self.nodes[index as usize].parent == parent);
        assert!(self.nodes[index as usize].child_first == NODE_NONE);
        assert!(self.nodes[parent as usize].child_last == index);

        let mut previous = NODE_NONE;
        let mut current = self.nodes[parent as usize].child_first;

        for _ in 0..self.node_count {
            if current == index {
                break;
            }

            previous = current;
            current = self.nodes[current as usize].sibling_next;
        }

        assert!(current == index);

        if previous == NODE_NONE {
            self.nodes[parent as usize].child_first = NODE_NONE;
        } else {
            self.nodes[previous as usize].sibling_next = NODE_NONE;
        }

        self.nodes[parent as usize].child_last = previous;
        self.nodes[index as usize] = Node::UNUSED;
        self.node_count = index;

        assert!(self.node_count >= 1);
    }

    #[must_use]
    pub fn span_bytes(&self, span: Span) -> &[u8] {
        assert!(span.end() <= self.text_length);

        &self.text[span.offset as usize..span.end() as usize]
    }

    pub fn text_append(&mut self, bytes: &[u8]) -> Result<Span> {
        assert!(self.text_length >= 1);
        assert!(self.text_length <= TEXT_BYTES_MAX);

        let offset = self.text_length;

        let Ok(length) = u32::try_from(bytes.len()) else {
            return Err(Error::TextCapacity { capacity_bytes: TEXT_BYTES_MAX });
        };

        if length > TEXT_BYTES_MAX - offset {
            return Err(Error::TextCapacity { capacity_bytes: TEXT_BYTES_MAX });
        }

        self.text[offset as usize..(offset + length) as usize].copy_from_slice(bytes);
        self.text_length += length;

        assert!(self.text_length == offset + length);

        Ok(Span { length, offset })
    }

    pub fn text_extend(&mut self, span: &mut Span, bytes: &[u8]) -> Result<()> {
        assert!(span.end() == self.text_length);

        let appended = self.text_append(bytes)?;
        span.length += appended.length;

        assert!(span.end() == self.text_length);

        Ok(())
    }

    #[must_use]
    pub const fn text_length(&self) -> u32 {
        self.text_length
    }

    pub fn text_copy_to_tail(&mut self, span: Span) -> Result<Span> {
        assert!(span.end() <= self.text_length);

        let offset = self.text_length;

        if span.length > TEXT_BYTES_MAX - offset {
            return Err(Error::TextCapacity { capacity_bytes: TEXT_BYTES_MAX });
        }

        let source = span.offset as usize..span.end() as usize;

        self.text.copy_within(source, offset as usize);
        self.text_length += span.length;

        assert!(self.text_length == offset + span.length);

        Ok(Span { length: span.length, offset })
    }

    pub fn text_node_place(&mut self, parent: u32, run: TextRun) -> Result<()> {
        assert!(parent < self.node_count);
        assert!(run.text.end() <= self.text_length);
        assert!(run.href.end() <= self.text_length);

        let TextRun { href, marks, text } = run;

        if text.is_empty() {
            return Ok(());
        }

        let last = self.node(parent).child_last;

        if last != NODE_NONE {
            let kind_last = self.node(last).kind;

            if let NodeKind::Text { href: href_last, marks: marks_last, text: text_last } =
                kind_last
            {
                let contiguous = text_last.end() == text.offset;
                let mergeable = contiguous && marks_last == marks && href_last == href;

                if mergeable {
                    let merged =
                        Span { length: text_last.length + text.length, offset: text_last.offset };

                    self.node_mut(last).kind = NodeKind::Text { href, marks, text: merged };

                    return Ok(());
                }
            }
        }

        self.node_append(parent, NodeKind::Text { href, marks, text })?;

        Ok(())
    }

    pub fn text_push(&mut self, byte: u8) -> Result<()> {
        assert!(self.text_length >= 1);

        if self.text_length == TEXT_BYTES_MAX {
            return Err(Error::TextCapacity { capacity_bytes: TEXT_BYTES_MAX });
        }

        self.text[self.text_length as usize] = byte;
        self.text_length += 1;

        assert!(self.text_length <= TEXT_BYTES_MAX);

        Ok(())
    }
}

impl fmt::Debug for Document {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Document")
            .field("node_count", &self.node_count)
            .field("text_length", &self.text_length)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Children {
    next: u32,
    remaining: u32,
}

impl Children {
    pub fn next(&mut self, document: &Document) -> Option<u32> {
        assert!(document.node_count() >= 1);

        if self.next == NODE_NONE {
            return None;
        }

        assert!(self.remaining > 0);
        self.remaining -= 1;

        let index = self.next;
        self.next = document.node(index).sibling_next;

        assert!(index != NODE_NONE);

        Some(index)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkEvent {
    Enter(u32),
    Leave(u32),
}

#[derive(Clone, Copy, Debug)]
pub struct Walk {
    current: u32,
    done: bool,
    entering: bool,
    remaining: u32,
    root: u32,
}

impl Walk {
    #[must_use]
    pub fn new(document: &Document, root: u32) -> Self {
        assert!(document.node_count() >= 1);
        assert!(root < document.node_count());

        Self {
            current: root,
            done: false,
            entering: true,
            remaining: document.node_count() * 2,
            root,
        }
    }

    pub fn next(&mut self, document: &Document) -> Option<WalkEvent> {
        assert!(self.root < document.node_count());

        if self.done {
            return None;
        }

        assert!(self.remaining > 0);
        self.remaining -= 1;

        let event = if self.entering {
            WalkEvent::Enter(self.current)
        } else {
            WalkEvent::Leave(self.current)
        };

        let node = document.node(self.current);

        if self.entering {
            if node.child_first == NODE_NONE {
                self.entering = false;
            } else {
                self.current = node.child_first;
            }
        } else if self.current == self.root {
            self.done = true;
        } else if node.sibling_next == NODE_NONE {
            self.current = node.parent;
        } else {
            self.current = node.sibling_next;
            self.entering = true;
        }

        Some(event)
    }

    pub fn skip_children(&mut self, document: &Document) {
        assert!(!self.done);
        assert!(self.current < document.node_count());

        if self.entering {
            let parent = document.node(self.current).parent;
            self.current = parent;
            self.entering = false;
        }

        assert!(!self.entering);
    }
}

#[cfg(test)]
mod tests {
    use super::{DEPTH_MAX, Marks, NODE_ROOT, NodeKind, Span, TextRun, Walk, WalkEvent};
    use crate::error::Error;
    use crate::test_support::with_document;

    #[test]
    fn append_links_siblings_and_children() {
        with_document(|document| {
            let paragraph = document.node_append(NODE_ROOT, NodeKind::Paragraph).unwrap();
            let text = document.text_append(b"hello").unwrap();
            let kind = NodeKind::Text { href: Span::EMPTY, marks: Marks::NONE, text };
            let first = document.node_append(paragraph, kind).unwrap();
            let second = document.node_append(paragraph, NodeKind::HardBreak).unwrap();

            assert_eq!(document.node(NODE_ROOT).child_first, paragraph);
            assert_eq!(document.node(paragraph).child_first, first);
            assert_eq!(document.node(paragraph).child_last, second);
            assert_eq!(document.node(first).sibling_next, second);
            assert_eq!(document.node(second).depth, 2);
            assert_eq!(document.span_bytes(text), b"hello");
        });
    }

    #[test]
    fn walk_visits_enter_then_leave() {
        with_document(|document| {
            let paragraph = document.node_append(NODE_ROOT, NodeKind::Paragraph).unwrap();
            let hard_break = document.node_append(paragraph, NodeKind::HardBreak).unwrap();
            let rule = document.node_append(NODE_ROOT, NodeKind::ThematicBreak).unwrap();

            let expected = [
                WalkEvent::Enter(NODE_ROOT),
                WalkEvent::Enter(paragraph),
                WalkEvent::Enter(hard_break),
                WalkEvent::Leave(hard_break),
                WalkEvent::Leave(paragraph),
                WalkEvent::Enter(rule),
                WalkEvent::Leave(rule),
                WalkEvent::Leave(NODE_ROOT),
            ];

            let mut walk = Walk::new(document, NODE_ROOT);

            for event in expected {
                let seen = walk.next(document);

                assert_eq!(seen, Some(event));
            }

            let seen = walk.next(document);

            assert_eq!(seen, None);
        });
    }

    #[test]
    fn walk_skip_children_leaves_subtree() {
        with_document(|document| {
            let paragraph = document.node_append(NODE_ROOT, NodeKind::Paragraph).unwrap();

            document.node_append(paragraph, NodeKind::HardBreak).unwrap();
            let rule = document.node_append(NODE_ROOT, NodeKind::ThematicBreak).unwrap();
            let mut walk = Walk::new(document, NODE_ROOT);
            let root_enter = walk.next(document);
            let paragraph_enter = walk.next(document);

            assert_eq!(root_enter, Some(WalkEvent::Enter(NODE_ROOT)));
            assert_eq!(paragraph_enter, Some(WalkEvent::Enter(paragraph)));

            walk.skip_children(document);

            let paragraph_leave = walk.next(document);
            let rule_enter = walk.next(document);

            assert_eq!(paragraph_leave, Some(WalkEvent::Leave(paragraph)));
            assert_eq!(rule_enter, Some(WalkEvent::Enter(rule)));
        });
    }

    #[test]
    fn depth_is_bounded() {
        with_document(|document| {
            let mut parent = NODE_ROOT;

            for _ in 0..DEPTH_MAX {
                parent = document.node_append(parent, NodeKind::BlockQuote).unwrap();
            }

            assert_eq!(
                document.node_append(parent, NodeKind::BlockQuote),
                Err(Error::DepthExceeded { depth_max: DEPTH_MAX }),
            );
        });
    }

    #[test]
    fn text_nodes_merge_only_when_contiguous_and_alike() {
        with_document(|document| {
            let paragraph = document.node_append(NODE_ROOT, NodeKind::Paragraph).unwrap();
            let first = document.text_append(b"ab").unwrap();

            document
                .text_node_place(
                    paragraph,
                    TextRun { href: Span::EMPTY, marks: Marks::NONE, text: first },
                )
                .unwrap();

            let second = document.text_append(b"cd").unwrap();

            document
                .text_node_place(
                    paragraph,
                    TextRun { href: Span::EMPTY, marks: Marks::NONE, text: second },
                )
                .unwrap();

            let third = document.text_append(b"ef").unwrap();

            document
                .text_node_place(
                    paragraph,
                    TextRun { href: Span::EMPTY, marks: Marks::STRONG, text: third },
                )
                .unwrap();

            let merged = document.node(paragraph).child_first;

            let NodeKind::Text { text, .. } = document.node(merged).kind else {
                panic!("first child is text");
            };

            assert_eq!(document.span_bytes(text), b"abcd");
            assert_eq!(document.node(merged).sibling_next, document.node(paragraph).child_last);
            assert_eq!(document.node_count(), 4);
        });
    }
}
