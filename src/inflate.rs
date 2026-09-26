use crate::bytes::{u8_from_u32, u16_from_usize, u32_from_usize};
use crate::error::{Error, Result};
use crate::workspace::PART_BYTES_MAX;

const CODE_LENGTH_ORDER: [usize; 19] =
    [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
const CODE_LENGTH_MAX: usize = 15;

const DISTANCE_BASE: [u16; 30] = [
    1,
    2,
    3,
    4,
    5,
    7,
    9,
    13,
    17,
    25,
    33,
    49,
    65,
    97,
    129,
    193,
    257,
    385,
    513,
    769,
    1025,
    1537,
    2049,
    3073,
    4097,
    6145,
    8193,
    12289,
    16385,
    24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0,
    0,
    0,
    0,
    1,
    1,
    2,
    2,
    3,
    3,
    4,
    4,
    5,
    5,
    6,
    6,
    7,
    7,
    8,
    8,
    9,
    9,
    10,
    10,
    11,
    11,
    12,
    12,
    13,
    13,
];
const DISTANCE_SYMBOL_COUNT_MAX: usize = 30;

const SYMBOL_LENGTH_BASE: [u16; 29] = [
    3,
    4,
    5,
    6,
    7,
    8,
    9,
    10,
    11,
    13,
    15,
    17,
    19,
    23,
    27,
    31,
    35,
    43,
    51,
    59,
    67,
    83,
    99,
    115,
    131,
    163,
    195,
    227,
    258,
];
const SYMBOL_LENGTH_EXTRA: [u8; 29] =
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const LITERAL_SYMBOL_COUNT_MAX: usize = 288;
const LITERAL_SYMBOL_END: u16 = 256;
const LITERAL_SYMBOL_LENGTH_FIRST: u16 = 257;

#[derive(Clone, Copy, Debug)]
struct Huffman {
    count: [u16; CODE_LENGTH_MAX + 1],
    symbol: [u16; LITERAL_SYMBOL_COUNT_MAX],
}

impl Huffman {
    const EMPTY: Self =
        Self { count: [0; CODE_LENGTH_MAX + 1], symbol: [0; LITERAL_SYMBOL_COUNT_MAX] };

    fn build(&mut self, lengths: &[u8]) -> bool {
        assert!(!lengths.is_empty());
        assert!(lengths.len() <= LITERAL_SYMBOL_COUNT_MAX);

        self.count = [0; CODE_LENGTH_MAX + 1];

        for &length in lengths {
            self.count[usize::from(length)] += 1;
        }

        let mut left: i32 = 1;

        for length in 1..=CODE_LENGTH_MAX {
            left <<= 1u32;
            left -= i32::from(self.count[length]);

            if left < 0i32 {
                return false;
            }
        }

        let mut offsets = [0u16; CODE_LENGTH_MAX + 2];

        for length in 1..=CODE_LENGTH_MAX {
            offsets[length + 1] = offsets[length] + self.count[length];
        }

        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let slot = usize::from(offsets[usize::from(length)]);
                self.symbol[slot] = u16_from_usize(symbol);
                offsets[usize::from(length)] += 1;
            }
        }

        assert!(left >= 0i32);

        true
    }
}

#[derive(Clone, Copy, Debug)]
struct Codes {
    distance: Huffman,
    literal: Huffman,
}

impl Codes {
    const EMPTY: Self = Self { distance: Huffman::EMPTY, literal: Huffman::EMPTY };
}

#[derive(Debug)]
struct Bits<'a> {
    bit_buffer: u32,
    bit_count: u32,
    input: &'a [u8],
    position: usize,
}

impl Bits<'_> {
    fn malformed(&self) -> Error {
        Error::DeflateMalformed { offset: u32_from_usize(self.position) }
    }

    fn take(&mut self, count: u32) -> Result<u32> {
        assert!(count <= 16);
        assert!(self.bit_count <= 32);

        for _ in 0u8..3 {
            if self.bit_count >= count {
                break;
            }

            if self.position >= self.input.len() {
                return Err(self.malformed());
            }

            self.bit_buffer |= u32::from(self.input[self.position]) << self.bit_count;
            self.position += 1;
            self.bit_count += 8;
        }

        let value = self.bit_buffer & ((1u32 << count) - 1);

        self.bit_buffer >>= count;
        self.bit_count -= count;

        assert!(value < (1u32 << count));

        Ok(value)
    }

    const fn align_to_byte(&mut self) {
        self.bit_buffer = 0;
        self.bit_count = 0;
    }

    fn decode(&mut self, huffman: &Huffman) -> Result<u16> {
        let mut code: u32 = 0;
        let mut first: u32 = 0;
        let mut index: u32 = 0;

        assert!(self.bit_count <= 32);

        for length in 1..=CODE_LENGTH_MAX {
            code |= self.take(1)?;

            let count = u32::from(huffman.count[length]);

            if code < first + count {
                assert!(code >= first);

                return Ok(huffman.symbol[(index + (code - first)) as usize]);
            }

            index += count;
            first += count;
            first <<= 1u32;
            code <<= 1u32;
        }

        Err(self.malformed())
    }
}

