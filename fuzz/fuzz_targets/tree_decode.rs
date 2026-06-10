//! Fuzz the typed content-tree decoder: CBOR bytes → Node, then the
//! node must round-trip through its canonical encoding.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vsd_core::cbor::Value;
use vsd_core::tree::Node;

fuzz_target!(|data: &[u8]| {
    let Ok(v) = Value::decode(data) else { return };
    let Ok(node) = Node::from_value(&v) else { return };
    // Accepted nodes must round-trip exactly.
    let v2 = node.to_value().expect("accepted node must encode");
    let node2 = Node::from_value(&v2).expect("canonical node must decode");
    assert_eq!(node, node2, "tree decode/encode not a bijection");
});
