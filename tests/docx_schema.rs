use docmux::{docx, markdown};
use docmux_support::{DOCUMENT, OUTPUT, WORKSPACE, guarded, serial};
use std::fs;
use std::path::Path;
use std::str;
use std::sync::PoisonError;

const DOCUMENT_COUNT_MIN: u32 = 200;
const PART_COUNT: u32 = 6;

const SEQUENCES: &[(&str, &str)] = &[
    ("abstractNum", "nsid multiLevelType tmpl name styleLink numStyleLink lvl"),
    ("docDefaults", "rPrDefault pPrDefault"),
    (
        "lvl",
        "start numFmt lvlRestart pStyle isLgl suff lvlText lvlPicBulletId legacy lvlJc pPr rPr",
    ),
    ("lvlOverride", "startOverride lvl"),
    ("num", "abstractNumId lvlOverride"),
    ("numPr", "ilvl numId numberingChange ins"),
    ("pBdr", "top left bottom right between bar"),
    (
        "pPr",
        concat!(
            "pStyle keepNext keepLines pageBreakBefore framePr widowControl numPr ",
            "suppressLineNumbers pBdr shd tabs suppressAutoHyphens kinsoku wordWrap ",
            "overflowPunct topLinePunct autoSpaceDE autoSpaceDN bidi adjustRightInd snapToGrid ",
            "spacing ind contextualSpacing mirrorIndents suppressOverlap jc textDirection ",
            "textAlignment textboxTightWrap outlineLvl divId cnfStyle rPr sectPr pPrChange",
        ),
    ),
    (
        "rPr",
        concat!(
            "rStyle rFonts b bCs i iCs caps smallCaps strike dstrike outline shadow emboss ",
            "imprint noProof snapToGrid vanish webHidden color spacing w kern position sz szCs ",
            "highlight u effect bdr shd fitText vertAlign rtl cs em lang eastAsianLayout ",
            "specVanish oMath rPrChange",
        ),
    ),
    (
        "sectPr",
        concat!(
            "headerReference footerReference footnotePr endnotePr type pgSz pgMar paperSrc ",
            "pgBorders lnNumType pgNumType cols formProt vAlign noEndnote titlePg textDirection ",
            "bidi rtlGutter docGrid printerSettings sectPrChange",
        ),
    ),
    (
        "style",
        concat!(
            "name aliases basedOn next link autoRedefine hidden uiPriority semiHidden ",
            "unhideWhenUsed qFormat locked personal personalCompose personalReply rsid pPr rPr ",
            "tblPr trPr tcPr tblStylePr",
        ),
    ),
    ("tbl", "tblPr tblGrid tr"),
    ("tblBorders", "top start left bottom end right insideH insideV"),
    ("tblCellMar", "top start left bottom end right"),
    ("tblGrid", "gridCol tblGridChange"),
    (
        "tblPr",
        concat!(
            "tblStyle tblpPr tblOverlap bidiVisual tblStyleRowBandSize tblStyleColBandSize tblW ",
            "jc tblCellSpacing tblInd tblBorders shd tblLayout tblCellMar tblLook tblCaption ",
            "tblDescription tblPrChange",
        ),
    ),
    (
        "tcPr",
        concat!(
            "cnfStyle tcW gridSpan hMerge vMerge tcBorders shd noWrap tcMar textDirection ",
            "tcFitText vAlign hideMark headers cellIns cellDel cellMerge tcPrChange",
        ),
    ),
    (
        "trPr",
        concat!(
            "cnfStyle divId gridBefore gridAfter wBefore wAfter cantSplit trHeight tblHeader ",
            "tblCellSpacing jc hidden ins del trPrChange",
        ),
    ),
];

#[test]
fn docx_property_children_follow_schema_order() {
    let _serial = serial();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut output = OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut count = 0u32;

    assert!(root.is_dir());

    for (name, bytes) in inputs(root) {
        let read = if Path::new(&name).extension().is_some_and(|extension| extension == "docx") {
            guarded(|| docx::read(&bytes, &mut workspace, &mut document))
        } else {
            guarded(|| {
                markdown::read(&bytes, markdown::Options::GFM, &mut workspace, &mut document)
            })
        };

        if read.is_err() {
            continue;
        }

        let length =
            guarded(|| docx::write(&document, &mut workspace, &mut output[..])).unwrap() as usize;

        assert!(length <= output.len());

        package_check(&output[..length], &name);
        count += 1;
    }

    assert!(count > DOCUMENT_COUNT_MIN, "only {count} documents produced");
}

fn file_read(path: &Path) -> Vec<u8> {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));

    assert!(!bytes.is_empty());

    bytes
}