#[derive(Debug)]
struct Output<'a> {
    buffer: &'a mut [u8],
    length: usize,
}

#[derive(Clone, Copy, Debug)]
struct BackReference {
    distance: u32,
    length: u32,
}

impl Output<'_> {
    fn write(&mut self, byte: u8) -> bool {
        assert!(self.length <= self.buffer.len());

        if self.length >= self.buffer.len() {
            return false;
        }

        self.buffer[self.length] = byte;
        self.length += 1;

        assert!(self.length <= self.buffer.len());

        true
    }

    fn copy_back(&mut self, reference: BackReference) -> bool {
        assert!(self.length <= self.buffer.len());
        assert!(reference.distance >= 1);

        let distance = reference.distance as usize;
        let length = reference.length as usize;

        if distance > self.length {
            return false;
        }

        if self.length + length > self.buffer.len() {
            return false;
        }

        for _ in 0..length {
            self.buffer[self.length] = self.buffer[self.length - distance];
            self.length += 1;
        }

        assert!(self.length <= self.buffer.len());

        true
    }
}

pub(crate) fn inflate(input: &[u8], output: &mut [u8]) -> Result<usize> {
    assert!(input.len() <= PART_BYTES_MAX as usize);

    let mut bits = Bits { bit_buffer: 0, bit_count: 0, input, position: 0 };
    let mut out = Output { buffer: output, length: 0 };
    let mut codes = Codes::EMPTY;

    for _ in 0..=input.len() {
        let last = bits.take(1)? == 1;
        let kind = bits.take(2)?;

        match kind {
            0 => block_stored(&mut bits, &mut out)?,
            1 => {
                codes_fixed(&mut codes);
                block_codes(&mut bits, &mut out, &codes)?;
            }
            2 => {
                codes_dynamic(&mut bits, &mut codes)?;
                block_codes(&mut bits, &mut out, &codes)?;
            }
            _ => return Err(bits.malformed()),
        }

        if last {
            assert!(out.length <= out.buffer.len());

            return Ok(out.length);
        }
    }

    Err(bits.malformed())
}

fn block_stored(bits: &mut Bits<'_>, out: &mut Output<'_>) -> Result<()> {
    assert!(bits.position <= bits.input.len());
    assert!(out.length <= out.buffer.len());

    bits.align_to_byte();

    let header_end = bits.position + 4;

    if header_end > bits.input.len() {
        return Err(bits.malformed());
    }

    let header = &bits.input[bits.position..header_end];

    assert!(header.len() == 4);

    let length = u16::from_le_bytes([header[0], header[1]]);
    let stored_length_complement = u16::from_le_bytes([header[2], header[3]]);

    if length != !stored_length_complement {
        return Err(bits.malformed());
    }

    let data_end = header_end + usize::from(length);

    if data_end > bits.input.len() {
        return Err(bits.malformed());
    }

    for &byte in &bits.input[header_end..data_end] {
        if !out.write(byte) {
            return Err(bits.malformed());
        }
    }

    bits.position = data_end;

    Ok(())
}

fn codes_fixed(codes: &mut Codes) {
    let mut lengths = [0u8; LITERAL_SYMBOL_COUNT_MAX];

    lengths[..144].fill(8);
    lengths[144..256].fill(9);
    lengths[256..280].fill(7);
    lengths[280..288].fill(8);

    assert!(codes.literal.build(&lengths));
    assert!(codes.distance.build(&[5u8; DISTANCE_SYMBOL_COUNT_MAX]));
}

