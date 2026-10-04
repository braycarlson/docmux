use crate::bytes::{
    Sink,
    decimal_parse_u32,
    range_from_u32,
    range_from_usize,
    u32_from_usize,
    utf8_encode,
};
use crate::error::{Error, Result};
use crate::xml::BytePush;
use core::ops::Range;

const NESTING_DEPTH_MAX: u32 = 256;

fn malformed(offset: usize) -> Error {
    Error::JSONMalformed { offset: u32_from_usize(offset) }
}

fn whitespace_skip(source: &[u8], position: usize) -> usize {
    assert!(position <= source.len());

    let rest = &source[position..];
    let skipped = rest.iter().position(|byte| !byte.is_ascii_whitespace()).unwrap_or(rest.len());
    let end = position + skipped;

    assert!(end <= source.len());

    end
}

pub(crate) fn value_end(source: &[u8], start: usize) -> Result<usize> {
    assert!(start <= source.len());

    let value_start = whitespace_skip(source, start);

    if value_start >= source.len() {
        return Err(malformed(value_start));
    }

    let end = match source[value_start] {
        b'"' => string_end(source, value_start)?,
        b'{' | b'[' => container_end(source, value_start)?,
        _ => scalar_end(source, value_start)?,
    };

    assert!(end > value_start);
    assert!(end <= source.len());

    Ok(end)
}

fn scalar_end(source: &[u8], start: usize) -> Result<usize> {
    assert!(start < source.len());

    let rest = &source[start..];

    let length = rest
        .iter()
        .position(|&byte| {
            byte == b',' || byte == b'}' || byte == b']' || byte.is_ascii_whitespace()
        })
        .unwrap_or(rest.len());

    if length == 0 {
        return Err(malformed(start));
    }

    let end = start + length;

    assert!(end <= source.len());

    Ok(end)
}

fn string_end(source: &[u8], start: usize) -> Result<usize> {
    assert!(start < source.len());
    assert!(source[start] == b'"');

    let mut escaped = false;

    for (index, &byte) in source.iter().enumerate().skip(start + 1) {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            assert!(index < source.len());

            return Ok(index + 1);
        }
    }

    Err(malformed(start))
}

fn container_end(source: &[u8], start: usize) -> Result<usize> {
    assert!(start < source.len());
    assert!(source[start] == b'{' || source[start] == b'[');

    let mut depth: u32 = 0;
    let mut position = start;

    for _ in 0..source.len() {
        if position >= source.len() {
            break;
        }

        match source[position] {
            b'"' => {
                position = string_end(source, position)?;

                continue;
            }
            b'{' | b'[' => {
                depth += 1;

                if depth > NESTING_DEPTH_MAX {
                    return Err(malformed(position));
                }
            }
            b'}' | b']' => {
                depth -= 1;

                if depth == 0 {
                    assert!(position < source.len());

                    return Ok(position + 1);
                }
            }
            _ => {}
        }

        position += 1;
    }

    Err(malformed(start))
}

#[derive(Clone, Debug)]
pub(crate) struct Object<'a> {
    pub(crate) range: Range<u32>,
    pub(crate) source: &'a [u8],
}

