use crate::bytes::Sink;
use crate::document::{
    Alignment,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HTMLRaw {
    Allow,
    AllowFiltered,
    Escape,
}

const TAGFILTER_NAMES: [&[u8]; 9] = [
    b"iframe",
    b"noembed",
    b"noframes",
    b"plaintext",
    b"script",
    b"style",
    b"textarea",
    b"title",
    b"xmp",
];

#[derive(Debug)]
struct Writer<'a, 'b, 'c> {
    document: &'c Document,
    href_open: Span,
    marks_open: Marks,
    raw: HTMLRaw,
    sink: &'b mut Sink<'a>,
}

pub fn write(document: &Document, raw: HTMLRaw, output: &mut [u8]) -> Result<usize> {
    assert!(document.node_count() >= 1);
    assert!(document.node(NODE_ROOT).kind == NodeKind::Document);

    let mut sink = Sink::new(output);

    let mut writer =
        Writer { document, href_open: Span::EMPTY, marks_open: Marks::NONE, raw, sink: &mut sink };

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

    assert!(writer.marks_open.is_empty());
    assert!(writer.href_open.is_empty());

    Ok(sink.length())
}

impl Writer<'_, '_, '_> {
    fn paragraph_bare(&self, index: u32) -> bool {
        assert!(index < self.document.node_count());

        let parent = self.document.node(index).parent;

        assert!(parent < self.document.node_count());

        if !matches!(self.document.node(parent).kind, NodeKind::ListItem { .. }) {
            return false;
        }

        let list = self.document.node(parent).parent;

        matches!(self.document.node(list).kind, NodeKind::List { tight: true, .. })
    }

    fn enter(&mut self, index: u32, walk: &mut Walk) -> Result<()> {
        assert!(index < self.document.node_count());

        let node = *self.document.node(index);

        match node.kind {
            NodeKind::Document => Ok(()),
            NodeKind::TableRow { .. } => self.row_open(index),
            NodeKind::Paragraph => {
                if self.paragraph_bare(index) {
                    Ok(())
                } else {
                    self.sink.write(b"<p>")
                }
            }
            NodeKind::Heading { level } => {
                assert!(level >= 1);
                assert!(level <= HEADING_LEVEL_MAX);

                self.sink.write(b"<h")?;
                self.sink.write_u32(u32::from(level))?;

                self.sink.write(b">")
            }
            NodeKind::BlockQuote => self.sink.write(b"<blockquote>\n"),
            NodeKind::List { ordered, start, .. } => self.list_open(ordered, start),
            NodeKind::ListItem { task } => self.item_open(task),
            NodeKind::CodeBlock { language } => {
                self.code_block(index, language)?;
                walk.skip_children(self.document);

                Ok(())
            }
            NodeKind::ThematicBreak => self.sink.write(b"<hr />\n"),
            NodeKind::Table { .. } => self.sink.write(b"<table>\n"),
            NodeKind::TableCell { alignment, colspan, header } => {
                self.cell_open(alignment, colspan, header)
            }
            NodeKind::Text { href, marks, text } => self.text(TextRun { href, marks, text }),
            NodeKind::HardBreak => {
                self.marks_close_all()?;

                self.sink.write(b"<br />\n")
            }
            NodeKind::Image { .. } => self.image(index),
            NodeKind::HTMLBlock { text } => {
                self.raw_write(text)?;

                self.sink.write(b"\n")
            }
            NodeKind::HTMLInline { text } => self.raw_write(text),
            NodeKind::Unused => unreachable!("unused node reached by walk"),
        }
    }

    fn raw_write(&mut self, text: Span) -> Result<()> {
        assert!(text.end() <= self.document.text_length());

        let bytes = self.document.span_bytes(text);

        assert!(bytes.len() == text.length as usize);

        match self.raw {
            HTMLRaw::Escape => text_escape(bytes, self.sink),
            HTMLRaw::Allow => self.sink.write(bytes),
            HTMLRaw::AllowFiltered => tagfilter_write(bytes, self.sink),
        }
    }

    fn leave(&mut self, index: u32) -> Result<()> {
        assert!(index < self.document.node_count());

        let node = *self.document.node(index);

        match node.kind {
            NodeKind::Paragraph => {
                self.marks_close_all()?;

                if self.paragraph_bare(index) {
                    self.sink.write(b"\n")
                } else {
                    self.sink.write(b"</p>\n")
                }
            }
            NodeKind::Heading { level } => {
                assert!(level >= 1);
                assert!(level <= HEADING_LEVEL_MAX);

                self.marks_close_all()?;
                self.sink.write(b"</h")?;
                self.sink.write_u32(u32::from(level))?;

                self.sink.write(b">\n")
            }
            NodeKind::BlockQuote => self.sink.write(b"</blockquote>\n"),
            NodeKind::List { ordered, .. } => {
                self.sink.write(if ordered { b"</ol>\n" } else { b"</ul>\n" })
            }
            NodeKind::ListItem { .. } => self.sink.write(b"</li>\n"),
            NodeKind::Table { .. } => self.sink.write(b"</table>\n"),
            NodeKind::TableRow { .. } => self.row_close(index),
            NodeKind::TableCell { header, .. } => {
                self.marks_close_all()?;

                self.sink.write(if header { b"</th>\n" } else { b"</td>\n" })
            }

            NodeKind::Document
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

    fn list_open(&mut self, ordered: bool, start: u32) -> Result<()> {
        assert!(self.marks_open.is_empty());

        if !ordered {
            return self.sink.write(b"<ul>\n");
        }

        if start == 1 {
            return self.sink.write(b"<ol>\n");
        }

        self.sink.write(b"<ol start=\"")?;
        self.sink.write_u32(start)?;

        self.sink.write(b"\">\n")
    }

    fn item_open(&mut self, task: TaskState) -> Result<()> {
        assert!(self.marks_open.is_empty());
        assert!(self.href_open.is_empty());

        self.sink.write(b"<li>")?;

        match task {
            TaskState::None => Ok(()),
            TaskState::Unchecked => self.sink.write(b"<input disabled=\"\" type=\"checkbox\" /> "),
            TaskState::Checked => {
                self.sink.write(b"<input checked=\"\" disabled=\"\" type=\"checkbox\" /> ")
            }
        }
    }

    fn code_block(&mut self, index: u32, language: Span) -> Result<()> {
        assert!(index < self.document.node_count());
        assert!(language.end() <= self.document.text_length());

        let document = self.document;

        self.sink.write(b"<pre><code")?;

        if !language.is_empty() {
            self.sink.write(b" class=\"language-")?;
            text_escape(document.span_bytes(language), self.sink)?;
            self.sink.write(b"\"")?;
        }

        self.sink.write(b">")?;

        let mut children = document.children(index);
        let mut any = false;

        for _ in 0..document.node_count() {
            let Some(child) = children.next(document) else {
                break;
            };

            if let NodeKind::Text { text, .. } = document.node(child).kind {
                text_escape(document.span_bytes(text), self.sink)?;
                any = any || !text.is_empty();
            }
        }

        if any {
            self.sink.write(b"\n")?;
        }

        self.sink.write(b"</code></pre>\n")
    }

    fn row_is_header(&self, row: u32) -> bool {
        assert!(row < self.document.node_count());
        assert!(matches!(self.document.node(row).kind, NodeKind::TableRow { .. }));

        matches!(self.document.node(row).kind, NodeKind::TableRow { header: true, .. })
    }

    fn row_open(&mut self, row: u32) -> Result<()> {
        assert!(row < self.document.node_count());

        let table = self.document.node(row).parent;
        let first = self.document.node(table).child_first == row;
        let header = self.row_is_header(row);
        let body_starts = !header && self.row_previous_is_header(row);

        assert!(matches!(self.document.node(table).kind, NodeKind::Table { .. }));

        if first {
            self.sink.write(if header { b"<thead>\n" } else { b"<tbody>\n" })?;
        } else if body_starts {
            self.sink.write(b"<tbody>\n")?;
        }

        self.sink.write(b"<tr>\n")
    }

    fn cell_open(&mut self, alignment: Alignment, colspan: u32, header: bool) -> Result<()> {
        assert!(colspan >= 1);
        assert!(self.marks_open.is_empty());

        self.sink.write(if header { b"<th" } else { b"<td" })?;

        let align: &[u8] = match alignment {
            Alignment::None => b"",
            Alignment::Left => b" align=\"left\"",
            Alignment::Center => b" align=\"center\"",
            Alignment::Right => b" align=\"right\"",
        };

        self.sink.write(align)?;

        if colspan > 1 {
            self.sink.write(b" colspan=\"")?;
            self.sink.write_u32(colspan)?;
            self.sink.write(b"\"")?;
        }

        self.sink.write(b">")
    }

    fn row_previous_is_header(&self, row: u32) -> bool {
        assert!(row < self.document.node_count());

        let table = self.document.node(row).parent;
        let mut rows = self.document.children(table);
        let mut previous = NODE_NONE;

        for _ in 0..self.document.node_count() {
            let Some(current) = rows.next(self.document) else {
                break;
            };

            if current == row {
                break;
            }

            previous = current;
        }

        assert!(previous != row);

        previous != NODE_NONE && self.row_is_header(previous)
    }

    fn row_close(&mut self, row: u32) -> Result<()> {
        assert!(row < self.document.node_count());
        assert!(self.marks_open.is_empty());

        self.sink.write(b"</tr>\n")?;

        let next = self.document.node(row).sibling_next;
        let header = self.row_is_header(row);
        let last = next == NODE_NONE;
        let head_ends = header && (last || !self.row_is_header(next));
        let body_ends = !header && last;

        if head_ends {
            self.sink.write(b"</thead>\n")?;
        }

        if body_ends {
            self.sink.write(b"</tbody>\n")?;
        }

        Ok(())
    }

    fn text(&mut self, run: TextRun) -> Result<()> {
        assert!(run.text.end() <= self.document.text_length());
        assert!(run.href.end() <= self.document.text_length());

        let TextRun { href, marks, text } = run;

        if text.is_empty() {
            if href == Span::EMPTY {
                return Ok(());
            }
        }

        let outer = marks.without(Marks::CODE);
        let changed = outer != self.marks_open || href != self.href_open;

        if changed {
            self.marks_transition(outer, href)?;
        }

        if marks.contains(Marks::CODE) {
            self.sink.write(b"<code>")?;
            text_escape(self.document.span_bytes(text), self.sink)?;

            return self.sink.write(b"</code>");
        }

        text_escape(self.document.span_bytes(text), self.sink)
    }

    fn marks_transition(&mut self, marks: Marks, href: Span) -> Result<()> {
        assert!(!marks.contains(Marks::CODE));
        assert!(href.end() <= self.document.text_length());

        let open = self.marks_open;
        let keep_href = href == self.href_open;

        let keep_strong =
            keep_href && marks.contains(Marks::STRONG) == open.contains(Marks::STRONG);

        let keep_emphasis =
            keep_strong && marks.contains(Marks::EMPHASIS) == open.contains(Marks::EMPHASIS);

        let keep_strike = keep_emphasis
            && marks.contains(Marks::STRIKETHROUGH) == open.contains(Marks::STRIKETHROUGH);

        if !keep_strike {
            if open.contains(Marks::STRIKETHROUGH) {
                self.sink.write(b"</del>")?;
            }
        }

        if !keep_emphasis {
            if open.contains(Marks::EMPHASIS) {
                self.sink.write(b"</em>")?;
            }
        }

        if !keep_strong {
            if open.contains(Marks::STRONG) {
                self.sink.write(b"</strong>")?;
            }
        }

        if !keep_href {
            if !self.href_open.is_empty() {
                self.sink.write(b"</a>")?;
            }

            if href != Span::EMPTY {
                self.sink.write(b"<a href=\"")?;
                text_escape(self.document.span_bytes(href), self.sink)?;
                self.sink.write(b"\">")?;
            }
        }

        if !keep_strong {
            if marks.contains(Marks::STRONG) {
                self.sink.write(b"<strong>")?;
            }
        }

        if !keep_emphasis {
            if marks.contains(Marks::EMPHASIS) {
                self.sink.write(b"<em>")?;
            }
        }

        if !keep_strike {
            if marks.contains(Marks::STRIKETHROUGH) {
                self.sink.write(b"<del>")?;
            }
        }

        self.marks_open = marks;
        self.href_open = href;

        Ok(())
    }

    fn marks_close_all(&mut self) -> Result<()> {
        if self.marks_open.is_empty() {
            if self.href_open.is_empty() {
                return Ok(());
            }
        }

        self.marks_transition(Marks::NONE, Span::EMPTY)?;

        assert!(self.marks_open.is_empty());
        assert!(self.href_open.is_empty());

        Ok(())
    }

    fn image(&mut self, index: u32) -> Result<()> {
        assert!(index < self.document.node_count());

        let NodeKind::Image { alt, url } = self.document.node(index).kind else {
            unreachable!("image writer called on a non-image node");
        };

        self.sink.write(b"<img src=\"")?;
        text_escape(self.document.span_bytes(url), self.sink)?;
        self.sink.write(b"\" alt=\"")?;
        text_escape(self.document.span_bytes(alt), self.sink)?;

        self.sink.write(b"\" />")
    }
}

fn text_escape(text: &[u8], sink: &mut Sink<'_>) -> Result<()> {
    let before = sink.length();

    for &byte in text {
        match byte {
            b'&' => sink.write(b"&amp;")?,
            b'<' => sink.write(b"&lt;")?,
            b'>' => sink.write(b"&gt;")?,
            b'"' => sink.write(b"&quot;")?,
            _ => sink.write_byte(byte)?,
        }
    }

    assert!(sink.length() >= before + text.len());

    Ok(())
}

fn tagfilter_write(bytes: &[u8], sink: &mut Sink<'_>) -> Result<()> {
    let before = sink.length();

    for (index, &byte) in bytes.iter().enumerate() {
        let filtered = byte == b'<' && tag_is_filtered(&bytes[index + 1..]);

        if filtered {
            sink.write(b"&lt;")?;
        } else {
            sink.write_byte(byte)?;
        }
    }

    assert!(sink.length() >= before + bytes.len());

    Ok(())
}

fn tag_is_filtered(after_bracket: &[u8]) -> bool {
    let name_start = usize::from(after_bracket.first() == Some(&b'/'));
    let rest = &after_bracket[name_start.min(after_bracket.len())..];

    assert!(name_start <= 1);
    assert!(rest.len() <= after_bracket.len());

    TAGFILTER_NAMES.iter().any(|name| {
        rest.len() >= name.len()
            && rest[..name.len()].eq_ignore_ascii_case(name)
            && rest
                .get(name.len())
                .is_none_or(|&next| matches!(next, b' ' | b'\t' | b'\n' | b'>' | b'/'))
    })
}
