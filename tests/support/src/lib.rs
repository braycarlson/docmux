use docmux::html::HTMLRaw;
use docmux::markdown::Options;
use docmux::{Document, Error, Workspace, adf, docx, html, markdown};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::sync::{Mutex, MutexGuard, PoisonError};

pub const OUTPUT_BYTES_MAX: usize = 32 << 20;

pub static DOCUMENT: Mutex<Document> = Mutex::new(Document::EMPTY);
pub static OUTPUT: Mutex<[u8; OUTPUT_BYTES_MAX]> = Mutex::new([0; OUTPUT_BYTES_MAX]);
pub static SCRATCH: Mutex<[u8; OUTPUT_BYTES_MAX]> = Mutex::new([0; OUTPUT_BYTES_MAX]);
pub static WORKSPACE: Mutex<Workspace> = Mutex::new(Workspace::EMPTY);
static SERIAL: Mutex<()> = Mutex::new(());

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static HEAP_CALL_COUNT: Cell<u64> = const { Cell::new(0) };
}

#[derive(Debug)]
pub struct CountingAllocator;

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        assert!(layout.size() != 0);

        heap_call_record();

        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        assert!(!pointer.is_null());
        assert!(layout.size() != 0);

        heap_call_record();

        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size_new: usize) -> *mut u8 {
        assert!(!pointer.is_null());
        assert!(layout.size() != 0);
        assert!(size_new != 0);

        heap_call_record();

        unsafe { System.realloc(pointer, layout, size_new) }
    }
}

fn heap_call_record() {
    let armed = ARMED.try_with(Cell::get).unwrap_or(false);

    if !armed {
        return;
    }

    HEAP_CALL_COUNT.try_with(|count| count.set(count.get() + 1)).unwrap_or_default();
}

pub fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn guarded<R, F: FnOnce() -> R>(body: F) -> R {
    HEAP_CALL_COUNT.with(|count| count.set(0));
    ARMED.with(|armed| armed.set(true));

    assert!(ARMED.with(Cell::get));

    let result = body();

    ARMED.with(|armed| armed.set(false));
    let heap_call_count = HEAP_CALL_COUNT.with(Cell::get);

    assert!(!ARMED.with(Cell::get));
    assert!(heap_call_count == 0, "conversion touched the heap {heap_call_count} times");

    result
}

#[must_use]
pub fn fixture(name: &str) -> Vec<u8> {
    assert!(!name.is_empty());

    let path = format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"));

    assert!(!bytes.is_empty());

    bytes
}

pub fn conversion_ok<T>(step: &str, result: Result<T, Error>) -> T {
    assert!(!step.is_empty());

    result.unwrap_or_else(|error| panic!("{step}: {error}"))
}

pub fn writers_all(document: &mut Document, workspace: &mut Workspace) {
    assert!(document.node_count() >= 1);

    let mut output = OUTPUT.lock().unwrap_or_else(PoisonError::into_inner);
    let mut scratch = SCRATCH.lock().unwrap_or_else(PoisonError::into_inner);

    let markdown_length =
        conversion_ok("markdown write", markdown::write(document, &mut output[..]));

    assert!(markdown_length <= output.len());

    let markdown_read =
        markdown::read(&output[..markdown_length], Options::GFM, workspace, document);

    conversion_ok("markdown read", markdown_read);
    conversion_ok("html write", html::write(document, HTMLRaw::Escape, &mut scratch[..]));

    let adf_length = conversion_ok("adf write", adf::write(document, &mut output[..]));

    assert!(adf_length <= output.len());

    conversion_ok("adf read", adf::read(&output[..adf_length], document));

    let docx_length =
        conversion_ok("docx write", docx::write(document, workspace, &mut scratch[..]));

    assert!(docx_length <= scratch.len());

    conversion_ok("docx read", docx::read(&scratch[..docx_length], workspace, document));

    assert!(document.node_count() >= 1);
}