impl Object<'_> {
    pub(crate) fn field(&self, key: &[u8]) -> Result<Option<Range<u32>>> {
        assert!(self.range.end as usize <= self.source.len());
        assert!(!key.is_empty());

        let source = self.source;
        let end = self.range.end as usize;
        let mut position = whitespace_skip(source, self.range.start as usize);

        if position >= end {
            return Err(malformed(position));
        }

        if source[position] != b'{' {
            return Err(malformed(position));
        }

        position += 1;

        for _ in 0..self.range.len() {
            position = whitespace_skip(source, position);

            if position >= end {
                return Err(malformed(position));
            }

            if source[position] == b'}' {
                return Ok(None);
            }

            if source[position] == b',' {
                position += 1;

                continue;
            }

            if source[position] != b'"' {
                return Err(malformed(position));
            }

            let key_end = string_end(source, position)?;
            let key_raw = &source[position + 1..key_end - 1];
            position = whitespace_skip(source, key_end);

            if position >= end {
                return Err(malformed(position));
            }

            if source[position] != b':' {
                return Err(malformed(position));
            }

            let value_start = whitespace_skip(source, position + 1);
            let value_end = value_end(source, value_start)?;

            if value_end > end {
                return Err(malformed(value_start));
            }

            if key_raw == key {
                return Ok(Some(range_from_usize(&(value_start..value_end))));
            }

            position = value_end;
        }

        Err(malformed(position))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ArrayItems {
    done: bool,
    end: u32,
    position: u32,
    remaining: u32,
}

impl ArrayItems {
    pub(crate) const EMPTY: Self = Self { done: true, end: 0, position: 0, remaining: 0 };

    pub(crate) fn new(source: &[u8], array: Range<u32>) -> Result<Self> {
        assert!(array.end as usize <= source.len());

        let position = whitespace_skip(source, array.start as usize);

        if position >= array.end as usize {
            return Err(malformed(position));
        }

        if source[position] != b'[' {
            return Err(malformed(position));
        }

        Ok(Self {
            done: false,
            end: array.end,
            position: u32_from_usize(position + 1),
            remaining: u32_from_usize(array.len()),
        })
    }

    pub(crate) fn next(&mut self, source: &[u8]) -> Result<Option<Range<u32>>> {
        assert!(self.end as usize <= source.len());

        if self.done {
            return Ok(None);
        }

        for _ in 0..self.remaining.max(1) {
            let start = whitespace_skip(source, self.position as usize);
            self.position = u32_from_usize(start);

            if self.position >= self.end {
                return Err(malformed(start));
            }

            match source[start] {
                b']' => {
                    self.done = true;

                    return Ok(None);
                }
                b',' => self.position += 1,
                _ => {
                    let end = value_end(source, start)?;

                    if end > self.end as usize {
                        return Err(malformed(start));
                    }

                    self.position = u32_from_usize(end);

                    assert!(end > start);

                    return Ok(Some(range_from_usize(&(start..end))));
                }
            }
        }

        Err(Error::JSONMalformed { offset: self.position })
    }
}

pub(crate) fn string_decode<P: BytePush>(
    source: &[u8],
    range: Range<u32>,
    out: &mut P,
) -> Result<()> {
    assert!(range.end as usize <= source.len());

    let body = string_raw(source, range.clone())?;
    let mut index = 0usize;

    assert!(body.len() + 2 == range.len());

    for _ in 0..body.len() {
        if index >= body.len() {
            break;
        }

        let byte = body[index];

        if byte != b'\\' {
            out.byte_push(byte)?;
            index += 1;

            continue;
        }

        let offset = range.start as usize + 1 + index;

        let (code_point, length) =
            escape_decode(&body[index..]).ok_or_else(|| malformed(offset))?;

        let mut encoded = [0u8; 4];
        let encoded_length = utf8_encode(code_point, &mut encoded);

        assert!(encoded_length >= 1);

        for &encoded_byte in &encoded[..encoded_length] {
            out.byte_push(encoded_byte)?;
        }

        index += length;
    }

    assert!(index == body.len());

    Ok(())
}

fn escape_decode(rest: &[u8]) -> Option<(u32, usize)> {
    assert!(!rest.is_empty());
    assert!(rest[0] == b'\\');

    let code = *rest.get(1)?;

    let code_point = match code {
        b'"' => u32::from(b'"'),
        b'\\' => u32::from(b'\\'),
        b'/' => u32::from(b'/'),
        b'b' => 0x08,
        b'f' => 0x0c,
        b'n' => u32::from(b'\n'),
        b'r' => u32::from(b'\r'),
        b't' => u32::from(b'\t'),
        b'u' => return unicode_escape_decode(rest),
        _ => return None,
    };

    Some((code_point, 2))
}

fn hex4(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 4 {
        return None;
    }

    let mut value: u32 = 0;

    for &byte in &bytes[..4] {
        value = value * 16 + char::from(byte).to_digit(16)?;
    }

    assert!(value <= 0xFFFF);

    Some(value)
}

fn unicode_escape_decode(rest: &[u8]) -> Option<(u32, usize)> {
    assert!(rest.len() >= 2);
    assert!(rest[1] == b'u');

    let high = hex4(&rest[2..])?;

    if (0xD800..0xDC00).contains(&high) {
        if rest.len() < 12 {
            return Some((u32::from(char::REPLACEMENT_CHARACTER), 6));
        }

        if rest.get(6..8) != Some(&b"\\u"[..]) {
            return Some((u32::from(char::REPLACEMENT_CHARACTER), 6));
        }

        let low = hex4(&rest[8..])?;

        if (0xDC00..0xE000).contains(&low) {
            let code_point = 0x10000 + ((high - 0xD800) << 10u32) + (low - 0xDC00);

            return Some((code_point, 12));
        }

        return Some((u32::from(char::REPLACEMENT_CHARACTER), 6));
    }

    Some((high, 6))
}

pub(crate) fn string_raw(source: &[u8], range: Range<u32>) -> Result<&[u8]> {
    assert!(range.end as usize <= source.len());

    let raw = &source[range_from_u32(&range)];
    let quoted = raw.len() >= 2 && raw[0] == b'"' && raw[raw.len() - 1] == b'"';

    if !quoted {
        return Err(Error::JSONMalformed { offset: range.start });
    }

    Ok(&raw[1..raw.len() - 1])
}

pub(crate) fn u32_parse(source: &[u8], range: Range<u32>) -> Result<u32> {
    assert!(range.end as usize <= source.len());

    let raw = &source[range_from_u32(&range)];

    decimal_parse_u32(raw).ok_or(Error::JSONMalformed { offset: range.start })
}

pub(crate) fn string_write(sink: &mut Sink<'_>, bytes: &[u8]) -> Result<()> {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    sink.write_byte(b'"')?;

    for &byte in bytes {
        match byte {
            b'"' => sink.write(b"\\\"")?,
            b'\\' => sink.write(b"\\\\")?,
            b'\n' => sink.write(b"\\n")?,
            b'\r' => sink.write(b"\\r")?,
            b'\t' => sink.write(b"\\t")?,
            _ => {
                if byte < 0x20 {
                    sink.write(b"\\u00")?;
                    sink.write_byte(HEX[usize::from(byte >> 4u8)])?;
                    sink.write_byte(HEX[usize::from(byte & 0x0f)])?;
                } else {
                    sink.write_byte(byte)?;
                }
            }
        }
    }

    sink.write_byte(b'"')
}

#[cfg(test)]
mod tests {
    use super::{
        ArrayItems,
        Object,
        string_decode,
        string_raw,
        string_write,
        u32_parse,
        value_end,
    };
    use crate::bytes::{Sink, range_from_u32, u32_from_usize};
    use crate::error::Error;

    #[test]
    fn finds_fields_in_any_order() {
        let source = br#"{"content":[{"a":1},{"b":[1,2]}],"type":"doc","n":42,"ok":true}"#;
        let object = 0..u32_from_usize(source.len());
        let kind = Object { range: object.clone(), source }.field(b"type").unwrap().unwrap();

        assert_eq!(string_raw(source, kind).unwrap(), b"doc");

        let number = Object { range: object.clone(), source }.field(b"n").unwrap().unwrap();

        assert_eq!(u32_parse(source, number).unwrap(), 42);
        assert_eq!(Object { range: object.clone(), source }.field(b"missing").unwrap(), None);

        let content = Object { range: object, source }.field(b"content").unwrap().unwrap();
        let mut items = ArrayItems::new(source, content).unwrap();
        let first = items.next(source).unwrap().unwrap();

        assert_eq!(&source[range_from_u32(&first)], b"{\"a\":1}");

        let second = items.next(source).unwrap().unwrap();
        let third = items.next(source).unwrap();

        assert_eq!(&source[range_from_u32(&second)], b"{\"b\":[1,2]}");
        assert_eq!(third, None);
    }

    #[test]
    fn decodes_and_encodes_strings() {
        let source = "\"a\\\"b\\\\c\\n\u{e9}\u{1f600}\\ud83d\\ude00\\ud83d\"".as_bytes();
        let mut decoded = [0u8; 32];
        let mut decoded_sink = Sink::new(&mut decoded);

        string_decode(source, 0..u32_from_usize(source.len()), &mut decoded_sink).unwrap();
        assert_eq!(decoded_sink.written(), "a\"b\\c\n\u{e9}\u{1f600}\u{1f600}\u{fffd}".as_bytes());

        let mut encoded = [0u8; 32];
        let mut encoded_sink = Sink::new(&mut encoded);

        string_write(&mut encoded_sink, b"x\"\n\x01").unwrap();
        assert_eq!(encoded_sink.written(), br#""x\"\n\u0001""#);
    }

    #[test]
    fn rejects_unterminated() {
        assert_eq!(value_end(b"[1,2", 0), Err(Error::JSONMalformed { offset: 0 }));
        assert_eq!(value_end(b"\"abc", 0), Err(Error::JSONMalformed { offset: 0 }));

        assert_eq!(
            Object { range: 0..3, source: b"[1]" }.field(b"a"),
            Err(Error::JSONMalformed { offset: 0 }),
        );
    }
}
