use docmux::{DEPTH_MAX, Error, NodeKind, TABLE_COLSPAN_MAX, adf, docx, markdown};
use docmux_support::{
    DOCUMENT,
    OUTPUT,
    SCRATCH,
    WORKSPACE,
    conversion_ok,
    fixture,
    guarded,
    serial,
};
use std::sync::PoisonError;

const LIBREOFFICE_EXPECTED: &str = concat!(
    "# Fixture Heading\n\nPlain paragraph with **bold**, *italic*, ~~strike~~ and a ",
    "[link](https://example.com/x?y=1).\n\n## Lists\n\n- Bullet one\n- Bullet two\n",
    "  - Nested bullet\n\n1. First\n2. Second\n\n5) Fifth\n6) Sixth\n\n> Quoted text\n\n",
    "```\nlet x = 1;\nlet y = 2;\n```\n\n| Name | Value |\n| :-: | :-: |\n| a | 1 |\n",
    "| wide | |\n\nLine with\\\na break and a tab\there.\n\n### Third level\n\n",
    "Unicode: caf\u{e9} & \\<tags> \u{201c}quotes\u{201d}\n",
);

const SAMPLE_VIA_DOCX_EXPECTED: &str = concat!(
    "# Meeting notes\n\nAttendees: **Alice**, *Bob*, ~~Carol~~ and `dave@example.com`.\n\n",
    "## Decisions\n\n1. Ship the **release** on Friday.\n2. Move the ",
    "[design review](https://example.com/review?a=1&b=2) to next week.\n",
    "   - nested bullet with a tab\tinside\n   - second nested\n3. Third item\n\n",
    "- [ ] Write the changelog\n- [x] Update the docs\n\n> A quoted remark spanning lines.\n\n",
    "```\nfn main() {\n    println!(\"hi\");\n}\n```\n\n| Task | Owner | Status |\n",
    "| --- | :-: | --: |\n| Build | Alice | done |\n| Test | Bob | pending |\n\n---\n\n",
    "Final paragraph with a hard\\\nbreak and an image [diagram](https://example.com/d.png).\n",
);

const CORRUPTION_ROUND_COUNT: u32 = 300;

fn markdown_read(source: &[u8]) {
    assert!(!source.is_empty());

    let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);

    guarded(|| {
        let read = markdown::read(source, markdown::Options::GFM, &mut workspace, &mut document);

        conversion_ok("markdown read", read);
    });

    assert!(document.node_count() >= 1);
}

fn markdown_write_string() -> String {
    let document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut output = OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);

    let length =
        guarded(|| conversion_ok("markdown write", markdown::write(&document, &mut output[..])));

    assert!(length <= output.len());

    String::from_utf8(output[..length].to_vec())
        .unwrap_or_else(|error| panic!("markdown output is not UTF-8: {error}"))
}

#[test]
fn markdown_round_trip_is_stable_without_heap() {
    let _serial = serial();
    let sample = fixture("sample.md");

    markdown_read(&sample);
    let first = markdown_write_string();

    assert_eq!(first, String::from_utf8(sample).unwrap());

    markdown_read(first.as_bytes());
    assert_eq!(markdown_write_string(), first);
}

#[test]
fn markdown_to_docx_and_back() {
    let _serial = serial();
    let sample = fixture("sample.md");

    markdown_read(&sample);

    {
        let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
        let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);
        let mut output = OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
        let length = guarded(|| docx::write(&document, &mut workspace, &mut output[..]).unwrap());

        guarded(|| docx::read(&output[..length], &mut workspace, &mut document).unwrap());

        assert!(document.node_count() > 1);
    }

    assert_eq!(markdown_write_string(), SAMPLE_VIA_DOCX_EXPECTED);
}

#[test]
fn adf_colspan_beyond_the_limit_is_clamped() {
    let _serial = serial();

    let cell = concat!(
        "{\"type\":\"tableCell\",\"attrs\":{\"colspan\":4000000000},",
        "\"content\":[{\"type\":\"paragraph\",\"content\":[{\"type\":\"text\",\"text\":\"x\"}]}]}",
    );

    let source = [
        "{\"version\":1,\"type\":\"doc\",\"content\":[{\"type\":\"table\",\"content\":",
        "[{\"type\":\"tableRow\",\"content\":[",
        cell,
        ",",
        cell,
        "]}]}]}",
    ]
    .concat();

    let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);

    guarded(|| adf::read(source.as_bytes(), &mut document).unwrap());

    assert!(document.node_count() > 1);

    let NodeKind::Table { column_count: columns } = document.node(1).kind else {
        panic!("node 1 is not the table");
    };

    assert_eq!(columns, 2 * TABLE_COLSPAN_MAX);
}

#[test]
fn markdown_to_adf_and_back() {
    let _serial = serial();
    let sample = fixture("sample.md");

    markdown_read(&sample);

    {
        let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
        let mut output = OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
        let length = guarded(|| adf::write(&document, &mut output[..]).unwrap());

        guarded(|| adf::read(&output[..length], &mut document).unwrap());

        assert!(document.node_count() > 1);
    }

    let expected = String::from_utf8(sample)
        .unwrap()
        .replace("| --- | :-: | --: |", "| --- | --- | --- |")
        .replace("![diagram]", "[diagram]");

    assert_eq!(markdown_write_string(), expected);
}

#[test]
fn libreoffice_docx_reads_to_markdown() {
    let _serial = serial();

    {
        let archive = fixture("libreoffice.docx");
        let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
        let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);

        guarded(|| docx::read(&archive, &mut workspace, &mut document).unwrap());
    }

    assert_eq!(markdown_write_string(), LIBREOFFICE_EXPECTED);
}

