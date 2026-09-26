use docmux::html::{self, HTMLRaw};
use docmux::markdown::{self, Options};
use docmux_support::{DOCUMENT, OUTPUT, SCRATCH, WORKSPACE, guarded, serial};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::str;
use std::sync::PoisonError;

const EXAMPLE_COUNT_MIN: usize = 600;
const FENCE: &str = "````````````````````````````````";

#[derive(Debug)]
struct Example {
    html: String,
    markdown: String,
    number: u32,
    section: String,
}

#[derive(Clone, Copy, Debug)]
struct Suite {
    failures_name: &'static str,
    options: Options,
    raw: HTMLRaw,
    specification_name: &'static str,
}

fn examples_parse(specification: &str) -> Vec<Example> {
    assert!(!specification.is_empty());

    let mut examples = Vec::new();
    let mut section = String::new();
    let mut lines = specification.lines();
    let mut number = 0u32;

    while let Some(line) = lines.next() {
        let heading = line.starts_with("## ") || line.starts_with("# ");

        if heading {
            line.trim_start_matches('#').trim().clone_into(&mut section);

            continue;
        }

        if !line.starts_with(FENCE) {
            continue;
        }

        if !line.contains("example") {
            continue;
        }

        let disabled = line.contains("disabled");
        let mut example_markdown = String::new();
        let mut example_html = String::new();
        let mut in_html = false;

        for body in lines.by_ref() {
            if body == FENCE {
                break;
            }

            let separator = body == "." && !in_html;

            if separator {
                in_html = true;

                continue;
            }

            let target = if in_html { &mut example_html } else { &mut example_markdown };

            target.push_str(&body.replace('\u{2192}', "\t"));
            target.push('\n');
        }

        if disabled {
            continue;
        }

        assert!(!section.is_empty());

        number += 1;

        examples.push(Example {
            html: example_html,
            markdown: example_markdown,
            number,
            section: section.clone(),
        });
    }

    assert!(!examples.is_empty());
    assert!(number as usize == examples.len());

    examples
}

#[derive(Debug, PartialEq, Eq)]
enum Item {
    Block(String),
    Text { formats: BTreeSet<String>, text: String },
}

