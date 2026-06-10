//! Deterministic CBOR (RFC 8949 §4.2 Core Deterministic Encoding).
//!
//! VSD admits exactly one byte representation for every logical value.
//! The encoder produces it; the decoder *rejects everything else*:
//!
//! - integers, lengths and float arguments use the shortest possible form
//! - indefinite-length items are forbidden
//! - map keys must be strictly increasing in bytewise encoded order
//!   (which also forbids duplicate keys)
//! - floats use the shortest of f16/f32/f64 that preserves the value;
//!   the only admissible NaN is the canonical quiet NaN `0xf9 0x7e 0x00`
//! - tags and "simple values" other than `false`/`true`/`null` are forbidden
//!
//! Strict decoding is a security property: a `.vsd` cannot be a polyglot,
//! and two conforming parsers cannot disagree about what a document says.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::error::{Error, Result};

/// Maximum nesting depth accepted by the decoder. Deep enough for any real
/// document tree, shallow enough to bound stack use on hostile input.
pub const MAX_DEPTH: usize = 256;

/// A CBOR data item restricted to the subset VSD uses.
#[derive(Clone, PartialEq)]
pub enum Value {
    /// Major type 0.
    Unsigned(u64),
    /// Major type 1; `Negative(n)` represents `-1 - n`.
    Negative(u64),
    /// Major type 2.
    Bytes(Vec<u8>),
    /// Major type 3.
    Text(String),
    /// Major type 4.
    Array(Vec<Value>),
    /// Major type 5. Entries are sorted by encoded key at encode time;
    /// the decoder enforces strictly increasing key order.
    Map(Vec<(Value, Value)>),
    /// Major type 7, simple values 20/21.
    Bool(bool),
    /// Major type 7, simple value 22.
    Null,
    /// Major type 7; encoded in the shortest exact form.
    Float(f64),
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Unsigned(n) => write!(f, "{n}"),
            Value::Negative(n) => write!(f, "-{}", (*n as u128) + 1),
            Value::Bytes(b) => write!(f, "h'{}'", hex::encode(b)),
            Value::Text(s) => write!(f, "{s:?}"),
            Value::Array(a) => f.debug_list().entries(a).finish(),
            Value::Map(m) => f
                .debug_map()
                .entries(m.iter().map(|(k, v)| (k, v)))
                .finish(),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Null => write!(f, "null"),
            Value::Float(x) => write!(f, "{x}"),
        }
    }
}

impl Value {
    pub fn text(s: impl Into<String>) -> Value {
        Value::Text(s.into())
    }

    pub fn int(n: i64) -> Value {
        if n >= 0 {
            Value::Unsigned(n as u64)
        } else {
            Value::Negative((-1i128 - n as i128) as u64)
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Unsigned(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(x) => Some(*x),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&[(Value, Value)]> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Look up a text key in a map value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_map()?
            .iter()
            .find(|(k, _)| k.as_text() == Some(key))
            .map(|(_, v)| v)
    }

    /// Encode to the unique deterministic byte representation.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(64);
        self.encode_into(&mut out, 0)?;
        Ok(out)
    }

    fn encode_into(&self, out: &mut Vec<u8>, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(Error::Cbor("nesting depth exceeds MAX_DEPTH".into()));
        }
        match self {
            Value::Unsigned(n) => write_head(out, 0, *n),
            Value::Negative(n) => write_head(out, 1, *n),
            Value::Bytes(b) => {
                write_head(out, 2, b.len() as u64);
                out.extend_from_slice(b);
            }
            Value::Text(s) => {
                write_head(out, 3, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
            }
            Value::Array(items) => {
                write_head(out, 4, items.len() as u64);
                for item in items {
                    item.encode_into(out, depth + 1)?;
                }
            }
            Value::Map(entries) => {
                // Encode each entry, then sort bytewise by encoded key.
                let mut encoded: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    let mut kb = Vec::new();
                    k.encode_into(&mut kb, depth + 1)?;
                    let mut vb = Vec::new();
                    v.encode_into(&mut vb, depth + 1)?;
                    encoded.push((kb, vb));
                }
                encoded.sort_by(|a, b| a.0.cmp(&b.0));
                for w in encoded.windows(2) {
                    if w[0].0 == w[1].0 {
                        return Err(Error::Cbor("duplicate map key".into()));
                    }
                }
                write_head(out, 5, encoded.len() as u64);
                for (kb, vb) in encoded {
                    out.extend_from_slice(&kb);
                    out.extend_from_slice(&vb);
                }
            }
            Value::Bool(false) => out.push(0xf4),
            Value::Bool(true) => out.push(0xf5),
            Value::Null => out.push(0xf6),
            Value::Float(x) => encode_float(out, *x),
        }
        Ok(())
    }

