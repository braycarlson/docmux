mod read;
mod write;

pub use read::{INPUT_BYTES_MAX, read};
pub use write::write;

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{read, write};
    use crate::markdown;
    use crate::test_support::with_workspace;
    use std::string::String;
    use std::vec;

    fn markdown_via_adf(source: &str) -> (String, String) {
        with_workspace(|workspace, document| {
            markdown::read(source.as_bytes(), markdown::Options::GFM, workspace, document).unwrap();

            let mut adf = vec![0u8; 1 << 16];
            let adf_length = write(document, &mut adf).unwrap() as usize;

            assert!(adf_length > 0);

            let adf_text = String::from_utf8(adf[..adf_length].to_vec()).unwrap();

            read(&adf[..adf_length], document).unwrap();

            let mut output = vec![0u8; 1 << 16];
            let length = markdown::write(document, &mut output).unwrap() as usize;

            assert!(length <= output.len());

            (String::from_utf8(output[..length].to_vec()).unwrap(), adf_text)
        })
    }

    fn assert_survives(source: &str) {
        let (round_tripped, adf) = markdown_via_adf(source);

        assert!(!adf.is_empty());
        assert_eq!(round_tripped, source, "adf was: {adf}");
    }

    #[test]
    fn blocks_survive_adf() {
        assert_survives(
            "# Title\n\nPara with **bold**, *em*, ~~strike~~, `code`, [link](https://x.y).\n",
        );

        assert_survives("- one\n- two\n  - nested\n\n1. first\n2. second\n");
        assert_survives("> quoted\n\n```rust\nfn x() {}\n```\n\n---\n");
        assert_survives("| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        assert_survives("- [ ] todo\n- [x] done\n");
        assert_survives("line\\\nbreak\n");
    }

    #[test]
    fn adf_shape_matches_atlassian() {
        let (_, adf) = markdown_via_adf("# H\n\ntext **b**\n\n- [x] t\n");

        assert!(adf.starts_with("{\"version\":1,\"type\":\"doc\",\"content\":["));

        assert!(adf.contains(concat!(
            "{\"type\":\"heading\",\"attrs\":{\"level\":1},",
            "\"content\":[{\"type\":\"text\",\"text\":\"H\"}]}",
        )));

        assert!(
            adf.contains("{\"type\":\"text\",\"text\":\"b\",\"marks\":[{\"type\":\"strong\"}]}")
        );

        assert!(adf.contains(
            "\"type\":\"taskItem\",\"attrs\":{\"localId\":\"task-1\",\"state\":\"DONE\"}"
        ));
    }

    #[test]
    fn heading_in_quote_degrades_to_strong_paragraph() {
        let (round_tripped, _) = markdown_via_adf("> # inside\n");

        assert_eq!(round_tripped, "> **inside**\n");
    }

    #[test]
    fn reads_confluence_specific_nodes() {
        let source = concat!(
            "{\"type\":\"doc\",\"version\":1,\"content\":[",
            "{\"type\":\"panel\",\"attrs\":{\"panelType\":\"info\"},\"content\":[",
            "{\"type\":\"paragraph\",\"content\":[{\"type\":\"text\",\"text\":\"note\"}]}]},",
            "{\"type\":\"paragraph\",\"content\":[",
            "{\"type\":\"mention\",\"attrs\":{\"id\":\"1\",\"text\":\"@Brayden\"}},",
            "{\"type\":\"text\",\"text\":\" on \"},",
            "{\"type\":\"date\",\"attrs\":{\"timestamp\":\"1704067200000\"}},",
            "{\"type\":\"text\",\"text\":\" \"},",
            "{\"type\":\"emoji\",\"attrs\":{\"shortName\":\":smile:\",",
            "\"text\":\"\\ud83d\\ude04\"}},",
            "{\"type\":\"text\",\"text\":\" \"},",
            "{\"type\":\"inlineCard\",\"attrs\":{\"url\":\"https://a.b/\"}}]},",
            "{\"type\":\"expand\",\"attrs\":{\"title\":\"More\"},\"content\":[",
            "{\"type\":\"paragraph\",\"content\":[{\"type\":\"text\",\"text\":\"hidden\"}]}]},",
            "{\"type\":\"layoutSection\",\"content\":[{\"type\":\"layoutColumn\",",
            "\"attrs\":{\"width\":50},\"content\":[{\"type\":\"paragraph\",\"content\":[",
            "{\"type\":\"text\",\"text\":\"col\"}]}]}]},",
            "{\"type\":\"table\",\"content\":[{\"type\":\"tableRow\",\"content\":[",
            "{\"type\":\"tableHeader\",\"attrs\":{\"colspan\":2},\"content\":[",
            "{\"type\":\"paragraph\",\"content\":[{\"type\":\"text\",\"text\":\"wide\"}]}]}]},",
            "{\"type\":\"tableRow\",\"content\":[",
            "{\"type\":\"tableCell\",\"content\":[{\"type\":\"paragraph\",\"content\":[",
            "{\"type\":\"text\",\"text\":\"a\"}]}]},",
            "{\"type\":\"tableCell\",\"content\":[{\"type\":\"paragraph\",\"content\":[",
            "{\"type\":\"text\",\"text\":\"b\"}]}]}]}]}]}",
        )
        .as_bytes();

        let output = with_workspace(|_, document| {
            read(source, document).unwrap();

            let mut output = vec![0u8; 1 << 16];
            let length = markdown::write(document, &mut output).unwrap() as usize;

            assert!(length > 0);

            String::from_utf8(output[..length].to_vec()).unwrap()
        });

        assert_eq!(
            output,
            concat!(
                "> note\n\n@Brayden on 2024-01-01 \u{1F604} [https://a.b/](https://a.b/)\n\n",
                "> **More**\n>\n> hidden\n\ncol\n\n| wide | |\n| --- | --- |\n| a | b |\n",
            ),
        );
    }

    #[test]
    fn rejects_non_document() {
        with_workspace(|_, document| {
            assert!(read(b"{\"type\":\"paragraph\"}", document).is_err());
            assert!(read(b"[]", document).is_err());

            assert!(
                read(b"{\"type\":\"doc\",\"content\":[{\"type\":\"text\"}]}", document).is_err()
            );
        });
    }
}
