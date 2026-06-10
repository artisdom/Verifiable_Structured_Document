//! Fuzz the full container parser: arbitrary bytes through the eager
//! reader and the streaming reader. Must never panic, OOM, or loop.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;
use vsd_container::{read_document, ReadOptions, StreamReader};

fuzz_target!(|data: &[u8]| {
    let _ = read_document(data, &ReadOptions::default());
    let _ = read_document(data, &ReadOptions { allow_unknown_chunks: true });

    if let Ok(mut reader) = StreamReader::open(Cursor::new(data.to_vec())) {
        // Walk a few objects through lazy verification too.
        let root = reader.manifest().root;
        let _ = reader.object(&root);
        let _ = reader.page_closure(0);
    }
});