fn directory_entries(directory: &Path) -> Vec<fs::DirEntry> {
    assert!(directory.is_dir());

    let listing =
        fs::read_dir(directory).unwrap_or_else(|error| panic!("{}: {error}", directory.display()));

    listing
        .map(|entry| entry.unwrap_or_else(|error| panic!("{}: {error}", directory.display())))
        .collect()
}

fn file_stem_lossy(path: &Path) -> String {
    let Some(name) = path.file_name() else { panic!("{} has no file name", path.display()) };

    assert!(!name.is_empty());

    name.to_string_lossy().into_owned()
}

fn inputs(root: &Path) -> Vec<(String, Vec<u8>)> {
    assert!(root.is_dir());

    let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();

    for name in ["fixtures/sample.md", "corpus/commonmark-spec.txt", "corpus/gfm-spec.txt"] {
        inputs.push((name.into(), file_read(&root.join("tests").join(name))));
    }

    for directory in directory_entries(&root.join("tests/corpus/docx")) {
        let directory_path = directory.path();

        if !directory_path.is_dir() {
            continue;
        }

        for entry in directory_entries(&directory_path) {
            let path = entry.path();

            if path.extension().is_some_and(|extension| extension == "docx") {
                let name =
                    format!("{}-{}", file_stem_lossy(&directory_path), file_stem_lossy(&path));

                inputs.push((name, file_read(&path)));
            }
        }
    }

    assert!(inputs.len() > 3);

    inputs
}

fn package_check(output: &[u8], label: &str) {
    const DECLARATION: &[u8; 6] = b"<?xml ";

    assert!(!output.is_empty());
    assert!(!label.is_empty());

    let mut index = 0usize;
    let mut parts = 0u32;

    while index < output.len() {
        let Some(offset) = find(&output[index..], DECLARATION) else {
            break;
        };

        let start = index + offset;
        index = start + part_check(&output[start..], label);
        parts += 1;
    }

    assert!(index <= output.len());
    assert!(parts == PART_COUNT, "{label}: found {parts} parts, expected {PART_COUNT}");
}

struct Frame<'a> {
    children: Vec<&'a str>,
    name: &'a str,
}

fn part_check(xml: &[u8], label: &str) -> usize {
    assert!(xml.starts_with(b"<?xml "));

    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut index = 0usize;

    while index < xml.len() {
        let Some(offset) = xml[index..].iter().position(|&byte| byte == b'<') else {
            break;
        };

        let open = index + offset;

        let Some(length) = xml[open..].iter().position(|&byte| byte == b'>') else {
            panic!("{label}: unterminated tag");
        };

        assert!(length >= 1);

        let tag = &xml[open + 1..open + length];
        index = open + length + 1;

        if tag.starts_with(b"?") {
            continue;
        }

        if tag.starts_with(b"!") {
            continue;
        }

        if let Some(closing) = tag.strip_prefix(b"/") {
            let name = local_name(closing);

            let Some(frame) = stack.pop() else {
                panic!("{label}: </{name}> with nothing open");
            };

            assert!(frame.name == name, "{label}: </{name}> closes <{}>", frame.name);
            children_check(&frame, label);

            if stack.is_empty() {
                return index;
            }

            continue;
        }

        let empty = tag.ends_with(b"/");
        let name = local_name(if empty { &tag[..tag.len() - 1] } else { tag });

        if let Some(frame) = stack.last_mut() {
            frame.children.push(name);
        }

        if !empty {
            stack.push(Frame { children: Vec::new(), name });
        }
    }

    panic!("{label}: {} elements left open", stack.len());
}

fn children_check(frame: &Frame<'_>, label: &str) {
    assert!(!frame.name.is_empty());

    let Frame { children, name: container } = frame;

    let Some(sequence) = SEQUENCES.iter().find(|entry| entry.0 == *container).map(|entry| entry.1)
    else {
        return;
    };

    assert!(!sequence.is_empty());

    let mut previous = 0usize;

    for &child in children {
        let Some(position) = sequence.split(' ').position(|name| name == child) else {
            panic!("{label}: <{child}> is not a child of <{container}> in the schema");
        };

        assert!(
            position >= previous,
            "{label}: <{child}> out of sequence in <{container}>, children {children:?}",
        );

        previous = position;
    }
}

fn local_name(tag: &[u8]) -> &str {
    assert!(!tag.is_empty());

    let name = tag.split(|&byte| byte == b' ').next().unwrap_or(tag);
    let text = str::from_utf8(name).unwrap_or_else(|error| panic!("tag name: {error}"));
    let local = text.rsplit(':').next().unwrap_or(text);

    assert!(!local.is_empty());

    local
}

fn find<const N: usize>(haystack: &[u8], needle: &[u8; N]) -> Option<usize> {
    assert!(N >= 1);

    haystack.windows(N).position(|window| window == needle)
}