    /// Strictly decode a byte string that must contain exactly one
    /// deterministically-encoded data item (no trailing bytes).
    pub fn decode(bytes: &[u8]) -> Result<Value> {
        let mut d = Decoder { buf: bytes, pos: 0 };
        let v = d.item(0)?;
        if d.pos != bytes.len() {
            return Err(Error::Cbor(format!(
                "{} trailing byte(s) after data item",
                bytes.len() - d.pos
            )));
        }
        Ok(v)
    }
}

fn write_head(out: &mut Vec<u8>, major: u8, arg: u64) {
    let mt = major << 5;
    if arg < 24 {
        out.push(mt | arg as u8);
    } else if arg <= u8::MAX as u64 {
        out.push(mt | 24);
        out.push(arg as u8);
    } else if arg <= u16::MAX as u64 {
        out.push(mt | 25);
        out.extend_from_slice(&(arg as u16).to_be_bytes());
    } else if arg <= u32::MAX as u64 {
        out.push(mt | 26);
        out.extend_from_slice(&(arg as u32).to_be_bytes());
    } else {
        out.push(mt | 27);
        out.extend_from_slice(&arg.to_be_bytes());
    }
}

/// Canonical quiet NaN per RFC 8949 §4.2.2.
const CANONICAL_NAN_F16: u16 = 0x7e00;

fn encode_float(out: &mut Vec<u8>, x: f64) {
    if x.is_nan() {
        out.push(0xf9);
        out.extend_from_slice(&CANONICAL_NAN_F16.to_be_bytes());
        return;
    }
    let as_f32 = x as f32;
    if as_f32 as f64 == x {
        if let Some(h) = f32_to_f16_exact(as_f32) {
            out.push(0xf9);
            out.extend_from_slice(&h.to_be_bytes());
            return;
        }
        out.push(0xfa);
        out.extend_from_slice(&as_f32.to_bits().to_be_bytes());
        return;
    }
    out.push(0xfb);
    out.extend_from_slice(&x.to_bits().to_be_bytes());
}

/// Convert an f32 to IEEE 754 binary16 iff the conversion is exact.
fn f32_to_f16_exact(x: f32) -> Option<u16> {
    let bits = x.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mant = bits & 0x007f_ffff;

    if exp == 0xff {
        // Infinity (NaN handled by caller).
        return if mant == 0 { Some(sign | 0x7c00) } else { None };
    }
    if exp == 0 && mant == 0 {
        return Some(sign); // ±0
    }
    let unbiased = exp - 127;
    if (-14..=15).contains(&unbiased) {
        // Normal f16 range: need the low 13 mantissa bits clear.
        if mant & 0x1fff != 0 {
            return None;
        }
        let h_exp = ((unbiased + 15) as u16) << 10;
        return Some(sign | h_exp | (mant >> 13) as u16);
    }
    if (-24..-14).contains(&unbiased) {
        // Subnormal f16. Implicit leading 1 joins the mantissa.
        let full = 0x0080_0000u32 | mant; // 24-bit significand
        let shift = (-14 - unbiased) as u32 + 13;
        if shift >= 24 {
            return None;
        }
        if full & ((1u32 << shift) - 1) != 0 {
            return None;
        }
        return Some(sign | (full >> shift) as u16);
    }
    None
}

