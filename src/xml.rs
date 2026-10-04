use crate::bytes::{Sink, u32_from_usize, utf8_encode};
use crate::document::Document;
use crate::error::{Error, Result};
use crate::workspace::PART_BYTES_MAX;

const ELEMENT_DEPTH_MAX: u32 = 1024;
const ENTITY_LENGTH_MAX: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XMLEvent<'a> {
    Empty { attributes: &'a [u8], name: &'a [u8] },
    End { name: &'a [u8] },
    Finished,
    Start { attributes: &'a [u8], name: &'a [u8] },
    Text { cdata: bool, raw: &'a [u8] },
}

#[derive(Debug)]
pub(crate) struct XMLReader<'a> {
    position: u32,
    source: &'a [u8],
}

impl<'a> XMLReader<'a> {
    pub(crate) fn new(source: &'a [u8]) -> Self {
        assert!(source.len() <= PART_BYTES_MAX as usize);

        Self { position: 0, source }
    }

    pub(crate) fn next(&mut self) -> Result<XMLEvent<'a>> {
        assert!(self.position as usize <= self.source.len());

        for _ in 0..self.source.len() {
            let rest = self.rest();

            if rest.is_empty() {
                return Ok(XMLEvent::Finished);
            }

            if rest[0] != b'<' {
                return Ok(self.text_read());
            }

            if rest.starts_with(b"<![CDATA[") {
                return self.cdata_read();
            }

            if rest.starts_with(b"<!--") {
                self.skip_until(b"-->")?;

                continue;
            }

            if rest.starts_with(b"<?") {
                self.skip_until(b"?>")?;

                continue;
            }

            if rest.starts_with(b"<!") {
                self.skip_until(b">")?;

                continue;
            }

            if rest.starts_with(b"</") {
                return self.end_tag_read();
            }

            return self.start_tag_read();
        }

        Err(self.malformed())
    }

    pub(crate) fn position(&self) -> u32 {
        assert!(self.position as usize <= self.source.len());

        self.position
    }

    pub(crate) fn skip_element(&mut self) -> Result<()> {
        assert!(self.position as usize <= self.source.len());

        let mut depth: u32 = 1;

        for _ in 0..self.source.len() {
            match self.next()? {
                XMLEvent::Start { .. } => {
                    depth += 1;

                    if depth > ELEMENT_DEPTH_MAX {
                        return Err(self.malformed());
                    }
                }
                XMLEvent::End { .. } => {
                    assert!(depth >= 1);

                    depth -= 1;

                    if depth == 0 {
                        return Ok(());
                    }
                }
                XMLEvent::Empty { .. } | XMLEvent::Text { .. } => {}
                XMLEvent::Finished => return Err(self.malformed()),
            }
        }

        Err(self.malformed())
    }

    fn cdata_read(&mut self) -> Result<XMLEvent<'a>> {
        assert!(self.rest().starts_with(b"<![CDATA["));

        let start = self.position as usize + b"<![CDATA[".len();
        let rest = &self.source[start..];
        let length = find(rest, b"]]>").ok_or_else(|| self.malformed())?;

        assert!(length + b"]]>".len() <= rest.len());

        self.position = u32_from_usize(start + length + b"]]>".len());

        assert!(self.position as usize <= self.source.len());

        Ok(XMLEvent::Text { cdata: true, raw: &rest[..length] })
    }

    fn end_tag_read(&mut self) -> Result<XMLEvent<'a>> {
        assert!(self.rest().starts_with(b"</"));

        let start = self.position as usize + 2;
        let rest = &self.source[start..];
        let length = find(rest, b">").ok_or_else(|| self.malformed())?;
        let name = rest[..length].trim_ascii();

        if name.is_empty() {
            return Err(self.malformed());
        }

        self.position = u32_from_usize(start + length + 1);

        assert!(self.position as usize <= self.source.len());

        Ok(XMLEvent::End { name })
    }

    const fn malformed(&self) -> Error {
        Error::XMLMalformed { offset: self.position }
    }

    fn rest(&self) -> &'a [u8] {
        assert!(self.position as usize <= self.source.len());

        &self.source[self.position as usize..]
    }

    fn skip_until<const N: usize>(&mut self, terminator: &[u8; N]) -> Result<()> {
        assert!(self.position as usize <= self.source.len());

        let rest = self.rest();
        let length = find(rest, terminator).ok_or_else(|| self.malformed())?;
        self.position += u32_from_usize(length + terminator.len());

        assert!(self.position as usize <= self.source.len());

        Ok(())
    }

    fn start_tag_read(&mut self) -> Result<XMLEvent<'a>> {
        assert!(self.rest().starts_with(b"<"));

        let start = self.position as usize + 1;
        let rest = &self.source[start..];

        let name_length = rest
            .iter()
            .position(|&byte| byte.is_ascii_whitespace() || byte == b'/' || byte == b'>')
            .ok_or_else(|| self.malformed())?;

        if name_length == 0 {
            return Err(self.malformed());
        }

        let name = &rest[..name_length];
        let tail = &rest[name_length..];
        let close = tag_close_find(tail).ok_or_else(|| self.malformed())?;

        assert!(close < tail.len());

        let empty = close > 0 && tail[close - 1] == b'/';
        let attributes_end = if empty { close - 1 } else { close };
        let attributes = tail[..attributes_end].trim_ascii();
        self.position = u32_from_usize(start + name_length + close + 1);

        assert!(self.position as usize <= self.source.len());

        if empty {
            Ok(XMLEvent::Empty { attributes, name })
        } else {
            Ok(XMLEvent::Start { attributes, name })
        }
    }

    fn text_read(&mut self) -> XMLEvent<'a> {
        assert!((self.position as usize) < self.source.len());

        let rest = self.rest();
        let length = rest.iter().position(|&byte| byte == b'<').unwrap_or(rest.len());
        self.position += u32_from_usize(length);

        assert!(length >= 1);

        XMLEvent::Text { cdata: false, raw: &rest[..length] }
    }
}

