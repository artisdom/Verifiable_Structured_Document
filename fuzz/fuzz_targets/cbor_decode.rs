//! Fuzz the strict CBOR decoder: must never panic, and anything it
//! accepts must re-encode to the identical bytes (canonical fixpoint —
//! the property that makes one document have one hash).

#![no_main]

use libfuzzer_sys::fuzz_target;
use vsd_core::cbor::Value;

fuzz_target!(|data: &[u8]| {
    if let Ok(v) = Value::decode(data) {
        let reencoded = v.encode().expect("accepted value must re-encode");
        assert_eq!(
            reencoded, data,
            "decoder accepted non-canonical bytes — polyglot hazard"
        );
    }
});
