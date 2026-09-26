#![no_main]

use docmux::adf;
use docmux_support::{DOCUMENT, WORKSPACE, writers_all};
use libfuzzer_sys::fuzz_target;
use std::sync::PoisonError;

fuzz_target!(|data: &[u8]| {
    let mut document = DOCUMENT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut workspace = WORKSPACE.lock().unwrap_or_else(PoisonError::into_inner);

    if adf::read(data, &mut document).is_ok() {
        writers_all(&mut document, &mut workspace);
    }
});