fn tag_close_find(tail: &[u8]) -> Option<usize> {
    let mut quote: u8 = 0;

    for (index, &byte) in tail.iter().enumerate() {
        if quote != 0 {
            if byte == quote {
                quote = 0;
            }
        } else if matches!(byte, b'"' | b'\'') {
            quote = byte;
        } else if byte == b'>' {
            assert!(quote == 0);

            return Some(index);
        }
    }

    None
}

pub(crate) fn find<const N: usize>(haystack: &[u8], needle: &[u8; N]) -> Option<usize> {
    assert!(N >= 1);

    if haystack.len() < N {
        return None;
    }

    let found = (0..=haystack.len() - N).find(|&index| haystack[index..].starts_with(needle));

    assert!(found.is_none_or(|index| index + N <= haystack.len()));

    found
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Attributes<'a> {
    position: u32,
    raw: &'a [u8],
}

impl<'a> Attributes<'a> {
    pub(crate) const fn new(raw: &'a [u8]) -> Self {
        Self { position: 0, raw }
    }

    pub(crate) fn get<const N: usize>(raw: &'a [u8], key: &[u8; N]) -> Option<&'a [u8]> {
        assert!(N >= 1);

        let mut attributes = Self::new(raw);

        for _ in 0..raw.len() {
            match attributes.next() {
                Some((name, value)) => {
                    if name == key {
                        return Some(value);
                    }
                }
                None => return None,
            }
        }

        None
    }

    pub(crate) fn next(&mut self) -> Option<(&'a [u8], &'a [u8])> {
        assert!(self.position as usize <= self.raw.len());

        let rest = &self.raw[self.position as usize..];
        let skipped = rest.iter().position(|byte| !byte.is_ascii_whitespace())?;
        let after_space = &rest[skipped..];

        let name_length =
            after_space.iter().position(|&byte| byte == b'=' || byte.is_ascii_whitespace())?;

        let name = &after_space[..name_length];
        let after_name = &after_space[name_length..];
        let equals = after_name.iter().position(|&byte| byte == b'=')?;
        let after_equals = &after_name[equals + 1..];
        let quote_offset = after_equals.iter().position(|byte| !byte.is_ascii_whitespace())?;
        let quote = after_equals[quote_offset];

        if !matches!(quote, b'"' | b'\'') {
            return None;
        }

        let value_start = quote_offset + 1;
        let value_length = after_equals[value_start..].iter().position(|&byte| byte == quote)?;
        let value = &after_equals[value_start..value_start + value_length];
        let consumed = skipped + name_length + equals + 1 + value_start + value_length + 1;
        self.position += u32_from_usize(consumed);

        assert!(self.position as usize <= self.raw.len());
        assert!(!name.is_empty());

        Some((name, value))
    }
}

pub(crate) trait BytePush {
    fn byte_push(&mut self, byte: u8) -> Result<()>;
}

impl BytePush for Sink<'_> {
    fn byte_push(&mut self, byte: u8) -> Result<()> {
        self.write_byte(byte)
    }
}

