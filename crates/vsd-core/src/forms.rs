//! Declarative form layer (spec §6).
//!
//! A total, terminating expression language. No loops, no I/O, no
//! string-eval, no network, no clock. Every expression provably
//! terminates: evaluation is a single bottom-up pass over a finite tree,
//! and regular expressions are RE2-class (Rust's `regex` crate — linear
//! time, no catastrophic backtracking, by construction).
//!
//! Wire form (CBOR): literals are themselves; field references are
//! `{"$": field-id}`; operations are arrays `[op, args...]`.

use std::collections::BTreeMap;

use crate::cbor::{MapBuilder, Value};
use crate::error::{Error, Result};

/// Hard cap on expression tree depth — bounds both decode and eval.
pub const MAX_EXPR_DEPTH: usize = 64;
/// Hard cap on total expression node count.
pub const MAX_EXPR_NODES: usize = 10_000;
/// Hard cap on regex pattern size (compiled size is bounded by `regex`).
pub const MAX_REGEX_LEN: usize = 1_000;

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// Literal number, string, or boolean.
    Num(f64),
    Str(String),
    Bool(bool),
    /// Reference to another field's value.
    FieldRef(String),
    /// ["+"/"-"/"*"/"/"/"min"/"max", expr...]
    Arith(ArithOp, Vec<Expr>),
    /// ["round", expr] — round half to even (banker's), matching the
    /// deterministic-arithmetic posture of the rest of the format.
    Round(Box<Expr>),
    /// Comparisons: ["=", a, b], ["<", a, b], ...
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    /// Boolean connectives.
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    /// ["if", cond, then, else]
    If(Box<Expr>, Box<Expr>, Box<Expr>),
    /// ["match", field-ref, [pattern, expr]..., default-expr]
    Match(String, Vec<(String, Expr)>, Box<Expr>),
    /// ["regex-valid", field-ref, anchored-pattern]
    RegexValid(String, String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A runtime field value during form evaluation.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Num(f64),
    Str(String),
    Bool(bool),
    /// Unfilled.
    Empty,
}

impl ArithOp {
    fn as_str(self) -> &'static str {
        match self {
            ArithOp::Add => "+",
            ArithOp::Sub => "-",
            ArithOp::Mul => "*",
            ArithOp::Div => "/",
            ArithOp::Min => "min",
            ArithOp::Max => "max",
        }
    }
}

impl CmpOp {
    fn as_str(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }
}

impl Expr {
    pub fn to_value(&self) -> Value {
        match self {
            Expr::Num(x) => Value::Float(*x),
            Expr::Str(s) => Value::text(s),
            Expr::Bool(b) => Value::Bool(*b),
            Expr::FieldRef(id) => MapBuilder::new().put("$", Value::text(id)).build(),
            Expr::Arith(op, args) => {
                let mut items = vec![Value::text(op.as_str())];
                items.extend(args.iter().map(Expr::to_value));
                Value::Array(items)
            }
            Expr::Round(e) => Value::Array(vec![Value::text("round"), e.to_value()]),
            Expr::Cmp(op, a, b) => {
                Value::Array(vec![Value::text(op.as_str()), a.to_value(), b.to_value()])
            }
            Expr::And(args) => {
                let mut items = vec![Value::text("and")];
                items.extend(args.iter().map(Expr::to_value));
                Value::Array(items)
            }
            Expr::Or(args) => {
                let mut items = vec![Value::text("or")];
                items.extend(args.iter().map(Expr::to_value));
                Value::Array(items)
            }
            Expr::Not(e) => Value::Array(vec![Value::text("not"), e.to_value()]),
            Expr::If(c, t, e) => Value::Array(vec![
                Value::text("if"),
                c.to_value(),
                t.to_value(),
                e.to_value(),
            ]),
            Expr::Match(field, arms, default) => {
                let mut items = vec![
                    Value::text("match"),
                    MapBuilder::new().put("$", Value::text(field)).build(),
                ];
                for (pat, e) in arms {
                    items.push(Value::Array(vec![Value::text(pat), e.to_value()]));
                }
                items.push(default.to_value());
                Value::Array(items)
            }
            Expr::RegexValid(field, pattern) => Value::Array(vec![
                Value::text("regex-valid"),
                MapBuilder::new().put("$", Value::text(field)).build(),
                Value::text(pattern),
            ]),
        }
    }

