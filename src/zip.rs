use crate::bytes::{
    Sink,
    range_from_u32,
    range_from_usize,
    u8_from_usize,
    u16_from_usize,
    u32_from_usize,
};
use crate::error::{Error, Result};
use crate::inflate::inflate;
use core::ops::Range;

const CENTRAL_HEADER_LENGTH: u32 = 46;
const CENTRAL_HEADER_SIGNATURE: u32 = 0x0201_4b50;
const END_RECORD_LENGTH: u32 = 22;
const END_RECORD_SEARCH_MAX: u32 = END_RECORD_LENGTH + u16::MAX as u32;
const END_RECORD_SIGNATURE: u32 = 0x0605_4b50;
const ENTRY_COUNT_MAX: u32 = 16;
const ENTRY_NAME_LENGTH_MAX: usize = 64;
const LOCAL_HEADER_LENGTH: u32 = 30;
const LOCAL_HEADER_SIGNATURE: u32 = 0x0403_4b50;
const METHOD_DEFLATE: u16 = 8;
const METHOD_STORED: u16 = 0;
const VERSION_NEEDED: u16 = 20;
const DOS_DATE_EPOCH: u16 = (1 << 5) | 1;
const DOS_TIME_EPOCH: u16 = 0;
const CRC_TABLE: [u32; 256] = crc_table_build();

const fn crc_table_build() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index: u32 = 0;

    while index < 256 {
        let mut value = index;
        let mut bit = 0u32;

        while bit < 8 {
            if value & 1 == 1 {
                value = 0xedb8_8320 ^ (value >> 1u32);
            } else {
                value >>= 1u32;
            }

            bit += 1;
        }

        table[index as usize] = value;
        index += 1;
    }

    assert!(table[1] == 0x7707_3096);
    assert!(table[255] == 0x2d02_ef8d);

    table
}

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;

    for &byte in bytes {
        crc = CRC_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8u32);
    }

    !crc
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    assert!(offset + 2 <= bytes.len());

    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    assert!(offset + 4 <= bytes.len());

    u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