impl BytePush for Document {
    fn byte_push(&mut self, byte: u8) -> Result<()> {
        self.text_push(byte)
    }
}

pub(crate) fn decode<P: BytePush>(raw: &[u8], out: &mut P) -> Result<()> {
    let mut index = 0usize;

    for _ in 0..raw.len() {
        if index >= raw.len() {
            break;
        }

        let byte = raw[index];

        if byte != b'&' {
            out.byte_push(byte)?;
            index += 1;

            continue;
        }

        let rest = &raw[index..];

        if let Some((code_point, length)) = entity_decode(rest) {
            let mut encoded = [0u8; 4];
            let encoded_length = utf8_encode(code_point, &mut encoded);

            assert!(encoded_length >= 1);

            for &encoded_byte in &encoded[..encoded_length] {
                out.byte_push(encoded_byte)?;
            }

            index += length;
        } else {
            out.byte_push(byte)?;
            index += 1;
        }
    }

    assert!(index == raw.len());

    Ok(())
}

pub(crate) fn entity_decode(rest: &[u8]) -> Option<(u32, usize)> {
    assert!(!rest.is_empty());
    assert!(rest[0] == b'&');

    let limit = rest.len().min(ENTITY_LENGTH_MAX as usize + 2);
    let semicolon = rest[..limit].iter().position(|&byte| byte == b';')?;

    assert!(semicolon >= 1);

    let body = &rest[1..semicolon];

    assert!(body.len() <= ENTITY_LENGTH_MAX as usize);

    let length = semicolon + 1;

    let code_point = match body {
        b"amp" => u32::from(b'&'),
        b"lt" => u32::from(b'<'),
        b"gt" => u32::from(b'>'),
        b"quot" => u32::from(b'"'),
        b"apos" => u32::from(b'\''),
        _ => numeric_entity_decode(body)?,
    };

    assert!(length >= 2);

    Some((code_point, length))
}

fn numeric_entity_decode(body: &[u8]) -> Option<u32> {
    let digits_all = body.strip_prefix(b"#")?;

    let (radix, digits, digits_max) = digits_all
        .strip_prefix(b"x")
        .or_else(|| digits_all.strip_prefix(b"X"))
        .map_or((10u32, digits_all, 7), |hex| (16u32, hex, 6));

    if !(1..=digits_max).contains(&digits.len()) {
        return None;
    }

    let mut value: u32 = 0;

    for &digit in digits {
        let digit_value = char::from(digit).to_digit(radix)?;
        value = value.checked_mul(radix)?.checked_add(digit_value)?;
    }

    let scalar = char::from_u32(value).filter(|character| *character != '\0');

    Some(scalar.map_or_else(|| u32::from(char::REPLACEMENT_CHARACTER), u32::from))
}

const fn is_xml_char(byte: u8) -> bool {
    byte >= 0x20 || byte == b'\t' || byte == b'\n' || byte == b'\r'
}