    pub fn from_value(v: &Value) -> Result<Expr> {
        let mut nodes = 0usize;
        Self::from_value_inner(v, 0, &mut nodes)
    }

    fn from_value_inner(v: &Value, depth: usize, nodes: &mut usize) -> Result<Expr> {
        if depth > MAX_EXPR_DEPTH {
            return Err(Error::Expr("expression too deep".into()));
        }
        *nodes += 1;
        if *nodes > MAX_EXPR_NODES {
            return Err(Error::Expr("expression too large".into()));
        }
        match v {
            Value::Float(x) => return Ok(Expr::Num(*x)),
            Value::Unsigned(n) => return Ok(Expr::Num(*n as f64)),
            Value::Negative(n) => return Ok(Expr::Num(-1.0 - *n as f64)),
            Value::Text(s) => return Ok(Expr::Str(s.clone())),
            Value::Bool(b) => return Ok(Expr::Bool(*b)),
            Value::Map(_) => return Ok(Expr::FieldRef(field_ref(v)?)),
            _ => {}
        }
        let arr = v
            .as_array()
            .ok_or_else(|| Error::Expr("expression must be literal, field-ref, or array".into()))?;
        let op = arr
            .first()
            .and_then(Value::as_text)
            .ok_or_else(|| Error::Expr("operation array must start with an op name".into()))?;
        let args = &arr[1..];
        let sub = |v: &Value, nodes: &mut usize| Self::from_value_inner(v, depth + 1, nodes);

        let arith = |op: ArithOp, args: &[Value], nodes: &mut usize| -> Result<Expr> {
            if args.len() < 2 {
                return Err(Error::Expr(format!(
                    "{:?} needs at least 2 arguments",
                    op.as_str()
                )));
            }
            Ok(Expr::Arith(
                op,
                args.iter()
                    .map(|a| Self::from_value_inner(a, depth + 1, nodes))
                    .collect::<Result<Vec<_>>>()?,
            ))
        };

        match op {
            "+" => arith(ArithOp::Add, args, nodes),
            "-" => arith(ArithOp::Sub, args, nodes),
            "*" => arith(ArithOp::Mul, args, nodes),
            "/" => arith(ArithOp::Div, args, nodes),
            "min" => arith(ArithOp::Min, args, nodes),
            "max" => arith(ArithOp::Max, args, nodes),
            "round" => {
                if args.len() != 1 {
                    return Err(Error::Expr("round takes exactly 1 argument".into()));
                }
                Ok(Expr::Round(Box::new(sub(&args[0], nodes)?)))
            }
            "=" | "!=" | "<" | "<=" | ">" | ">=" => {
                if args.len() != 2 {
                    return Err(Error::Expr(format!("{op:?} takes exactly 2 arguments")));
                }
                let cmp = match op {
                    "=" => CmpOp::Eq,
                    "!=" => CmpOp::Ne,
                    "<" => CmpOp::Lt,
                    "<=" => CmpOp::Le,
                    ">" => CmpOp::Gt,
                    _ => CmpOp::Ge,
                };
                Ok(Expr::Cmp(
                    cmp,
                    Box::new(sub(&args[0], nodes)?),
                    Box::new(sub(&args[1], nodes)?),
                ))
            }
            "and" | "or" => {
                if args.is_empty() {
                    return Err(Error::Expr(format!("{op:?} needs at least 1 argument")));
                }
                let parsed = args
                    .iter()
                    .map(|a| Self::from_value_inner(a, depth + 1, nodes))
                    .collect::<Result<Vec<_>>>()?;
                Ok(if op == "and" { Expr::And(parsed) } else { Expr::Or(parsed) })
            }
            "not" => {
                if args.len() != 1 {
                    return Err(Error::Expr("not takes exactly 1 argument".into()));
                }
                Ok(Expr::Not(Box::new(sub(&args[0], nodes)?)))
            }
            "if" => {
                if args.len() != 3 {
                    return Err(Error::Expr("if takes exactly 3 arguments".into()));
                }
                Ok(Expr::If(
                    Box::new(sub(&args[0], nodes)?),
                    Box::new(sub(&args[1], nodes)?),
                    Box::new(sub(&args[2], nodes)?),
                ))
            }
            "match" => {
                if args.len() < 2 {
                    return Err(Error::Expr(
                        "match needs a field-ref, arms, and a default".into(),
                    ));
                }
                let field = field_ref(&args[0])?;
                let mut arms = Vec::new();
                for arm in &args[1..args.len() - 1] {
                    let pair = arm
                        .as_array()
                        .filter(|a| a.len() == 2)
                        .ok_or_else(|| Error::Expr("match arm must be [pattern, expr]".into()))?;
                    let pat = pair[0]
                        .as_text()
                        .ok_or_else(|| Error::Expr("match pattern must be a string".into()))?;
                    arms.push((pat.to_owned(), sub(&pair[1], nodes)?));
                }
                let default = sub(&args[args.len() - 1], nodes)?;
                Ok(Expr::Match(field, arms, Box::new(default)))
            }
            "regex-valid" => {
                if args.len() != 2 {
                    return Err(Error::Expr(
                        "regex-valid takes a field-ref and a pattern".into(),
                    ));
                }
                let field = field_ref(&args[0])?;
                let pattern = args[1]
                    .as_text()
                    .ok_or_else(|| Error::Expr("regex pattern must be a string".into()))?;
                compile_anchored(pattern)?; // validate at decode time
                Ok(Expr::RegexValid(field, pattern.to_owned()))
            }
            other => Err(Error::Expr(format!("unknown operation {other:?}"))),
        }
    }