fn codes_dynamic(bits: &mut Bits<'_>, codes: &mut Codes) -> Result<()> {
    assert!(bits.position <= bits.input.len());

    let literal_count = bits.take(5)? as usize + 257;
    let distance_count = bits.take(5)? as usize + 1;
    let code_length_count = bits.take(4)? as usize + 4;

    if literal_count > 286 {
        return Err(bits.malformed());
    }

    if distance_count > DISTANCE_SYMBOL_COUNT_MAX {
        return Err(bits.malformed());
    }

    assert!(code_length_count <= CODE_LENGTH_ORDER.len());

    let mut lengths = [0u8; LITERAL_SYMBOL_COUNT_MAX + DISTANCE_SYMBOL_COUNT_MAX];

    for &order in &CODE_LENGTH_ORDER[..code_length_count] {
        lengths[order] = u8_from_u32(bits.take(3)?);
    }

    let mut code_lengths = Huffman::EMPTY;

    if !code_lengths.build(&lengths[..19]) {
        return Err(bits.malformed());
    }

    let total = literal_count + distance_count;

    code_lengths_read(bits, &code_lengths, &mut lengths[..total])?;

    if lengths[usize::from(LITERAL_SYMBOL_END)] == 0 {
        return Err(bits.malformed());
    }

    if !codes.literal.build(&lengths[..literal_count]) {
        return Err(bits.malformed());
    }

    if !codes.distance.build(&lengths[literal_count..total]) {
        return Err(bits.malformed());
    }

    Ok(())
}

fn code_lengths_read(
    bits: &mut Bits<'_>,
    code_lengths: &Huffman,
    lengths: &mut [u8],
) -> Result<()> {
    assert!(bits.position <= bits.input.len());
    assert!(lengths.len() <= LITERAL_SYMBOL_COUNT_MAX + DISTANCE_SYMBOL_COUNT_MAX);

    let total = lengths.len();
    let mut index = 0usize;

    for _ in 0..total {
        if index >= total {
            break;
        }

        let symbol = bits.decode(code_lengths)?;

        if symbol < 16 {
            lengths[index] = u8_from_u32(u32::from(symbol));
            index += 1;

            continue;
        }

        let (repeat_value, repeat_count) = match symbol {
            16 => {
                if index == 0 {
                    return Err(bits.malformed());
                }

                (lengths[index - 1], 3 + bits.take(2)? as usize)
            }
            17 => (0, 3 + bits.take(3)? as usize),
            _ => (0, 11 + bits.take(7)? as usize),
        };

        if index + repeat_count > total {
            return Err(bits.malformed());
        }

        lengths[index..index + repeat_count].fill(repeat_value);
        index += repeat_count;

        assert!(index <= total);
    }

    assert!(index == total);

    Ok(())
}

fn block_codes(bits: &mut Bits<'_>, out: &mut Output<'_>, codes: &Codes) -> Result<()> {
    assert!(bits.position <= bits.input.len());
    assert!(out.length <= out.buffer.len());

    for _ in 0..=out.buffer.len() {
        let symbol = bits.decode(&codes.literal)?;

        if symbol < LITERAL_SYMBOL_END {
            if !out.write(u8_from_u32(u32::from(symbol))) {
                return Err(bits.malformed());
            }

            continue;
        }

        if symbol == LITERAL_SYMBOL_END {
            return Ok(());
        }

        let symbol_length_index = usize::from(symbol - LITERAL_SYMBOL_LENGTH_FIRST);

        if symbol_length_index >= SYMBOL_LENGTH_BASE.len() {
            return Err(bits.malformed());
        }

        let length = u32::from(SYMBOL_LENGTH_BASE[symbol_length_index])
            + bits.take(u32::from(SYMBOL_LENGTH_EXTRA[symbol_length_index]))?;

        let distance_index = usize::from(bits.decode(&codes.distance)?);

        if distance_index >= DISTANCE_BASE.len() {
            return Err(bits.malformed());
        }

        let distance = u32::from(DISTANCE_BASE[distance_index])
            + bits.take(u32::from(DISTANCE_EXTRA[distance_index]))?;

        if !out.copy_back(BackReference { distance, length }) {
            return Err(bits.malformed());
        }
    }

    Err(bits.malformed())
}

#[cfg(test)]
mod tests {
    use super::inflate;

    const HELLO_DEFLATE: &[u8] = &[0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0x01];

    #[test]
    fn inflates_fixed_block_with_back_reference() {
        let mut output = [0u8; 64];
        let length = inflate(HELLO_DEFLATE, &mut output).unwrap();

        assert_eq!(&output[..length], b"hello hello hello hello");
    }

    #[test]
    fn inflates_stored_block() {
        let input = [0x01, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c'];
        let mut output = [0u8; 8];
        let length = inflate(&input, &mut output).unwrap();

        assert_eq!(&output[..length], b"abc");
    }

    #[test]
    fn rejects_truncated_and_oversized() {
        let mut output = [0u8; 64];

        inflate(&HELLO_DEFLATE[..4], &mut output).unwrap_err();
        let mut small = [0u8; 4];

        inflate(HELLO_DEFLATE, &mut small).unwrap_err();
    }
}