/// Exact binary16 → binary64 conversion via bit manipulation: every f16
/// value is exactly representable in f64, and using integer ops keeps the
/// conversion identical on every platform and free of `std` float math.
fn f16_to_f64(h: u16) -> f64 {
    let sign = ((h as u64) & 0x8000) << 48;
    let exp = ((h >> 10) & 0x1f) as u64;
    let mant = (h & 0x3ff) as u64;
    let bits = match (exp, mant) {
        (0, 0) => sign, // ±0
        (0, _) => {
            // Subnormal: value = mant × 2⁻²⁴ with mant in 1..=1023.
            // Normalize: leading 1 at bit p → value = 1.f × 2^(p−24).
            let p = 63 - mant.leading_zeros() as u64; // 0..=9
            let e = p + 999; // (p − 24) + 1023 bias
            let m = (mant ^ (1u64 << p)) << (52 - p); // clear leading 1, left-justify
            sign | (e << 52) | m
        }
        (0x1f, 0) => sign | 0x7ff0_0000_0000_0000, // ±∞
        (0x1f, _) => sign | 0x7ff8_0000_0000_0000 | (mant << 42), // NaN (payload preserved)
        _ => sign | ((exp + 1023 - 15) << 52) | (mant << 42),
    };
    f64::from_bits(bits)
}

struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| Error::Cbor("unexpected end of input".into()))?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or_else(|| Error::Cbor("unexpected end of input".into()))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// Read a head argument, enforcing minimal-length encoding.
    fn arg(&mut self, info: u8) -> Result<u64> {
        match info {
            0..=23 => Ok(info as u64),
            24 => {
                let v = self.byte()? as u64;
                if v < 24 {
                    return Err(Error::Cbor("non-minimal integer encoding (1-byte)".into()));
                }
                Ok(v)
            }
            25 => {
                let v = u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as u64;
                if v <= u8::MAX as u64 {
                    return Err(Error::Cbor("non-minimal integer encoding (2-byte)".into()));
                }
                Ok(v)
            }
            26 => {
                let v = u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as u64;
                if v <= u16::MAX as u64 {
                    return Err(Error::Cbor("non-minimal integer encoding (4-byte)".into()));
                }
                Ok(v)
            }
            27 => {
                let v = u64::from_be_bytes(self.take(8)?.try_into().unwrap());
                if v <= u32::MAX as u64 {
                    return Err(Error::Cbor("non-minimal integer encoding (8-byte)".into()));
                }
                Ok(v)
            }
            31 => Err(Error::Cbor("indefinite-length items are forbidden".into())),
            _ => Err(Error::Cbor(format!("reserved additional info {info}"))),
        }
    }

    fn len_arg(&mut self, info: u8) -> Result<usize> {
        let n = self.arg(info)?;
        usize::try_from(n).map_err(|_| Error::Cbor("length exceeds platform usize".into()))
    }

    fn item(&mut self, depth: usize) -> Result<Value> {
        if depth > MAX_DEPTH {
            return Err(Error::Cbor("nesting depth exceeds MAX_DEPTH".into()));
        }
        let head = self.byte()?;
        let major = head >> 5;
        let info = head & 0x1f;
        match major {
            0 => Ok(Value::Unsigned(self.arg(info)?)),
            1 => Ok(Value::Negative(self.arg(info)?)),
            2 => {
                let n = self.len_arg(info)?;
                Ok(Value::Bytes(self.take(n)?.to_vec()))
            }
            3 => {
                let n = self.len_arg(info)?;
                let s = core::str::from_utf8(self.take(n)?)
                    .map_err(|_| Error::Cbor("invalid UTF-8 in text string".into()))?;
                Ok(Value::Text(s.to_owned()))
            }
            4 => {
                let n = self.len_arg(info)?;
                let mut items = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    items.push(self.item(depth + 1)?);
                }
                Ok(Value::Array(items))
            }
            5 => {
                let n = self.len_arg(info)?;
                let mut entries: Vec<(Value, Value)> = Vec::with_capacity(n.min(4096));
                let mut prev_key: Option<&[u8]> = None;
                for _ in 0..n {
                    let key_start = self.pos;
                    let k = self.item(depth + 1)?;
                    let key_bytes = &self.buf[key_start..self.pos];
                    if let Some(prev) = prev_key {
                        if key_bytes <= prev {
                            return Err(Error::Cbor(
                                "map keys not in strictly increasing bytewise order".into(),
                            ));
                        }
                    }
                    prev_key = Some(key_bytes);
                    let v = self.item(depth + 1)?;
                    entries.push((k, v));
                }
                Ok(Value::Map(entries))
            }
            6 => Err(Error::Cbor("tags are forbidden in VSD".into())),
            7 => match info {
                20 => Ok(Value::Bool(false)),
                21 => Ok(Value::Bool(true)),
                22 => Ok(Value::Null),
                23 => Err(Error::Cbor("'undefined' is forbidden in VSD".into())),
                25 => {
                    let h = u16::from_be_bytes(self.take(2)?.try_into().unwrap());
                    let x = f16_to_f64(h);
                    if x.is_nan() && h != CANONICAL_NAN_F16 {
                        return Err(Error::Cbor("non-canonical NaN".into()));
                    }
                    Ok(Value::Float(x))
                }
                26 => {
                    let f = f32::from_bits(u32::from_be_bytes(self.take(4)?.try_into().unwrap()));
                    if f.is_nan() {
                        return Err(Error::Cbor("non-canonical NaN width".into()));
                    }
                    if f32_to_f16_exact(f).is_some() {
                        return Err(Error::Cbor("float not in shortest form (fits f16)".into()));
                    }
                    Ok(Value::Float(f as f64))
                }
                27 => {
                    let x = f64::from_bits(u64::from_be_bytes(self.take(8)?.try_into().unwrap()));
                    if x.is_nan() {
                        return Err(Error::Cbor("non-canonical NaN width".into()));
                    }
                    if x as f32 as f64 == x {
                        return Err(Error::Cbor("float not in shortest form (fits f32)".into()));
                    }
                    Ok(Value::Float(x))
                }
                _ => Err(Error::Cbor(format!("forbidden simple value {info}"))),
            },
            _ => unreachable!(),
        }
    }
}

