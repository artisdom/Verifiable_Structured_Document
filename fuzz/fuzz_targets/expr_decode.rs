//! Fuzz the forms expression language: parse arbitrary CBOR into an
//! expression and evaluate it. The language is total — evaluation must
//! terminate quickly and never panic, whatever the input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;
use vsd_core::cbor::Value;
use vsd_core::forms::{Expr, FieldValue};

fuzz_target!(|data: &[u8]| {
    let Ok(v) = Value::decode(data) else { return };
    let Ok(expr) = Expr::from_value(&v) else { return };

    // Evaluate against empty and small environments.
    let empty = BTreeMap::new();
    let _ = expr.eval(&empty);

    let mut env = BTreeMap::new();
    env.insert("a".to_string(), FieldValue::Num(1.5));
    env.insert("b".to_string(), FieldValue::Str("x".to_string()));
    env.insert("c".to_string(), FieldValue::Bool(true));
    let _ = expr.eval(&env);

    // Round-trip property.
    let v2 = expr.to_value();
    let expr2 = Expr::from_value(&v2).expect("canonical expr must decode");
    assert_eq!(expr, expr2);
});