pub(crate) fn escape_text(text: &[u8], sink: &mut Sink<'_>) -> Result<()> {
    for &byte in text {
        match byte {
            b'&' => sink.write(b"&amp;")?,
            b'<' => sink.write(b"&lt;")?,
            b'>' => sink.write(b"&gt;")?,
            _ => {
                if is_xml_char(byte) {
                    sink.write_byte(byte)?;
                }
            }
        }
    }

    Ok(())
}

pub(crate) fn escape_attribute(value: &[u8], sink: &mut Sink<'_>) -> Result<()> {
    for &byte in value {
        match byte {
            b'&' => sink.write(b"&amp;")?,
            b'<' => sink.write(b"&lt;")?,
            b'>' => sink.write(b"&gt;")?,
            b'"' => sink.write(b"&quot;")?,
            _ => {
                if is_xml_char(byte) {
                    sink.write_byte(byte)?;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Attributes, XMLEvent, XMLReader, decode, escape_attribute};
    use crate::bytes::Sink;
    use crate::error::Error;

    #[test]
    fn tokenizes_elements_text_and_attributes() {
        let source = concat!(
            "<?xml version=\"1.0\"?><w:p a=\"1\" b='x>y'>",
            "<w:t xml:space=\"preserve\"> hi &amp; </w:t><w:br/></w:p>",
        )
        .as_bytes();

        let mut reader = XMLReader::new(source);
        let paragraph = reader.next().unwrap();
        let text_open = reader.next().unwrap();
        let text = reader.next().unwrap();

        assert_eq!(paragraph, XMLEvent::Start { attributes: b"a=\"1\" b='x>y'", name: b"w:p" },);

        assert_eq!(
            text_open,
            XMLEvent::Start { attributes: b"xml:space=\"preserve\"", name: b"w:t" },
        );

        assert_eq!(text, XMLEvent::Text { cdata: false, raw: b" hi &amp; " });

        let text_close = reader.next().unwrap();
        let line_break = reader.next().unwrap();
        let paragraph_close = reader.next().unwrap();
        let end = reader.next().unwrap();

        assert_eq!(text_close, XMLEvent::End { name: b"w:t" });
        assert_eq!(line_break, XMLEvent::Empty { attributes: b"", name: b"w:br" });
        assert_eq!(paragraph_close, XMLEvent::End { name: b"w:p" });
        assert_eq!(end, XMLEvent::Finished);
        assert_eq!(Attributes::get(b"a=\"1\" b='x>y'", b"b"), Some(&b"x>y"[..]));
        assert_eq!(Attributes::get(b"a=\"1\" b='x>y'", b"c"), None);
    }

    #[test]
    fn skips_elements_and_rejects_unterminated() {
        let source = b"<a><b><c/>text</b></a><d>";
        let mut reader = XMLReader::new(source);
        let first = reader.next().unwrap();

        assert!(matches!(first, XMLEvent::Start { name: b"a", .. }));

        reader.skip_element().unwrap();

        let after_skip = reader.next().unwrap();
        let unterminated = XMLReader::new(b"<a").next();

        assert!(matches!(after_skip, XMLEvent::Start { name: b"d", .. }));
        assert_eq!(unterminated, Err(Error::XMLMalformed { offset: 0 }));
    }

    #[test]
    fn decodes_entities() {
        let mut buffer = [0u8; 32];
        let mut sink = Sink::new(&mut buffer);

        decode(b"a&lt;b&#233;&#x41;&bogus;&", &mut sink).unwrap();
        assert_eq!(sink.written(), "a<b\u{e9}A&bogus;&".as_bytes());
    }

    #[test]
    fn escapes_and_drops_control_characters() {
        let mut buffer = [0u8; 32];
        let mut sink = Sink::new(&mut buffer);

        escape_attribute(b"a<b>&\"\x0b", &mut sink).unwrap();
        assert_eq!(sink.written(), b"a&lt;b&gt;&amp;&quot;");
    }
}