/// Convenience builder for map values with text keys, omitting `None`s.
pub struct MapBuilder {
    entries: Vec<(Value, Value)>,
}

impl MapBuilder {
    pub fn new() -> Self {
        MapBuilder {
            entries: Vec::new(),
        }
    }

    pub fn put(mut self, key: &str, value: Value) -> Self {
        self.entries.push((Value::text(key), value));
        self
    }

    pub fn put_opt(self, key: &str, value: Option<Value>) -> Self {
        match value {
            Some(v) => self.put(key, v),
            None => self,
        }
    }

    pub fn build(self) -> Value {
        Value::Map(self.entries)
    }
}

impl Default for MapBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(v: &Value) -> Value {
        let bytes = v.encode().unwrap();
        Value::decode(&bytes).unwrap()
    }

    #[test]
    fn integer_minimality() {
        for (v, expect) in [
            (Value::Unsigned(0), vec![0x00]),
            (Value::Unsigned(23), vec![0x17]),
            (Value::Unsigned(24), vec![0x18, 24]),
            (Value::Unsigned(255), vec![0x18, 0xff]),
            (Value::Unsigned(256), vec![0x19, 0x01, 0x00]),
            (
                Value::Unsigned(u32::MAX as u64),
                vec![0x1a, 0xff, 0xff, 0xff, 0xff],
            ),
            (Value::Negative(0), vec![0x20]), // -1
        ] {
            assert_eq!(v.encode().unwrap(), expect);
            assert_eq!(roundtrip(&v), v);
        }
    }

    #[test]
    fn rejects_non_minimal_int() {
        // 24 encoded with a 2-byte argument
        assert!(Value::decode(&[0x19, 0x00, 0x18]).is_err());
        // 10 encoded with 1-byte argument
        assert!(Value::decode(&[0x18, 0x0a]).is_err());
    }

    #[test]
    fn rejects_indefinite_and_tags() {
        assert!(Value::decode(&[0x9f, 0xff]).is_err()); // indefinite array
        assert!(Value::decode(&[0x5f, 0xff]).is_err()); // indefinite bytes
        assert!(Value::decode(&[0xc0, 0x60]).is_err()); // tag 0
    }

    #[test]
    fn map_key_order_enforced_and_produced() {
        // Builder order is irrelevant; encoded order is canonical.
        let a = Value::Map(vec![
            (Value::text("zz"), Value::Unsigned(1)),
            (Value::text("a"), Value::Unsigned(2)),
        ]);
        let b = Value::Map(vec![
            (Value::text("a"), Value::Unsigned(2)),
            (Value::text("zz"), Value::Unsigned(1)),
        ]);
        assert_eq!(a.encode().unwrap(), b.encode().unwrap());

        // Decoder rejects wrong order: {"b":1,"a":2}
        let bad = [0xa2, 0x61, b'b', 0x01, 0x61, b'a', 0x02];
        assert!(Value::decode(&bad).is_err());

        // Decoder rejects duplicates: {"a":1,"a":2}
        let dup = [0xa2, 0x61, b'a', 0x01, 0x61, b'a', 0x02];
        assert!(Value::decode(&dup).is_err());
    }

    #[test]
    fn duplicate_keys_rejected_at_encode() {
        let v = Value::Map(vec![
            (Value::text("a"), Value::Unsigned(1)),
            (Value::text("a"), Value::Unsigned(2)),
        ]);
        assert!(v.encode().is_err());
    }

    #[test]
    fn float_shortest_form() {
        // 1.5 fits f16
        assert_eq!(Value::Float(1.5).encode().unwrap(), vec![0xf9, 0x3e, 0x00]);
        // 0.1 needs full f64
        assert_eq!(Value::Float(0.1).encode().unwrap()[0], 0xfb);
        // 1/3 in f32 precision needs f32
        let f = 1.0f32 / 3.0;
        assert_eq!(Value::Float(f as f64).encode().unwrap()[0], 0xfa);
        for x in [
            0.0,
            -0.0,
            1.5,
            0.1,
            65504.0,
            5.960464477539063e-8,
            f64::INFINITY,
        ] {
            assert_eq!(roundtrip(&Value::Float(x)), Value::Float(x));
        }
        // NaN canonical
        assert_eq!(
            Value::Float(f64::NAN).encode().unwrap(),
            vec![0xf9, 0x7e, 0x00]
        );
    }

    #[test]
    fn f16_conversion_exhaustive() {
        // Exhaustively check the bit-level f16→f64 conversion against the
        // arithmetic reference, and that every f16 round-trips through the
        // shortest-form encoder.
        for h in 0..=u16::MAX {
            let got = f16_to_f64(h);
            let sign = if h & 0x8000 != 0 { -1.0f64 } else { 1.0 };
            let exp = (h >> 10) & 0x1f;
            let mant = (h & 0x3ff) as f64;
            let want = match exp {
                0 => sign * mant * (2.0f64).powi(-24),
                0x1f => {
                    if mant == 0.0 {
                        sign * f64::INFINITY
                    } else {
                        f64::NAN
                    }
                }
                _ => sign * (1.0 + mant / 1024.0) * (2.0f64).powi(exp as i32 - 15),
            };
            if want.is_nan() {
                assert!(got.is_nan(), "h={h:04x}");
            } else {
                assert_eq!(got.to_bits(), want.to_bits(), "h={h:04x}");
                assert_eq!(f32_to_f16_exact(got as f32), Some(h), "h={h:04x}");
            }
        }
    }

    #[test]
    fn rejects_overlong_float() {
        // 1.5 as f32 (should be f16)
        let mut b = vec![0xfa];
        b.extend_from_slice(&1.5f32.to_bits().to_be_bytes());
        assert!(Value::decode(&b).is_err());
        // 1.5 as f64
        let mut b = vec![0xfb];
        b.extend_from_slice(&1.5f64.to_bits().to_be_bytes());
        assert!(Value::decode(&b).is_err());
    }

    #[test]
    fn rejects_trailing_bytes() {
        assert!(Value::decode(&[0x00, 0x00]).is_err());
    }

    #[test]
    fn nested_roundtrip() {
        let v = Value::Map(vec![
            (Value::text("t"), Value::text("doc")),
            (
                Value::text("children"),
                Value::Array(vec![
                    Value::Bytes(vec![1, 2, 3]),
                    Value::Bool(true),
                    Value::Null,
                    Value::int(-42),
                ]),
            ),
        ]);
        assert_eq!(roundtrip(&v), v);
    }
}
