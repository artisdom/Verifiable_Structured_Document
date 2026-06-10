//! Property-based tests for the deterministic CBOR layer.
//!
//! These pin the format's two load-bearing encoding properties:
//!
//! 1. **Roundtrip**: every encodable value decodes back to itself.
//! 2. **Canonical fixpoint**: any byte string the strict decoder accepts
//!    re-encodes to *exactly those bytes* — i.e. the decoder admits only
//!    the canonical form, so one logical value has one representation,
//!    so one document has one hash.

use proptest::collection::{btree_map, vec as pvec};
use proptest::prelude::*;

use vsd_core::cbor::Value;
use vsd_core::object::ObjectId;

/// Generate arbitrary values, bounded in depth and width. Map keys are
/// unique text strings (the encoder rejects duplicates by design).
fn arb_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        any::<u64>().prop_map(Value::Unsigned),
        any::<u64>().prop_map(Value::Negative),
        pvec(any::<u8>(), 0..64).prop_map(Value::Bytes),
        "[a-zA-Z0-9 ._-]{0,32}".prop_map(Value::Text),
        any::<bool>().prop_map(Value::Bool),
        Just(Value::Null),
        arb_float().prop_map(Value::Float),
    ];
    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            pvec(inner.clone(), 0..8).prop_map(Value::Array),
            btree_map("[a-z]{1,8}", inner, 0..8).prop_map(|m| {
                Value::Map(m.into_iter().map(|(k, v)| (Value::text(k), v)).collect())
            }),
        ]
    })
}

/// Finite or infinite floats; NaN is excluded because the format admits
/// only the canonical NaN, which has its own unit test.
fn arb_float() -> impl Strategy<Value = f64> {
    prop_oneof![
        any::<f64>().prop_filter("no NaN", |x| !x.is_nan()),
        any::<i32>().prop_map(|n| n as f64), // f16/f32-friendly values
        any::<f32>()
            .prop_filter("no NaN", |x| !x.is_nan())
            .prop_map(|x| x as f64),
        Just(0.0),
        Just(-0.0),
        Just(f64::INFINITY),
        Just(f64::NEG_INFINITY),
    ]
}

proptest! {
    #[test]
    fn roundtrip(v in arb_value()) {
        let bytes = v.encode().unwrap();
        let back = Value::decode(&bytes).unwrap();
        // Note: Float(-0.0) == Float(0.0) under f64 semantics, but their
        // encodings differ; compare encodings for the strong claim.
        prop_assert_eq!(back.encode().unwrap(), bytes);
    }

    #[test]
    fn encoding_is_order_independent(
        entries in btree_map("[a-z]{1,8}", any::<u64>(), 1..10)
    ) {
        let forward = Value::Map(
            entries.iter().map(|(k, v)| (Value::text(k.clone()), Value::Unsigned(*v))).collect(),
        );
        let reversed = Value::Map(
            entries.iter().rev().map(|(k, v)| (Value::text(k.clone()), Value::Unsigned(*v))).collect(),
        );
        prop_assert_eq!(forward.encode().unwrap(), reversed.encode().unwrap());
        prop_assert_eq!(
            ObjectId::of_value(&forward).unwrap(),
            ObjectId::of_value(&reversed).unwrap()
        );
    }

    #[test]
    fn decode_never_panics(bytes in pvec(any::<u8>(), 0..512)) {
        let _ = Value::decode(&bytes); // Ok or Err, never panic
    }

    #[test]
    fn accepted_bytes_are_canonical_fixpoint(bytes in pvec(any::<u8>(), 0..256)) {
        // THE security property: anything the decoder accepts re-encodes
        // to the identical bytes. No polyglots, no second representation.
        if let Ok(v) = Value::decode(&bytes) {
            prop_assert_eq!(v.encode().unwrap(), bytes);
        }
    }

    #[test]
    fn float_shortest_form_roundtrip(x in arb_float()) {
        let bytes = Value::Float(x).encode().unwrap();
        match Value::decode(&bytes).unwrap() {
            Value::Float(y) => prop_assert_eq!(x.to_bits(), y.to_bits()),
            other => prop_assert!(false, "decoded to {other:?}"),
        }
    }
}
