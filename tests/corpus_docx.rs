use docmux::{adf, docx, markdown};
use docmux_support::{DOCUMENT, OUTPUT, SCRATCH, WORKSPACE, guarded, serial};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::PoisonError;

const CORPUS_FILE_COUNT_MIN: usize = 250;

fn directory_entries(directory: &Path) -> Vec<fs::DirEntry> {
    assert!(directory.is_dir());

    let listing =
        fs::read_dir(directory).unwrap_or_else(|error| panic!("{}: {error}", directory.display()));

    listing
        .map(|entry| entry.unwrap_or_else(|error| panic!("{}: {error}", directory.display())))
        .collect()
}

fn corpus_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/docx");
    let mut files = Vec::new();

    assert!(root.is_dir());

    for directory in directory_entries(&root) {
        let directory_path = directory.path();

        if !directory_path.is_dir() {
            continue;
        }

        for entry in directory_entries(&directory_path) {
            let path = entry.path();

            if path.extension().is_some_and(|extension| extension == "docx") {
                files.push(path);
            }
        }
    }

    files.sort();

    assert!(!files.is_empty());

    files
}

#[test]
fn corpus_documents_convert_or_error_cleanly() {
    let _serial = serial();
    let update = env::var("DOCMUX_UPDATE_SNAPSHOTS").is_ok();
    let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut output = OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut scratch = SCRATCH.lock().unwrap_or_else(PoisonError::into_inner);
    let files = corpus_files();

    assert!(files.len() > CORPUS_FILE_COUNT_MIN, "corpus holds {} files", files.len());

    let mut mismatches = Vec::new();
    let mut errors = 0usize;

    for path in &files {
        let archive = fs::read(path).unwrap();
        let snapshot_path = path.with_extension("md");

        let actual = match guarded(|| docx::read(&archive, &mut workspace, &mut document)) {
            Ok(()) => {
                let length = guarded(|| markdown::write(&document, &mut output[..])).unwrap();
                let text = String::from_utf8(output[..length].to_vec()).unwrap();
                let adf_length = guarded(|| adf::write(&document, &mut scratch[..])).unwrap();

                guarded(|| adf::read(&scratch[..adf_length], &mut document)).unwrap();

                let docx_length =
                    guarded(|| docx::write(&document, &mut workspace, &mut scratch[..])).unwrap();

                guarded(|| docx::read(&scratch[..docx_length], &mut workspace, &mut document))
                    .unwrap();

                guarded(|| markdown::write(&document, &mut output[..])).unwrap();

                text
            }
            Err(error) => {
                errors += 1;

                format!("ERROR {error}\n")
            }
        };

        if update {
            fs::write(&snapshot_path, &actual).unwrap();

            continue;
        }

        let expected = fs::read_to_string(&snapshot_path).unwrap_or_default();

        if expected != actual {
            mismatches.push(path.display().to_string());
        }
    }

    println!("{} documents, {errors} errors", files.len());

    assert!(errors <= files.len());
    assert!(mismatches.is_empty(), "snapshots differ for {mismatches:?}");
}
