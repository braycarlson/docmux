use crate::bytes::{range_from_u32, range_from_usize, u16_from_usize, u32_from_usize, utf8_encode};
use crate::document::{Document, Marks, NodeKind, Span, TextRun};
use crate::entities::entity_lookup;
use crate::error::{Error, Result};
use crate::markdown::Options;
use crate::workspace::{LinkDefinition, PART_BYTES_MAX};
use crate::xml::entity_decode;
use core::ops::Range;

pub(crate) const INLINE_TOKEN_COUNT_MAX: u32 = 8192;
pub(crate) const LABEL_LENGTH_MAX: usize = 999;
const BRACKET_STACK_MAX: u32 = 256;
const DELIMITER_LITERAL_MAX: u32 = 16;
const ENTITY_NAME_LENGTH_MAX: u32 = 31;
const PARENTHESIS_DEPTH_MAX: u32 = 32;
const SCHEME_LENGTH_MAX: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum InlineTokenKind {
    Autolink = 1,
    Code = 2,
    Delimiter { character: u8 } = 3,
    HardBreak = 4,
    HTMLInline = 5,
    ImageClose = 6,
    ImageOpen = 7,
    LinkClose = 8,
    LinkOpen = 9,
    Removed = 0,
    SoftBreak = 10,
    Text = 11,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InlineToken {
    pub(crate) active: bool,
    pub(crate) can_close: bool,
    pub(crate) can_open: bool,
    pub(crate) count: u16,
    pub(crate) end: u32,
    pub(crate) href: Span,
    pub(crate) kind: InlineTokenKind,
    pub(crate) marks: Marks,
    pub(crate) start: u32,
}

impl InlineToken {
    pub(crate) const EMPTY: Self = Self {
        active: false,
        can_close: false,
        can_open: false,
        count: 0,
        end: 0,
        href: Span::EMPTY,
        kind: InlineTokenKind::Removed,
        marks: Marks::NONE,
        start: 0,
    };

    fn new(kind: InlineTokenKind, range: Range<usize>) -> Self {
        assert!(range.start <= range.end);

        Self {
            end: u32_from_usize(range.end),
            kind,
            start: u32_from_usize(range.start),
            ..Self::EMPTY
        }
    }

    fn range(self) -> Range<usize> {
        assert!(self.start <= self.end);

        let range = self.start as usize..self.end as usize;

        assert!(range.len() == (self.end - self.start) as usize);

        range
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Destination {
    pub(crate) after: u32,
    pub(crate) inner: Range<u32>,
}

#[derive(Clone, Copy, Debug)]
struct Neighbours {
    after: Option<char>,
    before: Option<char>,
}

pub(crate) fn parse(
    source: &[u8],
    options: Options,
    tokens: &mut [InlineToken],
    definitions: &[LinkDefinition],
    document: &mut Document,
    parent: u32,
) -> Result<()> {
    assert!(tokens.len() == INLINE_TOKEN_COUNT_MAX as usize);
    assert!(parent < document.node_count());

    let count = tokenize(source, options, tokens, definitions, document)? as usize;

    emphasis_process(&mut tokens[..count]);

    emit(source, &tokens[..count], document, parent)
}

#[derive(Debug)]
struct Tokenizer<'a> {
    bracket_count: u32,
    brackets: [u32; BRACKET_STACK_MAX as usize],
    count: u32,
    definitions: &'a [LinkDefinition],
    position: u32,
    source: &'a [u8],
    text_start: u32,
}

fn tokenize(
    source: &[u8],
    options: Options,
    tokens: &mut [InlineToken],
    definitions: &[LinkDefinition],
    document: &mut Document,
) -> Result<u32> {
    assert!(source.len() <= PART_BYTES_MAX as usize);
    assert!(tokens.len() == INLINE_TOKEN_COUNT_MAX as usize);

    let mut tokenizer = Tokenizer {
        bracket_count: 0,
        brackets: [0; BRACKET_STACK_MAX as usize],
        count: 0,
        definitions,
        position: 0,
        source,
        text_start: 0,
    };

    for _ in 0..source.len() {
        if tokenizer.position as usize >= source.len() {
            break;
        }

        let byte = source[tokenizer.position as usize];

        let consumed = match byte {
            b'\\' => tokenizer.backslash(tokens)?,
            b'`' => tokenizer.backtick(tokens)?,
            b'*' | b'_' => tokenizer.delimiter(tokens, byte)?,
            b'~' => {
                options.contains(Options::STRIKETHROUGH) && tokenizer.delimiter(tokens, byte)?
            }
            b'[' => tokenizer.bracket_open(tokens, false)?,
            b'!' => tokenizer.bracket_open(tokens, true)?,
            b']' => tokenizer.bracket_close(tokens, document)?,
            b'<' => tokenizer.angle_bracket(tokens, document)?,
            b'\n' => tokenizer.line_ending(tokens)?,
            b'h' | b'w' | b'f' => {
                options.contains(Options::AUTOLINKS_RAW)
                    && tokenizer.raw_autolink(tokens, document)?
            }
            b'@' => {
                options.contains(Options::AUTOLINKS_RAW)
                    && tokenizer.email_autolink(tokens, document)?
            }
            _ => false,
        };

        if !consumed {
            tokenizer.position += 1;
        }
    }

    tokenizer.text_flush(tokens)?;
    tokenizer.brackets_deactivate_all(tokens);

    assert!(tokenizer.position as usize == source.len());
    assert!(tokenizer.count as usize <= tokens.len());

    Ok(tokenizer.count)
}

impl Tokenizer<'_> {
    fn append(&mut self, tokens: &mut [InlineToken], token: InlineToken) -> Result<usize> {
        assert!(self.count as usize <= tokens.len());
        assert!(token.end as usize <= self.source.len());

        if self.count as usize >= tokens.len() {
            return Err(Error::InlineTokenCapacity { token_count_max: INLINE_TOKEN_COUNT_MAX });
        }

        tokens[self.count as usize] = token;
        self.count += 1;
        let index = self.count as usize - 1;

        assert!(index < tokens.len());

        Ok(index)
    }

    fn text_flush(&mut self, tokens: &mut [InlineToken]) -> Result<()> {
        assert!(self.text_start <= self.position);

        if self.text_start < self.position {
            let token = InlineToken::new(
                InlineTokenKind::Text,
                self.text_start as usize..self.position as usize,
            );

            self.append(tokens, token)?;
        }

        self.text_start = self.position;

        assert!(self.text_start == self.position);

        Ok(())
    }

    fn token_emit(&mut self, tokens: &mut [InlineToken], token: InlineToken) -> Result<bool> {
        assert!(token.start as usize == self.position as usize);
        assert!(token.end as usize <= self.source.len());

        self.text_flush(tokens)?;
        self.append(tokens, token)?;
        self.position = token.end;
        self.text_start = self.position;

        Ok(true)
    }

    fn backslash(&mut self, tokens: &mut [InlineToken]) -> Result<bool> {
        assert!(self.source[self.position as usize] == b'\\');
        assert!(self.count as usize <= tokens.len());

        let next = self.source.get(self.position as usize + 1).copied();

        match next {
            Some(b'\n') => {
                let start = self.position as usize;
                let token = InlineToken::new(InlineTokenKind::HardBreak, start..start + 2);

                self.token_emit(tokens, token)?;
                self.line_start_skip();

                Ok(true)
            }
            Some(byte) if byte.is_ascii_punctuation() => {
                self.position += 2;

                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn line_ending(&mut self, tokens: &mut [InlineToken]) -> Result<bool> {
        assert!(self.source[self.position as usize] == b'\n');
        assert!(self.text_start <= self.position);

        let line_end = self.position as usize;

        let trailing_spaces = self.source[self.text_start as usize..line_end]
            .iter()
            .rev()
            .take_while(|&&byte| byte == b' ')
            .count();

        let hard = trailing_spaces >= 2;
        let text_end = line_end - trailing_spaces;

        if (self.text_start as usize) < text_end {
            let token = InlineToken::new(InlineTokenKind::Text, self.text_start as usize..text_end);

            self.append(tokens, token)?;
        }

        let kind = if hard { InlineTokenKind::HardBreak } else { InlineTokenKind::SoftBreak };
        let token = InlineToken::new(kind, text_end..line_end + 1);

        self.append(tokens, token)?;
        self.position = u32_from_usize(line_end + 1);
        self.text_start = self.position;

        self.line_start_skip();

        Ok(true)
    }

    fn line_start_skip(&mut self) {
        assert!(self.position as usize <= self.source.len());

        let rest = &self.source[self.position as usize..];
        let skipped = rest.iter().take_while(|&&byte| byte == b' ' || byte == b'\t').count();
        self.position += u32_from_usize(skipped);
        self.text_start = self.position;

        assert!(self.position as usize <= self.source.len());
    }

    fn backtick(&mut self, tokens: &mut [InlineToken]) -> Result<bool> {
        assert!(self.source[self.position as usize] == b'`');
        assert!(self.count as usize <= tokens.len());

        let rest = &self.source[self.position as usize..];
        let run = rest.iter().take_while(|&&byte| byte == b'`').count();

        if u16::try_from(run).is_err() {
            self.position += u32_from_usize(run);

            return Ok(true);
        }

        let content_start = self.position as usize + run;

        let Some(content_length) = backtick_closer_find(&self.source[content_start..], run) else {
            self.position += u32_from_usize(run);

            return Ok(true);
        };

        let content_end = content_start + content_length;

        let token = InlineToken {
            count: u16_from_usize(run),
            ..InlineToken::new(InlineTokenKind::Code, content_start..content_end)
        };

        self.text_flush(tokens)?;
        self.append(tokens, token)?;
        self.position = u32_from_usize(content_end + run);
        self.text_start = self.position;

        Ok(true)
    }

    fn delimiter(&mut self, tokens: &mut [InlineToken], character: u8) -> Result<bool> {
        assert!(self.source[self.position as usize] == character);
        assert!(matches!(character, b'*' | b'_' | b'~'));

        let rest = &self.source[self.position as usize..];
        let run = rest.iter().take_while(|&&byte| byte == character).count();

        if u16::try_from(run).is_err() {
            self.position += u32_from_usize(run);

            return Ok(true);
        }

        if character == b'~' {
            if run > 2 {
                self.position += u32_from_usize(run);

                return Ok(true);
            }
        }

        let neighbours = Neighbours {
            after: char_after(self.source, self.position as usize + run),
            before: char_before(self.source, self.position as usize),
        };

        let (can_open, can_close) = flanking(character, neighbours);

        let token = InlineToken {
            active: true,
            can_close,
            can_open,
            count: u16_from_usize(run),
            ..InlineToken::new(
                InlineTokenKind::Delimiter { character },
                self.position as usize..self.position as usize + run,
            )
        };

        self.token_emit(tokens, token)
    }

    fn bracket_open(&mut self, tokens: &mut [InlineToken], image: bool) -> Result<bool> {
        assert!(self.bracket_count as usize <= self.brackets.len());
        assert!(self.count as usize <= tokens.len());

        let length = if image { 2 } else { 1 };

        if image {
            if self.source.get(self.position as usize + 1) != Some(&b'[') {
                return Ok(false);
            }
        }

        if self.bracket_count as usize >= self.brackets.len() {
            return Ok(false);
        }

        let kind = if image { InlineTokenKind::ImageOpen } else { InlineTokenKind::LinkOpen };

        let token = InlineToken {
            active: true,
            ..InlineToken::new(kind, self.position as usize..self.position as usize + length)
        };

        self.text_flush(tokens)?;

        let index = self.append(tokens, token)?;
        self.position = token.end;
        self.text_start = self.position;
        self.brackets[self.bracket_count as usize] = u32_from_usize(index);
        self.bracket_count += 1;

        Ok(true)
    }

    fn bracket_close(
        &mut self,
        tokens: &mut [InlineToken],
        document: &mut Document,
    ) -> Result<bool> {
        assert!(self.source[self.position as usize] == b']');
        assert!(self.bracket_count as usize <= self.brackets.len());

        if self.bracket_count == 0 {
            return Ok(false);
        }

        self.bracket_count -= 1;
        let opener = self.brackets[self.bracket_count as usize] as usize;
        let image = tokens[opener].kind == InlineTokenKind::ImageOpen;

        if !tokens[opener].active {
            tokens[opener].kind = InlineTokenKind::Text;

            return Ok(false);
        }

        self.text_flush(tokens)?;

        let close_start = self.position as usize;

        let Some((href, resolved_end)) = self.link_resolve(tokens, opener, document)? else {
            tokens[opener].kind = InlineTokenKind::Text;

            return Ok(false);
        };

        tokens[opener].href = href;
        let kind = if image { InlineTokenKind::ImageClose } else { InlineTokenKind::LinkClose };
        let token = InlineToken::new(kind, close_start..resolved_end);
        let closer = self.append(tokens, token)?;
        self.position = u32_from_usize(resolved_end);
        self.text_start = self.position;

        emphasis_process(&mut tokens[opener + 1..closer]);

        for inner in &mut tokens[opener + 1..closer] {
            if let InlineTokenKind::Delimiter { .. } = inner.kind {
                inner.can_close = false;
                inner.can_open = false;
            }
        }

        if !image {
            self.brackets_deactivate_links(tokens);
        }

        Ok(true)
    }

    fn brackets_deactivate_links(&self, tokens: &mut [InlineToken]) {
        assert!(self.bracket_count as usize <= self.brackets.len());
        assert!(self.count as usize <= tokens.len());

        for &index in &self.brackets[..self.bracket_count as usize] {
            if tokens[index as usize].kind == InlineTokenKind::LinkOpen {
                tokens[index as usize].active = false;
            }
        }
    }

    fn brackets_deactivate_all(&mut self, tokens: &mut [InlineToken]) {
        assert!(self.bracket_count as usize <= self.brackets.len());
        assert!(self.count as usize <= tokens.len());

        for &index in &self.brackets[..self.bracket_count as usize] {
            tokens[index as usize].kind = InlineTokenKind::Text;
        }

        self.bracket_count = 0;
    }

    fn link_resolve(
        &self,
        tokens: &[InlineToken],
        opener: usize,
        document: &mut Document,
    ) -> Result<Option<(Span, usize)>> {
        assert!(opener < self.count as usize);
        assert!(self.source[self.position as usize] == b']');

        let after_bracket = self.position as usize + 1;

        if let Some((href, end)) = inline_destination_parse(self.source, after_bracket, document)? {
            return Ok(Some((href, end)));
        }

        let text_range = tokens[opener].end as usize..self.position as usize;

        let (label_range, end) = match reference_label_parse(self.source, after_bracket) {
            Some((range, end)) if !range.is_empty() => (range, end),
            Some((_, end)) => (text_range, end),
            None => (text_range, after_bracket),
        };

        let mut normalized = [0u8; LABEL_LENGTH_MAX];

        let Some(label_length) = label_normalize(&self.source[label_range], &mut normalized) else {
            return Ok(None);
        };

        for definition in self.definitions {
            if document.span_bytes(definition.label) == &normalized[..label_length] {
                return Ok(Some((definition.url, end)));
            }
        }

        Ok(None)
    }

    fn angle_bracket(
        &mut self,
        tokens: &mut [InlineToken],
        document: &mut Document,
    ) -> Result<bool> {
        assert!(self.source[self.position as usize] == b'<');
        assert!(self.count as usize <= tokens.len());

        if self.angle_autolink(tokens, document)? {
            return Ok(true);
        }

        self.html_inline(tokens)
    }

    fn angle_autolink(
        &mut self,
        tokens: &mut [InlineToken],
        document: &mut Document,
    ) -> Result<bool> {
        assert!(self.source[self.position as usize] == b'<');
        assert!(document.node_count() >= 1);

        let rest = &self.source[self.position as usize + 1..];

        let Some(length) = rest.iter().position(|&byte| byte == b'>') else {
            return Ok(false);
        };

        let body = &rest[..length];

        if body.iter().any(|&byte| byte == b' ' || byte == b'<' || byte.is_ascii_control()) {
            return Ok(false);
        }

        let is_url = scheme_length(body).is_some();
        let is_email = !is_url && email_is_plausible(body);
        let is_link = is_url || is_email;

        if !is_link {
            return Ok(false);
        }

        let href = if is_email {
            let mut href = document.text_append(b"mailto:")?;

            document.text_extend(&mut href, body)?;

            href
        } else {
            document.text_append(body)?
        };

        let start = self.position as usize + 1;

        let token = InlineToken {
            href,
            ..InlineToken::new(InlineTokenKind::Autolink, start..start + length)
        };

        self.text_flush(tokens)?;
        self.append(tokens, token)?;
        self.position = u32_from_usize(start + length + 1);
        self.text_start = self.position;

        Ok(true)
    }

    fn email_autolink(
        &mut self,
        tokens: &mut [InlineToken],
        document: &mut Document,
    ) -> Result<bool> {
        assert!(self.source[self.position as usize] == b'@');
        assert!(document.node_count() >= 1);

        if self.bracket_count > 0 {
            return Ok(false);
        }

        let Some(domain_length) = email_domain_length(&self.source[self.position as usize + 1..])
        else {
            return Ok(false);
        };

        let Some(start) = self.email_local_start(tokens) else {
            return Ok(false);
        };

        let end = self.position as usize + 1 + domain_length;

        if start > self.text_start as usize {
            let token = InlineToken::new(InlineTokenKind::Text, self.text_start as usize..start);

            self.append(tokens, token)?;
        }

        let mut href = document.text_append(b"mailto:")?;

        document.text_extend(&mut href, &self.source[start..end])?;
        let token = InlineToken { href, ..InlineToken::new(InlineTokenKind::Autolink, start..end) };

        self.append(tokens, token)?;
        self.position = u32_from_usize(end);
        self.text_start = u32_from_usize(end);

        Ok(true)
    }

    fn email_local_start(&mut self, tokens: &mut [InlineToken]) -> Option<usize> {
        assert!(self.text_start <= self.position);
        assert!(self.count as usize <= tokens.len());

        let run = &self.source[self.text_start as usize..self.position as usize];
        let run_local = run.iter().rev().take_while(|&&byte| email_local_ok(byte)).count();
        let mut start = self.position as usize - run_local;

        if run_local < run.len() {
            return if run_local == 0 { None } else { Some(start) };
        }

        for index in (0..self.count as usize).rev() {
            let token = tokens[index];

            if token.end as usize != start {
                break;
            }

            match token.kind {
                InlineTokenKind::Delimiter { character: b'_' } => {
                    tokens[index].kind = InlineTokenKind::Removed;
                    start = token.start as usize;
                    self.text_start = u32_from_usize(start);
                }
                InlineTokenKind::Text => {
                    let text = &self.source[token.range()];
                    let local = text.iter().rev().take_while(|&&byte| email_local_ok(byte)).count();
                    start -= local;

                    if local == text.len() {
                        tokens[index].kind = InlineTokenKind::Removed;
                        self.text_start = u32_from_usize(start);
                    } else {
                        tokens[index].end = u32_from_usize(start);
                        self.text_start = u32_from_usize(start);

                        break;
                    }
                }

                InlineTokenKind::Delimiter { .. }
                | InlineTokenKind::Autolink
                | InlineTokenKind::Code
                | InlineTokenKind::HardBreak
                | InlineTokenKind::HTMLInline
                | InlineTokenKind::ImageClose
                | InlineTokenKind::ImageOpen
                | InlineTokenKind::LinkClose
                | InlineTokenKind::LinkOpen
                | InlineTokenKind::Removed
                | InlineTokenKind::SoftBreak => break,
            }
        }

        if start == self.position as usize { None } else { Some(start) }
    }

    fn html_inline(&mut self, tokens: &mut [InlineToken]) -> Result<bool> {
        assert!(self.source[self.position as usize] == b'<');
        assert!(self.count as usize <= tokens.len());

        let Some(length) = html_tag_length(self.source, self.position as usize) else {
            return Ok(false);
        };

        let token = InlineToken::new(
            InlineTokenKind::HTMLInline,
            self.position as usize..self.position as usize + length,
        );

        self.token_emit(tokens, token)
    }

    fn raw_autolink(
        &mut self,
        tokens: &mut [InlineToken],
        document: &mut Document,
    ) -> Result<bool> {
        assert!(matches!(self.source[self.position as usize], b'h' | b'w' | b'f'));
        assert!(document.node_count() >= 1);

        let before = if self.position as usize == 0 {
            b' '
        } else {
            self.source[self.position as usize - 1]
        };

        if !(before.is_ascii_whitespace() || matches!(before, b'*' | b'_' | b'~' | b'(')) {
            return Ok(false);
        }

        if self.bracket_count > 0 {
            return Ok(false);
        }

        let rest = &self.source[self.position as usize..];
        let www = rest.starts_with(b"www.");

        let http = rest.starts_with(b"http://")
            || rest.starts_with(b"https://")
            || rest.starts_with(b"ftp://");

        let scheme_seen = www || http;

        if !scheme_seen {
            return Ok(false);
        }

        let Some(length) = raw_autolink_length(rest) else {
            return Ok(false);
        };

        let href = if www {
            let mut href = document.text_append(b"http://")?;

            document.text_extend(&mut href, &rest[..length])?;

            href
        } else {
            document.text_append(&rest[..length])?
        };

        let token = InlineToken {
            href,
            ..InlineToken::new(
                InlineTokenKind::Autolink,
                self.position as usize..self.position as usize + length,
            )
        };

        self.token_emit(tokens, token)
    }
}

fn backtick_closer_find(rest: &[u8], run: usize) -> Option<usize> {
    assert!(run >= 1);

    let mut index = 0usize;

    for _ in 0..rest.len() {
        if index >= rest.len() {
            return None;
        }

        if rest[index] != b'`' {
            index += 1;

            continue;
        }

        let candidate = rest[index..].iter().take_while(|&&byte| byte == b'`').count();

        if candidate == run {
            return Some(index);
        }

        index += candidate;
    }

    None
}

fn char_before(source: &[u8], index: usize) -> Option<char> {
    assert!(index <= source.len());

    let start = (index.saturating_sub(4)..index)
        .rev()
        .find(|&position| source[position] & 0xC0 != 0x80)
        .unwrap_or(index);

    if start == index {
        return None;
    }

    assert!(index - start <= 4);

    str::from_utf8(&source[start..index]).ok()?.chars().next()
}

fn char_after(source: &[u8], index: usize) -> Option<char> {
    assert!(index <= source.len());

    let first = *source.get(index)?;

    let length = match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    };

    let end = (index + length).min(source.len());

    assert!(end > index);

    str::from_utf8(&source[index..end]).ok()?.chars().next()
}

fn is_punctuation_char(character: char) -> bool {
    let code = u32::from(character);

    assert!(code <= 0x10_FFFF);

    character.is_ascii_punctuation()
        || (0xA1..=0xBF).contains(&code)
        || code == 0xD7
        || code == 0xF7
        || (0x2010..=0x2027).contains(&code)
        || (0x2030..=0x205E).contains(&code)
        || (0x20A0..=0x20CF).contains(&code)
        || (0x2100..=0x214F).contains(&code)
        || (0x2190..=0x23FF).contains(&code)
        || (0x2500..=0x27BF).contains(&code)
        || (0x2900..=0x2BFF).contains(&code)
        || (0x3000..=0x303F).contains(&code)
        || (0xFE30..=0xFE4F).contains(&code)
        || (0xFF01..=0xFF0F).contains(&code)
        || (0xFF1A..=0xFF20).contains(&code)
        || (0xFF3B..=0xFF40).contains(&code)
        || (0xFF5B..=0xFF65).contains(&code)
}

fn is_punctuation_or_space(character: Option<char>) -> bool {
    character.is_none_or(|character| character.is_whitespace() || is_punctuation_char(character))
}

fn flanking(character: u8, neighbours: Neighbours) -> (bool, bool) {
    assert!(matches!(character, b'*' | b'_' | b'~'));

    let after_space = neighbours.after.is_none_or(char::is_whitespace);
    let after_punctuation = neighbours.after.is_some_and(is_punctuation_char);
    let before_space = neighbours.before.is_none_or(char::is_whitespace);
    let before_punctuation = neighbours.before.is_some_and(is_punctuation_char);
    let left_flanking = !after_space && (!after_punctuation || before_space || before_punctuation);
    let right_flanking = !before_space && (!before_punctuation || after_space || after_punctuation);

    assert!(!left_flanking || !after_space);

    if character == b'_' {
        let can_open =
            left_flanking && (!right_flanking || is_punctuation_or_space(neighbours.before));

        let can_close =
            right_flanking && (!left_flanking || is_punctuation_or_space(neighbours.after));

        (can_open, can_close)
    } else {
        (left_flanking, right_flanking)
    }
}

fn scheme_length(body: &[u8]) -> Option<usize> {
    let colon = body.iter().position(|&byte| byte == b':')?;

    if !(2..=SCHEME_LENGTH_MAX as usize).contains(&colon) {
        return None;
    }

    if !body[0].is_ascii_alphabetic() {
        return None;
    }

    assert!(colon < body.len());
    assert!(colon >= 2);

    let valid = body[..colon]
        .iter()
        .all(|&byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-'));

    valid.then_some(colon)
}

fn email_is_plausible(body: &[u8]) -> bool {
    let Some(at) = body.iter().position(|&byte| byte == b'@') else {
        return false;
    };

    if at == 0 {
        return false;
    }

    if at + 1 >= body.len() {
        return false;
    }

    assert!(at >= 1);
    assert!(at + 1 < body.len());

    body[at + 1..].contains(&b'.') && !body.contains(&b'\\')
}

const fn email_local_ok(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+')
}

const fn email_domain_ok(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
}

fn email_domain_length(rest: &[u8]) -> Option<usize> {
    let mut length = rest.iter().take_while(|&&byte| email_domain_ok(byte)).count();

    if length > 0 {
        if rest[length - 1] == b'.' {
            length -= 1;
        }
    }

    let domain = &rest[..length];

    if !domain.contains(&b'.') {
        return None;
    }

    if matches!(domain[length - 1], b'-' | b'_') {
        return None;
    }

    assert!(length >= 1);

    Some(length)
}

fn raw_autolink_length(rest: &[u8]) -> Option<usize> {
    let mut length = rest
        .iter()
        .position(|&byte| byte.is_ascii_whitespace() || byte == b'<')
        .unwrap_or(rest.len());

    for _ in 0..rest.len() {
        if length == 0 {
            break;
        }

        let last = rest[length - 1];

        let punctuation =
            matches!(last, b'?' | b'!' | b'.' | b',' | b':' | b'*' | b'_' | b'~' | b'\'' | b'"');

        if punctuation {
            length -= 1;

            continue;
        }

        if last == b')' {
            let open = byte_count(&rest[..length], b'(');
            let close = byte_count(&rest[..length], b')');

            if close > open {
                length -= 1;

                continue;
            }
        }

        break;
    }

    if let Some(ampersand) = rest[..length].iter().rposition(|&byte| byte == b'&') {
        let tail = &rest[ampersand..length];

        let entity_like = tail.len() > 2
            && tail[tail.len() - 1] == b';'
            && tail[1..tail.len() - 1].iter().all(u8::is_ascii_alphanumeric);

        if entity_like {
            length = ampersand;
        }
    }

    let scheme_end = if rest.starts_with(b"www.") {
        0
    } else {
        rest.iter().position(|&byte| byte == b':').map_or(0, |colon| colon + 3)
    };

    let host = &rest[scheme_end.min(length)..length];

    assert!(length <= rest.len());

    host.contains(&b'.').then_some(length)
}

fn byte_count(haystack: &[u8], needle: u8) -> usize {
    let mut count = 0usize;

    for &byte in haystack {
        if byte == needle {
            count += 1;
        }
    }

    assert!(count <= haystack.len());

    count
}

fn inline_destination_parse(
    source: &[u8],
    start: usize,
    document: &mut Document,
) -> Result<Option<(Span, usize)>> {
    assert!(start <= source.len());

    if source.get(start) != Some(&b'(') {
        return Ok(None);
    }

    let mut position = whitespace_skip(source, start + 1);

    let Some(destination) = destination_scan(source, position) else {
        return Ok(None);
    };

    position = whitespace_skip(source, destination.after as usize);

    if let Some(title_end) = title_scan(source, position) {
        position = whitespace_skip(source, title_end);
    }

    if source.get(position) != Some(&b')') {
        return Ok(None);
    }

    let raw = &source[range_from_u32(&destination.inner)];
    let mut href = Span { length: 0, offset: document.text_length() };

    text_decode(raw, document, &mut href)?;

    assert!(href.end() == document.text_length());

    Ok(Some((href, position + 1)))
}

fn whitespace_skip(source: &[u8], start: usize) -> usize {
    assert!(start <= source.len());

    let rest = &source[start..];
    let end = start + rest.iter().take_while(|&&byte| byte.is_ascii_whitespace()).count();

    assert!(end <= source.len());

    end
}

fn destination_scan_angle(source: &[u8], start: usize) -> Option<Destination> {
    assert!(source.get(start) == Some(&b'<'));

    let rest = &source[start + 1..];
    let mut escaped = false;

    for (index, &byte) in rest.iter().enumerate() {
        if escaped {
            escaped = false;

            continue;
        }

        match byte {
            b'\\' => escaped = true,
            b'>' => {
                let inner = range_from_usize(&(start + 1..start + 1 + index));

                return Some(Destination { after: u32_from_usize(start + 2 + index), inner });
            }
            b'\n' | b'<' => return None,
            _ => {}
        }
    }

    None
}

pub(crate) fn destination_scan(source: &[u8], start: usize) -> Option<Destination> {
    assert!(start <= source.len());

    if source.get(start) == Some(&b'<') {
        return destination_scan_angle(source, start);
    }

    let mut depth: u32 = 0;
    let mut end = start;
    let mut escaped = false;

    for (index, &byte) in source.iter().enumerate().skip(start) {
        if escaped {
            escaped = false;
            end = index + 1;

            continue;
        }

        if byte == b'\\' {
            escaped = true;
            end = index + 1;

            continue;
        }

        if byte.is_ascii_whitespace() {
            break;
        }

        if byte.is_ascii_control() {
            break;
        }

        if byte == b'(' {
            depth += 1;

            if depth > PARENTHESIS_DEPTH_MAX {
                return None;
            }
        } else if byte == b')' {
            if depth == 0 {
                break;
            }

            depth -= 1;
        }

        end = index + 1;
    }

    if depth != 0 {
        return None;
    }

    assert!(end >= start);

    Some(Destination { after: u32_from_usize(end), inner: range_from_usize(&(start..end)) })
}

pub(crate) fn title_scan(source: &[u8], start: usize) -> Option<usize> {
    assert!(start <= source.len());

    let open = *source.get(start)?;

    let close = match open {
        b'"' => b'"',
        b'\'' => b'\'',
        b'(' => b')',
        _ => return None,
    };

    let mut escaped = false;

    for (index, &byte) in source.iter().enumerate().skip(start + 1) {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == close {
            assert!(index > start);

            return Some(index + 1);
        }
    }

    None
}

fn reference_label_parse(source: &[u8], start: usize) -> Option<(Range<usize>, usize)> {
    assert!(start <= source.len());

    if source.get(start) != Some(&b'[') {
        return None;
    }

    let mut escaped = false;

    for (index, &byte) in source.iter().enumerate().skip(start + 1) {
        if index > start + LABEL_LENGTH_MAX + 1 {
            return None;
        }

        if escaped {
            escaped = false;

            continue;
        }

        match byte {
            b'\\' => escaped = true,
            b']' => {
                assert!(index > start);

                return Some((start + 1..index, index + 1));
            }
            b'[' => return None,
            _ => {}
        }
    }

    None
}

pub(crate) fn label_normalize(label: &[u8], out: &mut [u8; LABEL_LENGTH_MAX]) -> Option<usize> {
    let text = str::from_utf8(label).ok()?.trim();
    let mut length = 0usize;
    let mut pending_space = false;

    for character in text.chars() {
        if character.is_whitespace() {
            pending_space = true;

            continue;
        }

        if pending_space {
            if length >= LABEL_LENGTH_MAX {
                return None;
            }

            out[length] = b' ';
            length += 1;
            pending_space = false;
        }

        let mut folded = ['\0'; 3];

        let folded_length = if matches!(character, '\u{DF}' | '\u{1E9E}') {
            folded[..2].copy_from_slice(&['s', 's']);

            2
        } else {
            let mut count = 0usize;

            for lower in character.to_lowercase().take(3) {
                folded[count] = lower;
                count += 1;
            }

            count
        };

        assert!(folded_length >= 1);
        assert!(folded_length <= 3);

        for &lower in &folded[..folded_length] {
            let mut encoded = [0u8; 4];
            let encoded_length = lower.encode_utf8(&mut encoded).len();

            if length + encoded_length > LABEL_LENGTH_MAX {
                return None;
            }

            out[length..length + encoded_length].copy_from_slice(&encoded[..encoded_length]);
            length += encoded_length;
        }
    }

    assert!(length <= LABEL_LENGTH_MAX);

    if length == 0 { None } else { Some(length) }
}

fn emphasis_process(tokens: &mut [InlineToken]) {
    let mut closer = 0usize;

    let iterations_max =
        tokens.len() * 2 + tokens.iter().map(|token| usize::from(token.count)).sum::<usize>();

    assert!(iterations_max >= tokens.len());

    for _ in 0..iterations_max {
        if closer >= tokens.len() {
            break;
        }

        let closer_token = tokens[closer];

        let InlineTokenKind::Delimiter { character } = closer_token.kind else {
            closer += 1;

            continue;
        };

        let closer_active = closer_token.can_close && closer_token.count > 0;

        if !closer_active {
            closer += 1;

            continue;
        }

        match opener_find(tokens, closer, character) {
            Some(opener) => emphasis_pair(tokens, opener..closer, character),
            None => closer += 1,
        }
    }

    assert!(closer <= tokens.len());
}

fn opener_find(tokens: &[InlineToken], closer: usize, character: u8) -> Option<usize> {
    assert!(closer < tokens.len());
    assert!(matches!(character, b'*' | b'_' | b'~'));

    let closer_token = tokens[closer];

    assert!(closer_token.can_close);

    for opener in (0..closer).rev() {
        let opener_token = tokens[opener];

        if opener_token.kind != (InlineTokenKind::Delimiter { character }) {
            continue;
        }

        if !opener_token.can_open {
            continue;
        }

        if opener_token.count == 0 {
            continue;
        }

        if character == b'~' {
            if opener_token.count == closer_token.count {
                return Some(opener);
            }

            continue;
        }

        let odd_match = (closer_token.can_open || opener_token.can_close)
            && (opener_token.count + closer_token.count).is_multiple_of(3)
            && !(opener_token.count.is_multiple_of(3) && closer_token.count.is_multiple_of(3));

        if odd_match {
            continue;
        }

        return Some(opener);
    }

    None
}

fn emphasis_pair(tokens: &mut [InlineToken], pair: Range<usize>, character: u8) {
    assert!(pair.start < pair.end);
    assert!(pair.end < tokens.len());
    assert!(matches!(character, b'*' | b'_' | b'~'));

    let opener = pair.start;
    let closer = pair.end;
    let both_double = tokens[opener].count >= 2 && tokens[closer].count >= 2;

    let use_count = if character == b'~' {
        tokens[opener].count
    } else if both_double {
        2
    } else {
        1
    };

    let mark = if character == b'~' {
        Marks::STRIKETHROUGH
    } else if use_count == 2 {
        Marks::STRONG
    } else {
        Marks::EMPHASIS
    };

    for token in &mut tokens[opener + 1..closer] {
        if matches!(token.kind, InlineTokenKind::Delimiter { .. }) {
            token.can_close = false;
            token.can_open = false;
        }

        token.marks = token.marks.union(mark);
    }

    tokens[opener].count -= use_count;
    tokens[closer].count -= use_count;

    assert!(use_count >= 1);
}

fn emit(source: &[u8], tokens: &[InlineToken], document: &mut Document, parent: u32) -> Result<()> {
    assert!(parent < document.node_count());

    let mut href = Span::EMPTY;
    let mut index = 0usize;
    let mut link_text_seen = false;

    for _ in 0..tokens.len() {
        if index >= tokens.len() {
            break;
        }

        let token = tokens[index];
        index += 1;

        if !matches!(
            token.kind,
            InlineTokenKind::LinkOpen | InlineTokenKind::LinkClose | InlineTokenKind::Removed,
        ) {
            link_text_seen = true;
        }

        match token.kind {
            InlineTokenKind::LinkOpen => {
                href = token.href;
                link_text_seen = false;
            }
            InlineTokenKind::LinkClose => {
                if !link_text_seen {
                    if href != Span::EMPTY {
                        let kind = NodeKind::Text { href, marks: token.marks, text: Span::EMPTY };

                        document.node_append(parent, kind)?;
                    }
                }

                href = Span::EMPTY;
            }
            InlineTokenKind::ImageOpen => {
                index = image_emit(source, tokens, index, token.href, document, parent)?;
            }
            InlineTokenKind::ImageClose | InlineTokenKind::Removed => {}
            InlineTokenKind::Text
            | InlineTokenKind::Code
            | InlineTokenKind::Autolink
            | InlineTokenKind::HTMLInline
            | InlineTokenKind::SoftBreak
            | InlineTokenKind::HardBreak
            | InlineTokenKind::Delimiter { .. } => {
                token_emit(source, token, href, document, parent)?;
            }
        }
    }

    assert!(index == tokens.len());

    Ok(())
}

fn token_emit(
    source: &[u8],
    token: InlineToken,
    href: Span,
    document: &mut Document,
    parent: u32,
) -> Result<()> {
    assert!(token.end as usize <= source.len());
    assert!(href.end() <= document.text_length());
    assert!(parent < document.node_count());

    let raw = &source[token.range()];

    match token.kind {
        InlineTokenKind::Text => text_emit(raw, token.marks, href, document, parent),
        InlineTokenKind::Code => code_emit(raw, token.marks, href, document, parent),
        InlineTokenKind::Autolink => {
            let text = document.text_append(raw)?;
            let kind = NodeKind::Text { href: token.href, marks: token.marks, text };

            document.node_append(parent, kind)?;

            Ok(())
        }
        InlineTokenKind::HTMLInline => {
            let text = document.text_append(raw)?;

            document.node_append(parent, NodeKind::HTMLInline { text })?;

            Ok(())
        }
        InlineTokenKind::SoftBreak => text_emit(b" ", token.marks, href, document, parent),
        InlineTokenKind::HardBreak => {
            document.node_append(parent, NodeKind::HardBreak)?;

            Ok(())
        }
        InlineTokenKind::Delimiter { character } => {
            let mut literal = [0u8; DELIMITER_LITERAL_MAX as usize];
            let count = usize::from(token.count).min(literal.len());

            literal[..count].fill(character);

            text_emit(&literal[..count], token.marks, href, document, parent)
        }

        InlineTokenKind::LinkOpen
        | InlineTokenKind::LinkClose
        | InlineTokenKind::ImageOpen
        | InlineTokenKind::ImageClose
        | InlineTokenKind::Removed => unreachable!("structural tokens are handled by emit"),
    }
}

fn image_emit(
    source: &[u8],
    tokens: &[InlineToken],
    mut index: usize,
    url: Span,
    document: &mut Document,
    parent: u32,
) -> Result<usize> {
    assert!(index <= tokens.len());
    assert!(url.end() <= document.text_length());
    assert!(parent < document.node_count());

    let mut alt = Span { length: 0, offset: document.text_length() };

    for _ in 0..tokens.len() {
        if index >= tokens.len() {
            break;
        }

        let token = tokens[index];
        index += 1;

        let raw = &source[token.range()];

        match token.kind {
            InlineTokenKind::ImageClose => break,
            InlineTokenKind::Text | InlineTokenKind::Autolink => {
                text_decode(raw, document, &mut alt)?;
            }
            InlineTokenKind::HTMLInline | InlineTokenKind::Code => {
                document.text_extend(&mut alt, raw)?;
            }
            InlineTokenKind::SoftBreak | InlineTokenKind::HardBreak => {
                document.text_extend(&mut alt, b" ")?;
            }
            InlineTokenKind::Delimiter { character } => {
                for _ in 0..token.count {
                    document.text_extend(&mut alt, &[character])?;
                }
            }

            InlineTokenKind::LinkOpen
            | InlineTokenKind::LinkClose
            | InlineTokenKind::ImageOpen
            | InlineTokenKind::Removed => {}
        }
    }

    document.node_append(parent, NodeKind::Image { alt, url })?;

    assert!(index <= tokens.len());
    assert!(alt.end() <= document.text_length());

    Ok(index)
}

fn text_emit(
    raw: &[u8],
    marks: Marks,
    href: Span,
    document: &mut Document,
    parent: u32,
) -> Result<()> {
    assert!(!marks.contains(Marks::CODE));
    assert!(href.end() <= document.text_length());
    assert!(parent < document.node_count());

    let mut text = Span { length: 0, offset: document.text_length() };

    text_decode(raw, document, &mut text)?;

    if text.is_empty() {
        return Ok(());
    }

    assert!(text.end() == document.text_length());

    document.text_node_place(parent, TextRun { href, marks, text })
}

const fn code_space_is(byte: u8) -> bool {
    byte == b' ' || byte == b'\n'
}

fn code_emit(
    raw: &[u8],
    marks: Marks,
    href: Span,
    document: &mut Document,
    parent: u32,
) -> Result<()> {
    assert!(!marks.contains(Marks::CODE));
    assert!(href.end() <= document.text_length());
    assert!(parent < document.node_count());

    let mut text = Span { length: 0, offset: document.text_length() };
    let all_spaces = raw.iter().all(|&byte| code_space_is(byte));
    let padded = raw.len() >= 2 && code_space_is(raw[0]) && code_space_is(raw[raw.len() - 1]);
    let trimmed = padded && !all_spaces;
    let body = if trimmed { &raw[1..raw.len() - 1] } else { raw };

    for &byte in body {
        let mapped = if byte == b'\n' { b' ' } else { byte };

        document.text_extend(&mut text, &[mapped])?;
    }

    let kind = NodeKind::Text { href, marks: marks.union(Marks::CODE), text };

    document.node_append(parent, kind)?;

    assert!(text.end() == document.text_length());

    Ok(())
}

pub(crate) fn text_decode(raw: &[u8], document: &mut Document, span: &mut Span) -> Result<()> {
    assert!(span.end() == document.text_length());

    let mut index = 0usize;

    for _ in 0..raw.len() {
        if index >= raw.len() {
            break;
        }

        let byte = raw[index];
        let escapes = byte == b'\\' && raw.get(index + 1).is_some_and(u8::is_ascii_punctuation);

        if escapes {
            document.text_extend(span, &raw[index + 1..index + 2])?;
            index += 2;

            continue;
        }

        if byte == b'&' {
            if let Some((first, second, length)) = entity_decode_html(&raw[index..]) {
                let mut encoded = [0u8; 4];
                let encoded_length = utf8_encode(first, &mut encoded);

                document.text_extend(span, &encoded[..encoded_length])?;

                if second != 0 {
                    let second_length = utf8_encode(second, &mut encoded);

                    document.text_extend(span, &encoded[..second_length])?;
                }

                index += length;

                continue;
            }
        }

        document.text_extend(span, &raw[index..=index])?;
        index += 1;
    }

    assert!(index == raw.len());

    Ok(())
}

fn entity_decode_html(rest: &[u8]) -> Option<(u32, u32, usize)> {
    assert!(!rest.is_empty());
    assert!(rest[0] == b'&');

    if let Some((code_point, length)) = entity_decode(rest) {
        return Some((code_point, 0, length));
    }

    let semicolon =
        rest.iter().take(ENTITY_NAME_LENGTH_MAX as usize + 2).position(|&byte| byte == b';')?;
    let name = &rest[1..semicolon];

    if name.is_empty() {
        return None;
    }

    if !name.iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }

    let (first, second) = entity_lookup(name)?;
    let end = semicolon + 1;

    assert!(first != 0);
    assert!(end <= rest.len());

    Some((first, second, end))
}

pub(crate) fn html_tag_length(source: &[u8], start: usize) -> Option<usize> {
    assert!(start < source.len());

    let rest = &source[start..];

    assert!(rest.first() == Some(&b'<'));

    if rest.starts_with(b"<!-->") {
        return Some(5);
    }

    if rest.starts_with(b"<!--->") {
        return Some(6);
    }

    if rest.starts_with(b"<!--") {
        return terminated_length(rest, 4, b"-->");
    }

    if rest.starts_with(b"<?") {
        return terminated_length(rest, 2, b"?>");
    }

    if rest.starts_with(b"<![CDATA[") {
        return terminated_length(rest, 9, b"]]>");
    }

    if rest.starts_with(b"<!") {
        if !rest.get(2).is_some_and(u8::is_ascii_alphabetic) {
            return None;
        }

        return terminated_length(rest, 2, b">");
    }

    if rest.starts_with(b"</") {
        let name_length = html_tag_name_length(&rest[2..])?;
        let close = 2 + name_length + whitespace_length(&rest[2 + name_length..]);

        assert!(close <= rest.len());

        return (rest.get(close) == Some(&b'>')).then_some(close + 1);
    }

    html_open_tag_length(rest)
}

fn terminated_length<const N: usize>(
    rest: &[u8],
    from: usize,
    terminator: &[u8; N],
) -> Option<usize> {
    assert!(N >= 1);
    assert!(from <= rest.len());
    assert!(from >= 2);

    if rest.len() < from + N {
        return None;
    }

    let found = (from..=rest.len() - N).find(|&index| rest[index..].starts_with(terminator))?;
    let end = found + N;

    assert!(end <= rest.len());

    Some(end)
}

fn html_tag_name_length(rest: &[u8]) -> Option<usize> {
    if !rest.first().is_some_and(u8::is_ascii_alphabetic) {
        return None;
    }

    let length =
        rest.iter().take_while(|&&byte| byte.is_ascii_alphanumeric() || byte == b'-').count();

    assert!(length >= 1);

    Some(length)
}

fn whitespace_length(rest: &[u8]) -> usize {
    let length = rest.iter().take_while(|&&byte| matches!(byte, b' ' | b'\t' | b'\n')).count();

    assert!(length <= rest.len());

    length
}

fn html_open_tag_length(rest: &[u8]) -> Option<usize> {
    assert!(rest.first() == Some(&b'<'));

    let name_length = html_tag_name_length(&rest[1..])?;
    let mut position = 1 + name_length;

    assert!(name_length >= 1);

    for _ in 0..rest.len() {
        let space = whitespace_length(&rest[position..]);

        let Some(attribute_length) = html_attribute_length(&rest[position + space..]) else {
            break;
        };

        if space == 0 {
            return None;
        }

        position += space + attribute_length;
    }

    position += whitespace_length(&rest[position..]);

    if rest.get(position) == Some(&b'/') {
        position += 1;
    }

    if rest.get(position) != Some(&b'>') {
        return None;
    }

    let end = position + 1;

    assert!(end <= rest.len());

    Some(end)
}

fn html_attribute_length(rest: &[u8]) -> Option<usize> {
    let first = *rest.first()?;

    if !(first.is_ascii_alphabetic() || first == b'_' || first == b':') {
        return None;
    }

    let name_length = rest
        .iter()
        .take_while(|&&byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-')
        })
        .count();

    let after_name = name_length + whitespace_length(&rest[name_length..]);

    if rest.get(after_name) != Some(&b'=') {
        return Some(name_length);
    }

    let value_start = after_name + 1 + whitespace_length(&rest[after_name + 1..]);
    let value_length = html_attribute_value_length(&rest[value_start..])?;
    let end = value_start + value_length;

    assert!(value_length >= 1);
    assert!(end <= rest.len());

    Some(end)
}

fn html_attribute_value_length(rest: &[u8]) -> Option<usize> {
    let quote = *rest.first()?;

    if matches!(quote, b'"' | b'\'') {
        let close = rest[1..].iter().position(|&byte| byte == quote)?;

        assert!(close + 2 <= rest.len());

        return Some(close + 2);
    }

    let length = rest
        .iter()
        .take_while(|&&byte| {
            !matches!(byte, b' ' | b'\t' | b'\n' | b'"' | b'\'' | b'=' | b'<' | b'>' | b'`')
        })
        .count();

    if length == 0 { None } else { Some(length) }
}