    /// Collect every field id this expression reads.
    pub fn field_refs(&self, out: &mut Vec<String>) {
        match self {
            Expr::Num(_) | Expr::Str(_) | Expr::Bool(_) => {}
            Expr::FieldRef(id) => out.push(id.clone()),
            Expr::Arith(_, args) | Expr::And(args) | Expr::Or(args) => {
                for a in args {
                    a.field_refs(out);
                }
            }
            Expr::Round(e) | Expr::Not(e) => e.field_refs(out),
            Expr::Cmp(_, a, b) => {
                a.field_refs(out);
                b.field_refs(out);
            }
            Expr::If(c, t, e) => {
                c.field_refs(out);
                t.field_refs(out);
                e.field_refs(out);
            }
            Expr::Match(field, arms, default) => {
                out.push(field.clone());
                for (_, e) in arms {
                    e.field_refs(out);
                }
                default.field_refs(out);
            }
            Expr::RegexValid(field, _) => out.push(field.clone()),
        }
    }

    /// Evaluate against a set of field values. Total: cost is O(tree size),
    /// regex matching is linear in input, there is no recursion beyond the
    /// (depth-bounded) expression tree itself.
    pub fn eval(&self, fields: &BTreeMap<String, FieldValue>) -> Result<FieldValue> {
        Ok(match self {
            Expr::Num(x) => FieldValue::Num(*x),
            Expr::Str(s) => FieldValue::Str(s.clone()),
            Expr::Bool(b) => FieldValue::Bool(*b),
            Expr::FieldRef(id) => fields.get(id).cloned().unwrap_or(FieldValue::Empty),
            Expr::Arith(op, args) => {
                let nums = args
                    .iter()
                    .map(|a| a.eval(fields)?.as_num())
                    .collect::<Result<Vec<f64>>>()?;
                let mut acc = nums[0];
                for &x in &nums[1..] {
                    acc = match op {
                        ArithOp::Add => acc + x,
                        ArithOp::Sub => acc - x,
                        ArithOp::Mul => acc * x,
                        ArithOp::Div => acc / x,
                        ArithOp::Min => acc.min(x),
                        ArithOp::Max => acc.max(x),
                    };
                }
                FieldValue::Num(acc)
            }
            Expr::Round(e) => {
                let x = e.eval(fields)?.as_num()?;
                // Round half to even, like IEEE 754 default rounding.
                let r = x.round();
                let v = if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
                    r - (x - x.trunc()).signum()
                } else {
                    r
                };
                FieldValue::Num(v)
            }
            Expr::Cmp(op, a, b) => {
                let av = a.eval(fields)?;
                let bv = b.eval(fields)?;
                let res = match (op, &av, &bv) {
                    (CmpOp::Eq, _, _) => av == bv,
                    (CmpOp::Ne, _, _) => av != bv,
                    (_, FieldValue::Num(x), FieldValue::Num(y)) => match op {
                        CmpOp::Lt => x < y,
                        CmpOp::Le => x <= y,
                        CmpOp::Gt => x > y,
                        CmpOp::Ge => x >= y,
                        _ => unreachable!(),
                    },
                    (_, FieldValue::Str(x), FieldValue::Str(y)) => match op {
                        CmpOp::Lt => x < y,
                        CmpOp::Le => x <= y,
                        CmpOp::Gt => x > y,
                        CmpOp::Ge => x >= y,
                        _ => unreachable!(),
                    },
                    _ => {
                        return Err(Error::Expr(
                            "ordered comparison requires two numbers or two strings".into(),
                        ))
                    }
                };
                FieldValue::Bool(res)
            }
            Expr::And(args) => {
                let mut acc = true;
                for a in args {
                    acc = acc && a.eval(fields)?.as_bool()?;
                }
                FieldValue::Bool(acc)
            }
            Expr::Or(args) => {
                let mut acc = false;
                for a in args {
                    acc = acc || a.eval(fields)?.as_bool()?;
                }
                FieldValue::Bool(acc)
            }
            Expr::Not(e) => FieldValue::Bool(!e.eval(fields)?.as_bool()?),
            Expr::If(c, t, e) => {
                if c.eval(fields)?.as_bool()? {
                    t.eval(fields)?
                } else {
                    e.eval(fields)?
                }
            }
            Expr::Match(field, arms, default) => {
                let val = fields.get(field).cloned().unwrap_or(FieldValue::Empty);
                let text = match &val {
                    FieldValue::Str(s) => s.clone(),
                    FieldValue::Num(x) => format_num(*x),
                    FieldValue::Bool(b) => b.to_string(),
                    FieldValue::Empty => String::new(),
                };
                let mut result = None;
                for (pat, e) in arms {
                    if *pat == text {
                        result = Some(e.eval(fields)?);
                        break;
                    }
                }
                match result {
                    Some(r) => r,
                    None => default.eval(fields)?,
                }
            }
            Expr::RegexValid(field, pattern) => {
                let re = compile_anchored(pattern)?;
                let text = match fields.get(field) {
                    Some(FieldValue::Str(s)) => s.clone(),
                    Some(FieldValue::Num(x)) => format_num(*x),
                    _ => String::new(),
                };
                FieldValue::Bool(re.is_match(&text))
            }
        })
    }
}

