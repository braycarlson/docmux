<p align="center">
    <picture>
        <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/braycarlson/docmux/main/assets/docmux-wordmark-on-dark-outline.svg">
        <source media="(prefers-color-scheme: light)" srcset="https://raw.githubusercontent.com/braycarlson/docmux/main/assets/docmux-wordmark-on-light-outline.svg">
        <img alt="docmux" src="https://raw.githubusercontent.com/braycarlson/docmux/main/assets/docmux-wordmark-on-light-outline.svg" width="280">
    </picture>
</p>

&nbsp;

<p align="center">
    A statically allocated converter between Markdown, DOCX, and Atlassian Document Format.
</p>

<p align="center">
    <a href="https://github.com/braycarlson/docmux/actions/workflows/ci.yml"><img alt="ci" src="https://img.shields.io/github/actions/workflow/status/braycarlson/docmux/ci.yml?branch=main&amp;style=flat-square&amp;label=ci"></a>
    <a href="https://www.rust-lang.org"><img alt="rust" src="https://img.shields.io/badge/rust-2024-orange.svg?style=flat-square"></a>
    <a href="LICENSE"><img alt="license" src="https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square"></a>
</p>

## Overview

docmux reads each format into one `Document`, a fixed arena of nodes and text, and writes any format back out of it. The crate is `no_std` and never links `alloc`, so an allocation is a compile error, and every capacity is a named constant that returns an `Error` when exceeded rather than growing.

## Features

- **Three formats**: Markdown, DOCX, and the Atlassian Document Format that Confluence and Jira store, each with a reader and a writer over the same tree.
- **CommonMark and GFM**: The Markdown reader passes every example of both specs, with tables, strikethrough, task items, and raw autolinks behind an `Options` bit set.
- **HTML preview**: An HTML writer renders the tree in the shape cmark emits, with raw HTML escaped, allowed, or filtered as GitHub does.
- **Word-compatible DOCX**: The writer emits its own styles and numbering, and the reader resolves style chains so localised and LibreOffice documents map correctly.
- **Confluence tolerance**: Panels, expands, mentions, emoji, status, dates, inline cards, and media each degrade to the nearest thing the tree can hold.
- **Guarded tests**: A counting allocator fails any test on a single heap call, and every conversion in the suite runs under it.
- **Dependencies**: There are none.

## Install

```toml
[dependencies]
docmux = { git = "https://github.com/braycarlson/docmux" }
```

docmux requires Rust 1.97.1, which `rust-toolchain.toml` pins.

## Usage

`Document` and `Workspace` are several megabytes, so they live in statics rather than on the stack. A reader fills the document and a writer returns the number of bytes it put in the caller's buffer.

```rust
use docmux::{Document, Workspace, docx, markdown};
use std::sync::Mutex;

static DOCUMENT: Mutex<Document> = Mutex::new(Document::EMPTY);
static WORKSPACE: Mutex<Workspace> = Mutex::new(Workspace::EMPTY);
static OUTPUT: Mutex<[u8; 4 << 20]> = Mutex::new([0; 4 << 20]);

fn markdown_to_docx(source: &[u8]) -> Result<usize, docmux::Error> {
    let mut document = DOCUMENT.lock().unwrap();
    let mut workspace = WORKSPACE.lock().unwrap();
    let mut output = OUTPUT.lock().unwrap();

    markdown::read(source, markdown::Options::GFM, &mut workspace, &mut document)?;

    docx::write(&document, &mut workspace, &mut output[..])
}
```

## Development

| Command | What it runs |
|---|---|
| `cargo test` | Each suite: the spec corpora, the DOCX corpora against their snapshots, the ADF schema, and the DOCX element sequences. |
| `cargo clippy --workspace --all-targets -- -D warnings` | Clippy, which the manifest sets to warn on `restriction`. |
| `cargo +nightly fmt --check` | The format gate, which is nightly because stable rustfmt ignores `imports_layout`. |
| `cargo +nightly fuzz run <target>` | The named fuzzer: `markdown`, `docx`, or `adf`. |
| `tigerstyle check .` | The sibling linter, `tigerstyle-lsp`, over this tree. |

The ADF schema suite needs `python3` with the `jsonschema` package.

## Licence

MIT. See [LICENSE](LICENSE).