fn entity_decode(text: &str) -> String {
    let decoded = text
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#39;", "'")
        .replace("&amp;", "&");

    assert!(decoded.len() <= text.len());

    decoded
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;

    while index < bytes.len() {
        let hex = bytes.get(index + 1..index + 3).and_then(|pair| str::from_utf8(pair).ok());

        match (bytes[index], hex.and_then(|pair| u8::from_str_radix(pair, 16).ok())) {
            (b'%', Some(value)) => {
                out.push(value);
                index += 3;
            }
            (byte, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }

    assert!(index == bytes.len());
    assert!(out.len() <= bytes.len());

    String::from_utf8_lossy(&out).into_owned()
}

fn attribute(tag: &str, name: &'static str) -> Option<String> {
    assert!(tag.starts_with('<'));
    assert!(!name.is_empty());

    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')? + start;

    Some(entity_decode(&tag[start..end]))
}

fn tag_name(tag: &str) -> (bool, &str) {
    assert!(tag.starts_with('<'));

    let closing = tag.starts_with("</");
    let body = tag.trim_start_matches('<').trim_start_matches('/');
    let name_length = body.find([' ', '>', '/']).unwrap_or(body.len());

    assert!(name_length <= body.len());

    (closing, &body[..name_length])
}

fn tag_normalize(tag: &str) -> String {
    assert!(tag.starts_with('<'));

    let (closing, name) = tag_name(tag);
    let mut normalized = String::from(if closing { "/" } else { "" });

    normalized.push_str(name);

    for key in ["start", "align", "class", "src", "alt", "checked", "colspan"] {
        if let Some(value) = attribute(tag, key) {
            let decoded = if key == "src" { percent_decode(&value) } else { value };

            normalized.push(' ');
            normalized.push_str(key);
            normalized.push('=');
            normalized.push_str(&decoded);
        }
    }

    assert!(!normalized.is_empty());
    assert!(normalized.len() >= name.len());

    normalized
}

fn whitespace_collapse(text: &str) -> String {
    let core = text.split_whitespace().collect::<Vec<_>>().join(" ");

    assert!(core.len() <= text.len());

    if core.is_empty() {
        return String::new();
    }

    let leading = text.starts_with(char::is_whitespace);
    let trailing = text.ends_with(char::is_whitespace);

    let collapsed =
        format!("{}{core}{}", if leading { " " } else { "" }, if trailing { " " } else { "" });

    assert!(collapsed.len() <= text.len());

    collapsed
}

fn html_items(html: &str) -> Vec<Item> {
    let mut items = Vec::new();
    let mut formats: Vec<String> = Vec::new();
    let mut hrefs: Vec<String> = Vec::new();
    let mut in_pre = false;
    let mut rest = html;

    while !rest.is_empty() {
        let (text, after) = rest.find('<').map_or((rest, ""), |index| rest.split_at(index));

        if !text.is_empty() {
            let decoded = entity_decode(text);
            let cleaned = if in_pre { decoded } else { whitespace_collapse(&decoded) };

            if !cleaned.is_empty() {
                let mut set: BTreeSet<String> = formats.iter().cloned().collect();

                if let Some(href) = hrefs.last() {
                    let _inserted = set.insert(format!("a={}", percent_decode(href)));
                }

                items.push(Item::Text { formats: set, text: cleaned });
            }
        }

        if after.is_empty() {
            break;
        }

        let end = after.find('>').map_or(after.len(), |index| index + 1);

        assert!(end >= 1);

        let tag = &after[..end];
        rest = &after[end..];

        let (closing, name) = tag_name(tag);

        match name {
            "em" | "strong" | "del" | "code" if !in_pre => {
                if closing {
                    if let Some(position) = formats.iter().rposition(|format| *format == name) {
                        let _removed = formats.remove(position);
                    }
                } else {
                    formats.push(name.to_owned());
                }
            }
            "a" => {
                if closing {
                    let _popped = hrefs.pop();
                } else {
                    hrefs.push(attribute(tag, "href").unwrap_or_default());
                }
            }
            _ => {
                if name == "pre" {
                    in_pre = !closing;
                }

                items.push(Item::Block(tag_normalize(tag)));
            }
        }
    }

    items_merge(items)
}

fn items_merge(items: Vec<Item>) -> Vec<Item> {
    let item_count = items.len();
    let mut merged: Vec<Item> = Vec::new();

    for item in items {
        match (merged.last_mut(), item) {
            (
                Some(Item::Text { formats, text }),
                Item::Text { formats: formats_next, text: text_next },
            ) if *formats == formats_next => {
                text.push_str(&text_next);
            }
            (_, other) => merged.push(other),
        }
    }

    for item in &mut merged {
        if let Item::Text { text, .. } = item {
            *text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }

    merged.retain(|item| !matches!(item, Item::Text { text, .. } if text.is_empty()));

    assert!(merged.len() <= item_count);

    merged
}

fn utf8_string(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap_or_else(|error| panic!("output is not UTF-8: {error}"))
}

fn convert(source: &str, options: Options, raw: HTMLRaw) -> Option<(String, String, String)> {
    let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut output = OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut scratch = SCRATCH.lock().unwrap_or_else(PoisonError::into_inner);

    guarded(|| markdown::read(source.as_bytes(), options, &mut workspace, &mut document)).ok()?;
    let html_length = guarded(|| html::write(&document, raw, &mut output[..])).ok()?;

    assert!(html_length <= output.len());

    let html_text = utf8_string(&output[..html_length]);
    let markdown_length = guarded(|| markdown::write(&document, &mut scratch[..])).ok()?;

    assert!(markdown_length <= scratch.len());

    let written = utf8_string(&scratch[..markdown_length]);

    guarded(|| markdown::read(&scratch[..markdown_length], options, &mut workspace, &mut document))
        .ok()?;

    let again_length = guarded(|| markdown::write(&document, &mut output[..])).ok()?;

    assert!(again_length <= output.len());

    let again = utf8_string(&output[..again_length]);

    Some((html_text, written, again))
}

fn known_failures_read(path: &str) -> BTreeMap<u32, String> {
    assert!(!path.is_empty());

    let known: BTreeMap<u32, String> = fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (number, reason) = line.split_once(' ')?;

            Some((number.parse().ok()?, reason.to_owned()))
        })
        .collect();

    assert!(known.keys().all(|number| *number >= 1));

    known
}

fn examples_run(
    examples: &[Example],
    suite: Suite,
    details: &mut BTreeMap<u32, String>,
) -> BTreeMap<u32, String> {
    assert!(!examples.is_empty());

    let suite_gfm = suite.options == Options::GFM;
    let mut failing: BTreeMap<u32, String> = BTreeMap::new();

    assert!(suite_gfm || suite.options == Options::COMMONMARK);

    for example in examples {
        assert!(example.number >= 1);

        let extension = example.section.contains("(extension)");
        let plain_section = suite_gfm && !extension;

        let (options, raw) = if plain_section {
            (Options::COMMONMARK, HTMLRaw::Allow)
        } else {
            (suite.options, suite.raw)
        };

        let Some((html_text, written, again)) = convert(&example.markdown, options, raw) else {
            let _previous = failing
                .insert(example.number, format!("{}: read or write failed", example.section));

            continue;
        };

        if html_items(&html_text) != html_items(&example.html) {
            let _reason =
                failing.insert(example.number, format!("{}: html differs", example.section));

            let _detail = details.insert(example.number, format!("--- ours\n{html_text}"));
        } else if written != again {
            let _reason =
                failing.insert(example.number, format!("{}: round trip unstable", example.section));

            let _detail =
                details.insert(example.number, format!("--- first\n{written}--- second\n{again}"));
        }
    }

    assert!(failing.len() <= examples.len());

    failing
}

fn suite_run(suite: Suite) {
    assert!(!suite.failures_name.is_empty());
    assert!(!suite.specification_name.is_empty());

    let _serial = serial();
    let root = env!("CARGO_MANIFEST_DIR");
    let specification_path = format!("{root}/tests/corpus/{}", suite.specification_name);

    let specification = fs::read_to_string(&specification_path)
        .unwrap_or_else(|error| panic!("{specification_path}: {error}"));

    let failures_path = format!("{root}/tests/corpus/{}", suite.failures_name);
    let known = known_failures_read(&failures_path);
    let examples = examples_parse(&specification);

    assert!(examples.len() > EXAMPLE_COUNT_MIN, "specification parsed {} examples", examples.len());

    let mut details: BTreeMap<u32, String> = BTreeMap::new();
    let failing = examples_run(&examples, suite, &mut details);

    if env::var("DOCMUX_UPDATE_KNOWN_FAILURES").is_ok() {
        let mut text = String::new();

        for (number, reason) in &failing {
            text.push_str(&number.to_string());
            text.push(' ');
            text.push_str(reason);
            text.push('\n');
        }

        fs::write(&failures_path, text).unwrap_or_else(|error| panic!("{failures_path}: {error}"));
    }

    let unexpected: Vec<_> =
        failing.iter().filter(|(number, _)| !known.contains_key(number)).collect();

    let fixed: Vec<_> = known.keys().filter(|number| !failing.contains_key(number)).collect();
    let passing = examples.len() - failing.len();

    println!("{}: {passing} of {} examples pass", suite.specification_name, examples.len());

    let verbose = env::var("DOCMUX_SPEC_VERBOSE").is_ok();

    for (number, reason) in
        failing.iter().filter(|(number, _)| verbose || !known.contains_key(number))
    {
        let example = &examples[usize::try_from(*number - 1).unwrap_or(0)];

        println!(
            "\nfailure {number} ({reason})\n--- markdown\n{}--- expected\n{}{}",
            example.markdown,
            example.html,
            details.get(number).map_or("", String::as_str),
        );
    }

    assert!(unexpected.is_empty(), "{} unexpected failures", unexpected.len());
    assert!(fixed.is_empty(), "examples now pass and must leave the known-failure list: {fixed:?}");
}

#[test]
fn commonmark_specification() {
    suite_run(Suite {
        failures_name: "commonmark-known-failures.txt",
        options: Options::COMMONMARK,
        raw: HTMLRaw::Allow,
        specification_name: "commonmark-spec.txt",
    });
}

#[test]
fn gfm_specification() {
    suite_run(Suite {
        failures_name: "gfm-known-failures.txt",
        options: Options::GFM,
        raw: HTMLRaw::AllowFiltered,
        specification_name: "gfm-spec.txt",
    });
}