impl FieldValue {
    fn as_num(&self) -> Result<f64> {
        match self {
            FieldValue::Num(x) => Ok(*x),
            FieldValue::Empty => Ok(0.0),
            _ => Err(Error::Expr("expected a number".into())),
        }
    }

    fn as_bool(&self) -> Result<bool> {
        match self {
            FieldValue::Bool(b) => Ok(*b),
            _ => Err(Error::Expr("expected a boolean".into())),
        }
    }
}

fn format_num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

fn field_ref(v: &Value) -> Result<String> {
    let m = v
        .as_map()
        .filter(|m| m.len() == 1)
        .ok_or_else(|| Error::Expr("field-ref must be {\"$\": id}".into()))?;
    if m[0].0.as_text() != Some("$") {
        return Err(Error::Expr("field-ref must be {\"$\": id}".into()));
    }
    m[0].1
        .as_text()
        .map(str::to_owned)
        .ok_or_else(|| Error::Expr("field id must be a string".into()))
}

/// Compile a pattern as fully anchored. The `regex` crate is RE2-class:
/// guaranteed linear-time matching, no backreferences, no lookaround —
/// exactly the ReDoS-immune subset the spec requires.
pub fn compile_anchored(pattern: &str) -> Result<regex::Regex> {
    if pattern.len() > MAX_REGEX_LEN {
        return Err(Error::Expr("regex pattern too long".into()));
    }
    regex::RegexBuilder::new(&format!("\\A(?:{pattern})\\z"))
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
        .map_err(|e| Error::Expr(format!("invalid regex: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, FieldValue)]) -> BTreeMap<String, FieldValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn arithmetic_and_roundtrip() {
        // total = round(qty * price)
        let e = Expr::Round(Box::new(Expr::Arith(
            ArithOp::Mul,
            vec![Expr::FieldRef("qty".into()), Expr::FieldRef("price".into())],
        )));
        let v = e.to_value();
        let bytes = v.encode().unwrap();
        let back = Expr::from_value(&Value::decode(&bytes).unwrap()).unwrap();
        assert_eq!(e, back);

        let env = fields(&[
            ("qty", FieldValue::Num(3.0)),
            ("price", FieldValue::Num(2.5)),
        ]);
        assert_eq!(back.eval(&env).unwrap(), FieldValue::Num(8.0)); // 7.5 → 8 (half to even)
    }

    #[test]
    fn regex_valid_is_anchored() {
        let e = Expr::RegexValid("zip".into(), "[0-9]{5}".into());
        let yes = fields(&[("zip", FieldValue::Str("12345".into()))]);
        let no = fields(&[("zip", FieldValue::Str("12345 extra".into()))]);
        assert_eq!(e.eval(&yes).unwrap(), FieldValue::Bool(true));
        assert_eq!(e.eval(&no).unwrap(), FieldValue::Bool(false));
    }

    #[test]
    fn backreferences_rejected() {
        // Backreferences (the ReDoS vector) are not RE2-expressible.
        let v = Value::Array(vec![
            Value::text("regex-valid"),
            MapBuilder::new().put("$", Value::text("x")).build(),
            Value::text(r"(a+)\1"),
        ]);
        assert!(Expr::from_value(&v).is_err());
    }

    #[test]
    fn depth_bounded() {
        // Build an expression nested past MAX_EXPR_DEPTH.
        let mut v = Value::Float(1.0);
        for _ in 0..(MAX_EXPR_DEPTH + 2) {
            v = Value::Array(vec![Value::text("not"), v]);
        }
        assert!(Expr::from_value(&v).is_err());
    }

    #[test]
    fn match_expression() {
        let e = Expr::Match(
            "country".into(),
            vec![
                ("NZ".into(), Expr::Num(0.15)),
                ("AU".into(), Expr::Num(0.10)),
            ],
            Box::new(Expr::Num(0.0)),
        );
        let env = fields(&[("country", FieldValue::Str("NZ".into()))]);
        assert_eq!(e.eval(&env).unwrap(), FieldValue::Num(0.15));
        let env2 = fields(&[("country", FieldValue::Str("US".into()))]);
        assert_eq!(e.eval(&env2).unwrap(), FieldValue::Num(0.0));
    }
}
