pub(crate) mod inline;

mod block;
mod write;

use crate::bytes::utf8_validate;
use crate::document::Document;
use crate::error::{Error, Result};
use crate::workspace::{PART_BYTES_MAX, Workspace};

pub const INPUT_BYTES_MAX: u32 = PART_BYTES_MAX;

pub use write::write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    bits: u8,
}

impl Options {
    pub const AUTOLINKS_RAW: Self = Self { bits: 1 << 0 };
    pub const COMMONMARK: Self = Self { bits: 0 };

    pub const GFM: Self = Self {
        bits: Self::AUTOLINKS_RAW.bits
            | Self::STRIKETHROUGH.bits
            | Self::TABLES.bits
            | Self::TASK_ITEMS.bits,
    };

    pub const STRIKETHROUGH: Self = Self { bits: 1 << 1 };
    pub const TABLES: Self = Self { bits: 1 << 2 };
    pub const TASK_ITEMS: Self = Self { bits: 1 << 3 };

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.bits & other.bits == other.bits
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self { bits: self.bits | other.bits }
    }
}

pub fn read(
    source: &[u8],
    options: Options,
    workspace: &mut Workspace,
    document: &mut Document,
) -> Result<()> {
    if source.len() > INPUT_BYTES_MAX as usize {
        return Err(Error::InputCapacity { capacity_bytes: INPUT_BYTES_MAX });
    }

    utf8_validate(source)?;
    document.reset();
    workspace.reset();

    assert!(document.node_count() == 1);
    assert!(workspace.part_length == 0);

    let mut parser = block::BlockParser::new(options);
    let mut lines = source.split(|&byte| byte == b'\n');

    for _ in 0..=source.len() {
        let Some(line) = lines.next() else {
            break;
        };

        let line = line.strip_suffix(b"\r").unwrap_or(line);

        parser.line_process(line, workspace, document)?;
    }

    parser.finish(workspace, document)?;

    assert!(document.node_count() >= 1);

    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{Options, read, write};
    use crate::test_support::with_workspace;
    use std::string::String;
    use std::vec;

    fn round_trip(source: &str) -> String {
        with_workspace(|workspace, document| {
            read(source.as_bytes(), Options::GFM, workspace, document).unwrap();

            let mut output = vec![0u8; 1 << 16];
            let length = write(document, &mut output).unwrap();

            String::from_utf8(output[..length].to_vec()).unwrap()
        })
    }

    fn assert_stable(source: &str) {
        let first = round_trip(source);

        assert_eq!(first, source);

        let second = round_trip(&first);

        assert_eq!(second, first);
    }

    #[test]
    fn options_compose() {
        assert!(Options::GFM.contains(Options::TABLES));
        assert!(!Options::COMMONMARK.contains(Options::TABLES));
        assert_eq!(Options::COMMONMARK.union(Options::TABLES), Options::TABLES);
    }

    #[test]
    fn headings_and_paragraphs() {
        assert_stable("# Title\n\nSome text here.\n\n## Second\n\nMore text.\n");
        assert_eq!(round_trip("Title\n=====\n\nSub\n---\n"), "# Title\n\n## Sub\n");
        assert_eq!(round_trip("# Closed ##\n"), "# Closed\n");
        assert_eq!(round_trip("line one\nline two\n"), "line one line two\n");
    }

    #[test]
    fn emphasis_and_code() {
        assert_stable("This is **bold** and *italic* and ~~gone~~ and `code`.\n");
        assert_eq!(round_trip("***both***\n"), "***both***\n");
        assert_eq!(round_trip("__bold__ _em_\n"), "**bold** *em*\n");
        assert_eq!(round_trip("a * b * c\n"), "a \\* b \\* c\n");
        assert_eq!(round_trip("`` a`b ``\n"), "``a`b``\n");
        assert_eq!(round_trip("**bold *nested* bold**\n"), "**bold *nested* bold**\n");
        assert_eq!(round_trip("** not bold **\n"), "\\*\\* not bold \\*\\*\n");
    }

    #[test]
    fn links_and_images() {
        assert_stable("A [link](https://example.com) and ![alt](img.png) here.\n");
        assert_eq!(round_trip("[ref][r]\n\n[r]: https://x.y/z\n"), "[ref](https://x.y/z)\n");
        assert_eq!(round_trip("<https://a.b/c>\n"), "[https://a.b/c](https://a.b/c)\n");
        assert_eq!(round_trip("see https://a.b/c.\n"), "see [https://a.b/c](https://a.b/c).\n");
        assert_eq!(round_trip("[**bold** link](u)\n"), "[**bold** link](u)\n");
        assert_eq!(round_trip("[a](<b c> \"t\")\n"), "[a](b%20c)\n");
    }

    #[test]
    fn lists() {
        assert_stable("- one\n- two\n  - nested\n  - again\n- three\n");
        assert_stable("1. first\n2. second\n3. third\n");
        assert_eq!(round_trip("3) a\n4) b\n"), "3. a\n4. b\n");
        assert_stable("- [ ] todo\n- [x] done\n");

        assert_eq!(
            round_trip("- para one\n\n  para two\n- next\n"),
            "- para one\n\n  para two\n\n- next\n",
        );

        assert_stable("- para one\n\n  para two\n\n- next\n");
        assert_stable("- tight\n  - nested\n- items\n");
        assert_eq!(round_trip("- a\n\n\n- b\n"), "- a\n\n- b\n");
        assert_eq!(round_trip("- a\n* b\n"), "- a\n\n* b\n");
        assert_eq!(round_trip("- a\nlazy\n"), "- a lazy\n");
        assert_eq!(round_trip("- a\n\nb\n"), "- a\n\nb\n");

        assert_eq!(
            round_trip("1. a\n\n   ```\n   code\n   ```\n"),
            "1. a\n\n   ```\n   code\n   ```\n",
        );
    }

    #[test]
    fn block_quotes_and_code() {
        assert_eq!(round_trip("> quoted\n> more\n"), "> quoted more\n");
        assert_stable("> level one\n>\n> > level two\n");
        assert_stable("```rust\nfn main() {}\n```\n");
        assert_eq!(round_trip("    indented\n    code\n"), "```\nindented\ncode\n```\n");
        assert_eq!(round_trip("~~~\nx\n~~~\n"), "```\nx\n```\n");
        assert_eq!(round_trip("```\na ``` b\n```\n"), "````\na ``` b\n````\n");
        assert_stable("> - item\n> - item\n");
    }

    #[test]
    fn tables_and_breaks() {
        assert_stable("| a | b |\n| --- | :-: |\n| 1 | 2 |\n");
        assert_eq!(round_trip("a|b\n-|-\n1|2\n"), "| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        assert_eq!(round_trip("| a |\n| - |\n| x \\| y |\n"), "| a |\n| --- |\n| x \\| y |\n");
        assert_stable("first\\\nsecond\n");
        assert_eq!(round_trip("first  \nsecond\n"), "first\\\nsecond\n");
        assert_stable("above\n\n---\n\nbelow\n");
        assert_eq!(round_trip("* * *\n"), "---\n");
    }

    #[test]
    fn escapes_and_entities() {
        assert_eq!(round_trip("\\*literal\\*\n"), "\\*literal\\*\n");
        assert_eq!(round_trip("&amp; &lt; &copy; &#65;\n"), "& < \u{a9} A\n");
        assert_eq!(round_trip("\\# not heading\n"), "\\# not heading\n");
        assert_eq!(round_trip("1\\. not list\n"), "1\\. not list\n");
    }

    #[test]
    fn rejects_invalid_utf8() {
        with_workspace(|workspace, document| {
            assert!(read(b"\xff\xfe", Options::GFM, workspace, document).is_err());
        });
    }
}