#[test]
fn libreoffice_docx_survives_adf_and_docx_rewrites() {
    let _serial = serial();
    let archive = fixture("libreoffice.docx");
    let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut output = OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut scratch = SCRATCH.lock().unwrap_or_else(PoisonError::into_inner);

    guarded(|| docx::read(&archive, &mut workspace, &mut document).unwrap());

    let adf_length = guarded(|| adf::write(&document, &mut output[..]).unwrap());

    guarded(|| adf::read(&output[..adf_length], &mut document).unwrap());

    let docx_length = guarded(|| docx::write(&document, &mut workspace, &mut scratch[..]).unwrap());

    guarded(|| docx::read(&scratch[..docx_length], &mut workspace, &mut document).unwrap());

    let length = guarded(|| markdown::write(&document, &mut output[..]).unwrap());
    let text = String::from_utf8(output[..length].to_vec()).unwrap();
    let expected = LIBREOFFICE_EXPECTED.replace("| :-: | :-: |", "| --- | --- |");

    assert_eq!(text, expected);
}

#[test]
fn errors_are_reported_not_panicked() {
    let _serial = serial();
    let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);
    let archive = fixture("libreoffice.docx");
    let half = archive.len().div_euclid(2);
    let truncated = docx::read(&archive[..half], &mut workspace, &mut document);

    assert!(matches!(truncated, Err(Error::ZipMalformed { .. })));

    let not_zip =
        docx::read(b"PK\x03\x04 definitely not a zip archive", &mut workspace, &mut document);

    assert!(matches!(not_zip, Err(Error::ZipMalformed { .. })));

    let end_record_only = concat!(
        "PK\x05\x06",
        "\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
        "\x00\x00\x00\x00\x00\x00\x00\x00",
    )
    .as_bytes();

    let no_document = docx::read(end_record_only, &mut workspace, &mut document);

    assert!(matches!(no_document, Err(Error::DocxPartMissing { .. })));

    let mut deep = String::new();

    for _ in 0..=DEPTH_MAX {
        deep.push_str("> ");
    }

    deep.push_str("too deep\n");

    let nested =
        markdown::read(deep.as_bytes(), markdown::Options::GFM, &mut workspace, &mut document);

    assert_eq!(nested, Err(Error::DepthExceeded { depth_max: DEPTH_MAX }));

    let mut small = [0u8; 8];

    markdown::read(b"# heading\n", markdown::Options::GFM, &mut workspace, &mut document).unwrap();

    let markdown_written = markdown::write(&document, &mut small);
    let adf_written = adf::write(&document, &mut small);
    let docx_written = docx::write(&document, &mut workspace, &mut small);

    assert_eq!(markdown_written, Err(Error::OutputCapacity));
    assert_eq!(adf_written, Err(Error::OutputCapacity));
    assert_eq!(docx_written, Err(Error::OutputCapacity));
}

fn xorshift(state: &mut u32) -> u32 {
    assert!(*state != 0);

    *state ^= *state << 13u32;
    *state ^= *state >> 17u32;
    *state ^= *state << 5u32;
    let next = *state;

    assert!(next != 0);

    next
}

fn index_pick(state: &mut u32, length: usize) -> usize {
    assert!(length >= 1);

    let index = xorshift(state) as usize % length;

    assert!(index < length);

    index
}

#[test]
fn corrupted_inputs_error_instead_of_panicking() {
    let _serial = serial();
    let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut output = OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
    let docx = fixture("libreoffice.docx");
    let sample = fixture("sample.md");

    markdown::read(&sample, markdown::Options::GFM, &mut workspace, &mut document).unwrap();
    let adf_length = adf::write(&document, &mut output[..]).unwrap();
    let adf_text = output[..adf_length].to_vec();
    let mut state: u32 = 0x9e37_79b9;

    for round in 0..CORRUPTION_ROUND_COUNT {
        let mut docx_mutated = docx.clone();
        let mut adf_mutated = adf_text.clone();
        let mut markdown_mutated = sample.clone();

        for _ in 0..=round % 8 {
            let docx_index = index_pick(&mut state, docx_mutated.len());
            docx_mutated[docx_index] = u8::try_from(xorshift(&mut state) & 0xff).unwrap();
            let adf_index = index_pick(&mut state, adf_mutated.len());
            adf_mutated[adf_index] = b"{}[]\":,\\n"[index_pick(&mut state, 9)];
            let markdown_index = index_pick(&mut state, markdown_mutated.len());
            markdown_mutated[markdown_index] = b"*_`[]()<>|#-\n\\~"[index_pick(&mut state, 15)];
        }

        let truncated = index_pick(&mut state, docx_mutated.len());
        let _truncated_read = docx::read(&docx_mutated[..truncated], &mut workspace, &mut document);
        let _docx_read = docx::read(&docx_mutated, &mut workspace, &mut document);
        let _adf_read = adf::read(&adf_mutated, &mut document);
        let options = markdown::Options::GFM;

        if markdown::read(&markdown_mutated, options, &mut workspace, &mut document).is_ok() {
            let length = markdown::write(&document, &mut output[..]).unwrap();

            markdown::read(&output[..length], options, &mut workspace, &mut document).unwrap();
            adf::write(&document, &mut output[..]).unwrap();
            docx::write(&document, &mut workspace, &mut output[..]).unwrap();
        }
    }
}
