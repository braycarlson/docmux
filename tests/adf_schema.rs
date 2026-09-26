use docmux::{adf, docx, markdown};
use docmux_support::{DOCUMENT, OUTPUT, WORKSPACE, guarded, serial};
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::PoisonError;

const DOCUMENT_COUNT_MIN: u32 = 200;
#[cfg(windows)]
const PYTHON: &str = "python";
#[cfg(not(windows))]
const PYTHON: &str = "python3";

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

    inputs.push(("sample.md".into(), file_read(&root.join("tests/fixtures/sample.md"))));

    inputs.push((
        "libreoffice.docx".into(),
        file_read(&root.join("tests/fixtures/libreoffice.docx")),
    ));

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

                assert!(!name.is_empty());

                inputs.push((name, file_read(&path)));
            }
        }
    }

    let specification = file_read(&root.join("tests/corpus/commonmark-spec.txt"));

    inputs.push(("commonmark-spec.md".into(), specification));

    assert!(inputs.len() > 3);

    inputs
}

#[test]
fn adf_output_matches_atlassian_schema() {
    let _serial = serial();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output_directory = root.join("target/adf-schema-check");

    if output_directory.exists() {
        fs::remove_dir_all(&output_directory).unwrap();
    }

    fs::create_dir_all(&output_directory).unwrap();

    assert!(output_directory.is_dir());

    let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut output = OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut count = 0u32;

    for (name, bytes) in &inputs(root) {
        let read = if Path::new(name).extension().is_some_and(|extension| extension == "docx") {
            guarded(|| docx::read(bytes, &mut workspace, &mut document))
        } else {
            guarded(|| markdown::read(bytes, markdown::Options::GFM, &mut workspace, &mut document))
        };

        if read.is_err() {
            continue;
        }

        let length = guarded(|| adf::write(&document, &mut output[..])).unwrap();

        assert!(length <= output.len());

        fs::write(output_directory.join(format!("{name}.json")), &output[..length]).unwrap();
        count += 1;
    }

    assert!(count > DOCUMENT_COUNT_MIN, "only {count} documents produced");

    let result = Command::new(PYTHON)
        .arg(root.join("tests/adf_schema_check.py"))
        .arg(root.join("tests/corpus/adf-schema.json"))
        .arg(&output_directory)
        .output()
        .expect("python with the jsonschema package is required for this test");

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);

    println!("{stdout}");

    assert!(result.status.success(), "ADF schema validation failed:\n{stdout}{stderr}");
}
