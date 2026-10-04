use crate::error::{Error, Result};
use core::ops::Range;
use core::str;

pub(crate) const DECIMAL_DIGIT_COUNT_MAX: usize = 10;

#[derive(Debug)]
pub(crate) struct Sink<'a> {
    buffer: &'a mut [u8],
    length: u32,
}

impl<'a> Sink<'a> {
    pub(crate) fn new(buffer: &'a mut [u8]) -> Self {
        let capacity = buffer.len().min(u32::MAX as usize);

        Self { buffer: &mut buffer[..capacity], length: 0 }
    }

    pub(crate) fn length(&self) -> u32 {
        assert!(self.length as usize <= self.buffer.len());

        self.length
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<()> {
        assert!(self.length as usize <= self.buffer.len());

        let start = self.length as usize;
        let end = start + bytes.len();

        if end > self.buffer.len() {
            return Err(Error::OutputCapacity);
        }

        self.buffer[start..end].copy_from_slice(bytes);
        self.length = u32_from_usize(end);

        assert!(self.length as usize <= self.buffer.len());

        Ok(())
    }

    pub(crate) fn write_byte(&mut self, byte: u8) -> Result<()> {
        assert!(self.length as usize <= self.buffer.len());

        let index = self.length as usize;

        if index == self.buffer.len() {
            return Err(Error::OutputCapacity);
        }

        self.buffer[index] = byte;
        self.length += 1;

        assert!(self.length as usize <= self.buffer.len());

        Ok(())
    }

    pub(crate) fn write_repeat(&mut self, byte: u8, count: usize) -> Result<()> {
        assert!(self.length as usize <= self.buffer.len());

        let start = self.length as usize;
        let end = start + count;

        if end > self.buffer.len() {
            return Err(Error::OutputCapacity);
        }

        self.buffer[start..end].fill(byte);
        self.length = u32_from_usize(end);

        assert!(self.length as usize <= self.buffer.len());

        Ok(())
    }

    pub(crate) fn write_u32(&mut self, value: u32) -> Result<()> {
        let mut digits = [0u8; DECIMAL_DIGIT_COUNT_MAX];
        let length = decimal_format_u32(value, &mut digits);

        assert!(length >= 1);

        self.write(&digits[..length])
    }

    pub(crate) fn write_u32_le(&mut self, value: u32) -> Result<()> {
        self.write(&value.to_le_bytes())
    }

    pub(crate) fn write_u16_le(&mut self, value: u16) -> Result<()> {
        self.write(&value.to_le_bytes())
    }

    pub(crate) fn write_u32_le_at(&mut self, offset: usize, value: u32) {
        assert!(offset + 4 <= self.length as usize);

        self.buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());

        assert!(self.buffer[offset..offset + 4] == value.to_le_bytes());
    }

    pub(crate) fn written(&self) -> &[u8] {
        assert!(self.length as usize <= self.buffer.len());

        &self.buffer[..self.length as usize]
    }
}

pub(crate) fn u8_from_u32(value: u32) -> u8 {
    assert!(value & !u32::from(u8::MAX) == 0);

    let Ok(narrow) = u8::try_from(value) else { unreachable!("{value} does not fit in a u8") };

    assert!(u32::from(narrow) == value);

    narrow
}

pub(crate) fn u8_from_usize(value: usize) -> u8 {
    assert!(value & !usize::from(u8::MAX) == 0);

    let Ok(narrow) = u8::try_from(value) else { unreachable!("{value} does not fit in a u8") };

    assert!(usize::from(narrow) == value);

    narrow
}

pub(crate) fn u16_from_usize(value: usize) -> u16 {
    assert!(value & !usize::from(u16::MAX) == 0);

    let Ok(narrow) = u16::try_from(value) else { unreachable!("{value} does not fit in a u16") };

    assert!(usize::from(narrow) == value);

    narrow
}

pub(crate) fn u32_from_usize(value: usize) -> u32 {
    assert!(value & !(u32::MAX as usize) == 0);

    let Ok(narrow) = u32::try_from(value) else { unreachable!("{value} does not fit in a u32") };

    assert!(narrow as usize == value);

    narrow
}