fn malformed(offset: usize) -> Error {
    Error::ZipMalformed { offset: u32_from_usize(offset) }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ZipEntry {
    pub(crate) crc: u32,
    pub(crate) data: Range<u32>,
    pub(crate) method: u16,
    pub(crate) name: Range<u32>,
    pub(crate) size_uncompressed: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ZipArchive<'a> {
    bytes: &'a [u8],
    directory_start: u32,
    end_record: u32,
    entry_count: u32,
}

impl<'a> ZipArchive<'a> {
    pub(crate) fn open(bytes: &'a [u8]) -> Result<Self> {
        if u32::try_from(bytes.len()).is_err() {
            return Err(Error::InputCapacity { capacity_bytes: u32::MAX });
        }

        let end_record = end_record_find(bytes)?;
        let entry_count = u32::from(u16_at(bytes, end_record + 10));
        let directory_start = u32_at(bytes, end_record + 16);

        if directory_start as usize > end_record {
            return Err(malformed(end_record));
        }

        assert!(end_record + END_RECORD_LENGTH as usize <= bytes.len());

        Ok(Self { bytes, directory_start, end_record: u32_from_usize(end_record), entry_count })
    }

    pub(crate) fn entry_find(&self, name: &[u8]) -> Result<Option<ZipEntry>> {
        assert!(!name.is_empty());
        assert!(self.directory_start <= self.end_record);

        let archive = self.bytes;
        let end_record = self.end_record as usize;
        let mut offset = self.directory_start as usize;

        for _ in 0..self.entry_count {
            if offset + CENTRAL_HEADER_LENGTH as usize > end_record {
                return Err(malformed(offset));
            }

            if u32_at(archive, offset) != CENTRAL_HEADER_SIGNATURE {
                return Err(malformed(offset));
            }

            let name_length = usize::from(u16_at(archive, offset + 28));
            let extra_length = usize::from(u16_at(archive, offset + 30));
            let comment_length = usize::from(u16_at(archive, offset + 32));
            let name_start = offset + CENTRAL_HEADER_LENGTH as usize;
            let name_end = name_start + name_length;

            if name_end > end_record {
                return Err(malformed(offset));
            }

            if &archive[name_start..name_end] == name {
                let entry = entry_from_central(archive, offset, name_start..name_end)?;

                return Ok(Some(entry));
            }

            offset = name_end + extra_length + comment_length;
        }

        Ok(None)
    }

    pub(crate) fn entry_extract(
        &self,
        entry: &ZipEntry,
        name: &'static str,
        output: &mut [u8],
    ) -> Result<usize> {
        assert!(entry.data.start <= entry.data.end);
        assert!(entry.data.end as usize <= self.bytes.len());
        assert!(!name.is_empty());

        let data = &self.bytes[range_from_u32(&entry.data)];
        let expected = entry.size_uncompressed as usize;

        if expected > output.len() {
            return Err(Error::ZipEntryTooLarge { name });
        }

        let length = match entry.method {
            METHOD_STORED => {
                if data.len() != expected {
                    return Err(Error::ZipMalformed { offset: entry.data.start });
                }

                output[..data.len()].copy_from_slice(data);

                data.len()
            }
            METHOD_DEFLATE => inflate(data, &mut output[..expected])?,
            method => return Err(Error::ZipUnsupported { method }),
        };

        if length != expected {
            return Err(Error::ZipMalformed { offset: entry.data.start });
        }

        if crc32(&output[..length]) != entry.crc {
            return Err(Error::ZipMalformed { offset: entry.data.start });
        }

        assert!(length <= output.len());

        Ok(length)
    }
}

fn end_record_find(archive: &[u8]) -> Result<usize> {
    if archive.len() < END_RECORD_LENGTH as usize {
        return Err(malformed(0));
    }

    let search_start = archive.len().saturating_sub(END_RECORD_SEARCH_MAX as usize);
    let candidate_last = archive.len() - END_RECORD_LENGTH as usize;

    assert!(search_start <= candidate_last);

    for offset in (search_start..=candidate_last).rev() {
        if u32_at(archive, offset) == END_RECORD_SIGNATURE {
            return Ok(offset);
        }
    }

    Err(malformed(archive.len()))
}

fn entry_from_central(archive: &[u8], central: usize, name: Range<usize>) -> Result<ZipEntry> {
    assert!(central + CENTRAL_HEADER_LENGTH as usize <= archive.len());
    assert!(name.end <= archive.len());

    let size_compressed = u32_at(archive, central + 20);
    let size_uncompressed = u32_at(archive, central + 24);
    let local = u32_at(archive, central + 42) as usize;

    if size_compressed == u32::MAX {
        return Err(malformed(central));
    }

    if size_uncompressed == u32::MAX {
        return Err(malformed(central));
    }

    if local + LOCAL_HEADER_LENGTH as usize > archive.len() {
        return Err(malformed(central));
    }

    if u32_at(archive, local) != LOCAL_HEADER_SIGNATURE {
        return Err(malformed(local));
    }

    let local_name_length = usize::from(u16_at(archive, local + 26));
    let local_extra_length = usize::from(u16_at(archive, local + 28));
    let data_start = local + LOCAL_HEADER_LENGTH as usize + local_name_length + local_extra_length;
    let data_end = data_start + size_compressed as usize;

    if data_end > archive.len() {
        return Err(malformed(local));
    }

    assert!(data_start <= data_end);

    let method = u16_at(archive, central + 10);
    let crc = u32_at(archive, central + 16);
    let data = range_from_usize(&(data_start..data_end));

    Ok(ZipEntry { crc, data, method, name: range_from_usize(&name), size_uncompressed })
}

#[derive(Clone, Copy, Debug)]
struct WriterEntry {
    crc: u32,
    local_offset: u32,
    name: [u8; ENTRY_NAME_LENGTH_MAX],
    name_length: u8,
    size: u32,
}

impl WriterEntry {
    const EMPTY: Self =
        Self { crc: 0, local_offset: 0, name: [0; ENTRY_NAME_LENGTH_MAX], name_length: 0, size: 0 };
}

#[derive(Debug)]
pub(crate) struct ZipWriter<'a, 'b> {
    data_start: u32,
    entries: [WriterEntry; ENTRY_COUNT_MAX as usize],
    entry_count: u32,
    entry_open: bool,
    sink: &'b mut Sink<'a>,
}

impl<'a, 'b> ZipWriter<'a, 'b> {
    pub(crate) fn new(sink: &'b mut Sink<'a>) -> Self {
        assert!(sink.length() == 0);

        Self {
            data_start: 0,
            entries: [WriterEntry::EMPTY; ENTRY_COUNT_MAX as usize],
            entry_count: 0,
            entry_open: false,
            sink,
        }
    }

    pub(crate) fn entry_begin(&mut self, name: &[u8]) -> Result<()> {
        assert!(!self.entry_open);
        assert!(self.entry_count < ENTRY_COUNT_MAX);
        assert!(!name.is_empty());
        assert!(name.len() <= ENTRY_NAME_LENGTH_MAX);

        let entry = &mut self.entries[self.entry_count as usize];
        entry.local_offset = self.sink.length();

        entry.name[..name.len()].copy_from_slice(name);
        entry.name_length = u8_from_usize(name.len());

        self.sink.write_u32_le(LOCAL_HEADER_SIGNATURE)?;
        self.sink.write_u16_le(VERSION_NEEDED)?;
        self.sink.write_u16_le(0)?;
        self.sink.write_u16_le(METHOD_STORED)?;
        self.sink.write_u16_le(DOS_TIME_EPOCH)?;
        self.sink.write_u16_le(DOS_DATE_EPOCH)?;
        self.sink.write_u32_le(0)?;
        self.sink.write_u32_le(0)?;
        self.sink.write_u32_le(0)?;
        self.sink.write_u16_le(u16_from_usize(name.len()))?;
        self.sink.write_u16_le(0)?;
        self.sink.write(name)?;

        self.data_start = self.sink.length();
        self.entry_open = true;

        assert!(
            self.data_start
                == entry.local_offset + LOCAL_HEADER_LENGTH + u32_from_usize(name.len())
        );

        Ok(())
    }

    pub(crate) fn sink(&mut self) -> &mut Sink<'a> {
        assert!(self.entry_open);

        self.sink
    }

    pub(crate) fn entry_end(&mut self) {
        assert!(self.entry_open);
        assert!(self.sink.length() >= self.data_start);

        let size = self.sink.length() - self.data_start;
        let crc = crc32(&self.sink.written()[self.data_start as usize..]);
        let entry = &mut self.entries[self.entry_count as usize];
        entry.crc = crc;
        entry.size = size;

        let header = entry.local_offset as usize;

        self.sink.write_u32_le_at(header + 14, crc);
        self.sink.write_u32_le_at(header + 18, size);
        self.sink.write_u32_le_at(header + 22, size);

        self.entry_count += 1;
        self.entry_open = false;

        assert!(self.entry_count <= ENTRY_COUNT_MAX);
    }

    pub(crate) fn finish(mut self) -> Result<()> {
        assert!(!self.entry_open);
        assert!(self.entry_count >= 1);

        let directory_start = self.sink.length();

        self.central_directory_write()?;

        let directory_end = self.sink.length();

        self.end_record_write(directory_start..directory_end)?;

        assert!(self.sink.length() == directory_end + END_RECORD_LENGTH);

        Ok(())
    }

    fn central_directory_write(&mut self) -> Result<()> {
        assert!(!self.entry_open);
        assert!(self.entry_count <= ENTRY_COUNT_MAX);

        for entry in &self.entries[..self.entry_count as usize] {
            self.sink.write_u32_le(CENTRAL_HEADER_SIGNATURE)?;
            self.sink.write_u16_le(VERSION_NEEDED)?;
            self.sink.write_u16_le(VERSION_NEEDED)?;
            self.sink.write_u16_le(0)?;
            self.sink.write_u16_le(METHOD_STORED)?;
            self.sink.write_u16_le(DOS_TIME_EPOCH)?;
            self.sink.write_u16_le(DOS_DATE_EPOCH)?;
            self.sink.write_u32_le(entry.crc)?;
            self.sink.write_u32_le(entry.size)?;
            self.sink.write_u32_le(entry.size)?;

            let name = &entry.name[..usize::from(entry.name_length)];

            self.sink.write_u16_le(u16_from_usize(name.len()))?;
            self.sink.write_u16_le(0)?;
            self.sink.write_u16_le(0)?;
            self.sink.write_u16_le(0)?;
            self.sink.write_u16_le(0)?;
            self.sink.write_u32_le(0)?;
            self.sink.write_u32_le(entry.local_offset)?;
            self.sink.write(name)?;
        }

        Ok(())
    }

    fn end_record_write(&mut self, directory: Range<u32>) -> Result<()> {
        assert!(directory.start <= directory.end);
        assert!(self.sink.length() == directory.end);

        self.sink.write_u32_le(END_RECORD_SIGNATURE)?;
        self.sink.write_u16_le(0)?;
        self.sink.write_u16_le(0)?;
        self.sink.write_u16_le(u16_from_usize(self.entry_count as usize))?;
        self.sink.write_u16_le(u16_from_usize(self.entry_count as usize))?;
        self.sink.write_u32_le(directory.end - directory.start)?;
        self.sink.write_u32_le(directory.start)?;
        self.sink.write_u16_le(0)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{METHOD_STORED, ZipArchive, ZipWriter, crc32};
    use crate::bytes::Sink;

    #[test]
    fn crc32_matches_reference() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn written_archive_reads_back() {
        let mut buffer = [0u8; 512];
        let mut sink = Sink::new(&mut buffer);
        let mut writer = ZipWriter::new(&mut sink);

        writer.entry_begin(b"a.txt").unwrap();
        writer.sink().write(b"hello").unwrap();
        writer.entry_end();
        writer.entry_begin(b"dir/b.txt").unwrap();
        writer.sink().write(b"world!").unwrap();
        writer.entry_end();
        writer.finish().unwrap();

        let archive = ZipArchive::open(sink.written()).unwrap();
        let entry = archive.entry_find(b"dir/b.txt").unwrap().unwrap();

        assert_eq!(entry.method, METHOD_STORED);
        assert_eq!(entry.size_uncompressed, 6);

        let mut output = [0u8; 16];
        let length = archive.entry_extract(&entry, "b", &mut output).unwrap();

        assert_eq!(&output[..length], b"world!");
        assert_eq!(archive.entry_find(b"missing").unwrap(), None);
    }

    #[test]
    fn rejects_garbage() {
        ZipArchive::open(b"not a zip archive at all, definitely").unwrap_err();
        ZipArchive::open(b"PK").unwrap_err();
    }
}
