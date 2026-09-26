use core::error::Error as ErrorTrait;
use core::fmt;
use core::result::Result as CoreResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    ADFMalformed { offset: u32 },
    DeflateMalformed { offset: u32 },
    DepthExceeded { depth_max: u8 },
    DocxMalformed { offset: u32 },
    DocxPartMissing { name: &'static str },
    DocxTableCapacity { name: &'static str },
    InlineTokenCapacity { token_count_max: u32 },
    InputCapacity { capacity_bytes: u32 },
    JSONMalformed { offset: u32 },
    LinkCapacity { link_count_max: u32 },
    NodeCapacity { node_count_max: u32 },
    OutputCapacity,
    TextCapacity { capacity_bytes: u32 },
    UTF8Invalid { offset: u32 },
    WorkspaceCapacity { capacity_bytes: u32 },
    XMLMalformed { offset: u32 },
    ZipEntryTooLarge { name: &'static str },
    ZipMalformed { offset: u32 },
    ZipUnsupported { method: u16 },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ADFMalformed { offset } => {
                write!(formatter, "ADF document is malformed at byte {offset}")
            }
            Self::DeflateMalformed { offset } => {
                write!(formatter, "deflate stream is malformed at byte {offset}")
            }
            Self::DepthExceeded { depth_max } => {
                write!(formatter, "document nesting exceeds {depth_max} levels")
            }
            Self::DocxMalformed { offset } => {
                write!(formatter, "DOCX document.xml is malformed at byte {offset}")
            }
            Self::DocxPartMissing { name } => {
                write!(formatter, "DOCX package has no part named {name}")
            }
            Self::DocxTableCapacity { name } => write!(formatter, "DOCX {name} table is full"),
            Self::InlineTokenCapacity { token_count_max } => {
                write!(formatter, "paragraph has more than {token_count_max} inline tokens")
            }
            Self::InputCapacity { capacity_bytes } => {
                write!(formatter, "input is larger than {capacity_bytes} bytes")
            }
            Self::JSONMalformed { offset } => {
                write!(formatter, "JSON is malformed at byte {offset}")
            }
            Self::LinkCapacity { link_count_max } => {
                write!(formatter, "document has more than {link_count_max} links")
            }
            Self::NodeCapacity { node_count_max } => {
                write!(formatter, "document has more than {node_count_max} nodes")
            }
            Self::OutputCapacity => write!(formatter, "output buffer is full"),
            Self::TextCapacity { capacity_bytes } => {
                write!(formatter, "document text exceeds {capacity_bytes} bytes")
            }
            Self::UTF8Invalid { offset } => {
                write!(formatter, "input is not valid UTF-8 at byte {offset}")
            }
            Self::WorkspaceCapacity { capacity_bytes } => {
                write!(formatter, "workspace buffer of {capacity_bytes} bytes is full")
            }
            Self::XMLMalformed { offset } => write!(formatter, "XML is malformed at byte {offset}"),
            Self::ZipEntryTooLarge { name } => {
                write!(formatter, "zip entry {name} does not fit the workspace")
            }
            Self::ZipMalformed { offset } => {
                write!(formatter, "zip archive is malformed at byte {offset}")
            }
            Self::ZipUnsupported { method } => {
                write!(formatter, "zip compression method {method} is not supported")
            }
        }
    }
}

impl ErrorTrait for Error {}

pub type Result<T> = CoreResult<T, Error>;