pub(crate) const fn range_from_u32(range: &Range<u32>) -> Range<usize> {
    assert!(range.start <= range.end);

    range.start as usize..range.end as usize
}

pub(crate) fn range_from_usize(range: &Range<usize>) -> Range<u32> {
    assert!(range.start <= range.end);

    u32_from_usize(range.start)..u32_from_usize(range.end)
}

pub(crate) fn decimal_format_u32(
    mut value: u32,
    digits: &mut [u8; DECIMAL_DIGIT_COUNT_MAX],
) -> usize {
    let mut length = 0usize;

    for _ in 0..DECIMAL_DIGIT_COUNT_MAX {
        digits[DECIMAL_DIGIT_COUNT_MAX - 1 - length] = b'0' + u8_from_u32(value % 10);
        value /= 10;
        length += 1;

        if value == 0 {
            break;
        }
    }

    assert!(value == 0);
    assert!(length >= 1);
    assert!(length <= DECIMAL_DIGIT_COUNT_MAX);
    digits.copy_within(DECIMAL_DIGIT_COUNT_MAX - length..DECIMAL_DIGIT_COUNT_MAX, 0);

    length
}

pub(crate) fn decimal_parse_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }

    if bytes.len() > DECIMAL_DIGIT_COUNT_MAX {
        return None;
    }

    let mut value: u32 = 0;

    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }

        value = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
    }

    Some(value)
}

pub(crate) fn utf8_validate(bytes: &[u8]) -> Result<()> {
    match str::from_utf8(bytes) {
        Ok(_) => Ok(()),
        Err(error) => Err(Error::UTF8Invalid { offset: u32_from_usize(error.valid_up_to()) }),
    }
}

pub(crate) fn utf8_encode(code_point: u32, out: &mut [u8; 4]) -> usize {
    let character = char::from_u32(code_point).unwrap_or(char::REPLACEMENT_CHARACTER);
    let encoded = character.encode_utf8(out);

    assert!(!encoded.is_empty());
    assert!(encoded.len() <= 4);

    encoded.len()
}

#[cfg(test)]
mod tests {
    use super::{Sink, decimal_format_u32, decimal_parse_u32, u32_from_usize, utf8_encode};
    use crate::error::Error;

    #[test]
    fn decimal_round_trips() {
        let mut digits = [0u8; 10];
        let length = decimal_format_u32(u32::MAX, &mut digits);

        assert_eq!(&digits[..length], b"4294967295");
        assert_eq!(decimal_parse_u32(&digits[..length]), Some(u32::MAX));

        let digits_of_zero = decimal_format_u32(0, &mut digits);

        assert_eq!(&digits[..digits_of_zero], b"0");
        assert_eq!(decimal_parse_u32(b"4294967296"), None);
        assert_eq!(decimal_parse_u32(b""), None);
        assert_eq!(decimal_parse_u32(b"12a"), None);
    }

    #[test]
    fn sink_reports_capacity() {
        let mut buffer = [0u8; 4];
        let mut sink = Sink::new(&mut buffer);
        let filled = sink.write(b"abc");
        let overflowed = sink.write(b"de");
        let filled_byte = sink.write_byte(b'd');
        let overflowed_byte = sink.write_byte(b'e');

        assert_eq!(filled, Ok(()));
        assert_eq!(overflowed, Err(Error::OutputCapacity));
        assert_eq!(filled_byte, Ok(()));
        assert_eq!(overflowed_byte, Err(Error::OutputCapacity));
        assert_eq!(sink.written(), b"abcd");
    }

    #[test]
    fn narrowing_and_encoding_hold_their_bounds() {
        assert_eq!(u32_from_usize(u32::MAX as usize), u32::MAX);

        let mut out = [0u8; 4];

        assert_eq!(utf8_encode(0x41, &mut out), 1);
        assert_eq!(utf8_encode(0x1F600, &mut out), 4);
        assert_eq!(utf8_encode(0xD800, &mut out), 3);
        assert_eq!(&out[..3], "\u{FFFD}".as_bytes());
    }
}
