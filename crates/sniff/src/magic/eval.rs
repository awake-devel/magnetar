//! Rule tree evaluation against the known bytes.
//!
//! A node resolves its offset, runs its test, then its children in order. A required child
//! that fails fails the node (and drops what the node and its children captured); an optional
//! child that fails is skipped. Label/confidence overrides and `set` values of the deepest
//! matching node win (later siblings override earlier ones). Bytes that are not known make the
//! test "missing": at the top level the rule is undecided (and may ask for the bytes when the
//! file is otherwise unidentified); below it, a required child that is missing yields a partial
//! match at lower confidence that asks for the range.

use std::borrow::Cow;

use crate::magic::parse::{Class, Cmp, Node, NumKind, Offset, Operand, Rule, Test, TextKind};
use crate::sparse::Range;
use crate::types::{Field, Hit, Value};
use crate::util::{cstr, us, utf16_field};
use crate::view::View;

/// Confidence lost by a rule whose required continuation bytes are missing.
const PARTIAL_PENALTY: u8 = 20;

/// Highest confidence a rule may report for the evidence its matched tests carry, in bits
/// (-log2 of the chance the tests pass on random bytes): `root` from the top-level test (the
/// magic), `total` from every matched test.
///
/// - 95+ means "signature plus a passing structural check" ([`crate::Detection::confidence`]):
///   it needs 64 bits, a long magic or a magic plus structural fields (sizes, versions, a
///   second marker). Below that the rule stays at 94 whatever it says.
/// - A short magic (under [`LONG_MAGIC_BITS`]: shorter than 8 bytes) whose continuations add
///   less than [`WEAK_CHECK_BITS`] is a hint, not a fact: at most [`WEAK_CONFIDENCE`], under
///   the 80 that callers store.
pub(crate) fn confidence_cap(root: f64, total: f64) -> u8 {
    if total >= 64.0 {
        100
    } else if root < LONG_MAGIC_BITS && total - root < WEAK_CHECK_BITS {
        WEAK_CONFIDENCE
    } else {
        94
    }
}

/// Evidence of a long magic: 8 bytes, less a few bits for alternatives (8 bytes with up to 16
/// alternatives still count; 7 bytes do not).
pub(crate) const LONG_MAGIC_BITS: f64 = 60.0;

/// Evidence the continuations of a short magic must add to count as a structural check
/// (1 in 256 on random bytes).
pub(crate) const WEAK_CHECK_BITS: f64 = 8.0;

/// Highest confidence of a short magic without a passing structural check (shown, not stored).
pub(crate) const WEAK_CONFIDENCE: u8 = crate::types::WEAK;

fn class_bits(c: Class) -> f64 {
    let n: f64 = match c {
        Class::Printable => 95.0,
        Class::Alnum => 62.0,
        Class::UpperAlnum => 36.0,
        Class::Digit => 10.0,
        Class::Hex => 22.0,
        Class::Graphic => 94.0,
        Class::Whitespace => 6.0,
    };
    (256.0 / n).log2()
}

/// Evidence of a numeric test that matched: -log2 of the chance a random value passes.
fn num_bits(kind: NumKind, mask: Option<u64>, cmps: &[Cmp], size: u64) -> f64 {
    let b = mask.map_or(kind.width() as u32 * 8, u64::count_ones);
    let d = 2f64.powi(b as i32);
    let c = |v: u64| (v as f64).min(d);
    let p: f64 = cmps
        .iter()
        .map(|cmp| match *cmp {
            Cmp::Any | Cmp::Ne(_) => d,
            Cmp::Eq(_) => 1.0,
            Cmp::Lt(o) => c(operand(o, size)),
            Cmp::Le(o) => c(operand(o, size).saturating_add(1)),
            Cmp::Gt(o) => d - c(operand(o, size).saturating_add(1)),
            Cmp::Ge(o) => d - c(operand(o, size)),
            Cmp::AllSet(m) | Cmp::NoneSet(m) => d / 2f64.powi(m.count_ones() as i32),
            Cmp::Range(lo, hi, incl) => (c(hi.saturating_add(u64::from(incl))) - c(lo)).max(0.0),
        })
        .sum::<f64>()
        / d;
    if p <= 0.0 {
        0.0
    } else {
        (-p.min(1.0).log2()).max(0.0)
    }
}

pub(crate) enum RuleResult {
    Matched(Box<Hit>),
    NoMatch,
    /// The top-level test needs these bytes.
    Missing(Range),
}

enum Ev {
    /// Matched; the end offset of the matched bytes (for relative children).
    Match(u64),
    NoMatch,
    Missing(Range),
}

#[derive(Default)]
struct State {
    fields: Vec<Field>,
    label: Option<String>,
    confidence: Option<u8>,
    missing: Vec<Range>,
    /// Evidence of the tests matched so far (see [`confidence_cap`]).
    bits: f64,
    /// Evidence of the top-level test alone (set once, before any child runs).
    root_bits: Option<f64>,
}

type Mark = (usize, usize, Option<String>, Option<u8>, f64);

impl State {
    fn mark(&self) -> Mark {
        (
            self.fields.len(),
            self.missing.len(),
            self.label.clone(),
            self.confidence,
            self.bits,
        )
    }
    fn rollback(&mut self, m: Mark) {
        self.fields.truncate(m.0);
        self.missing.truncate(m.1);
        self.label = m.2;
        self.confidence = m.3;
        self.bits = m.4;
    }
}

fn resolve(v: &View, off: &Offset, relative: bool, parent_end: u64) -> Result<u64, Ev> {
    let size = v.size();
    match *off {
        Offset::Start(o) if relative => Ok(parent_end.saturating_add(o)),
        Offset::Start(o) => Ok(o),
        Offset::End(o) => size.checked_sub(o).ok_or(Ev::NoMatch),
        Offset::Indirect { at, kind, add, mul } => {
            let pos = if at >= 0 {
                at.unsigned_abs()
            } else {
                size.checked_sub(at.unsigned_abs()).ok_or(Ev::NoMatch)?
            };
            let w = kind.width();
            if pos.saturating_add(w as u64) > size {
                return Err(Ev::NoMatch);
            }
            let b = v.at(pos, w).ok_or(Ev::Missing(Range::new(pos, w as u64)))?;
            let val = kind.read(b).ok_or(Ev::NoMatch)?.saturating_mul(mul);
            let r = if add >= 0 {
                val.checked_add(add.unsigned_abs())
            } else {
                val.checked_sub(add.unsigned_abs())
            };
            r.ok_or(Ev::NoMatch)
        }
    }
}

fn operand(o: Operand, size: u64) -> u64 {
    match o {
        Operand::Lit(v) => v,
        Operand::Size => size,
    }
}

fn cmp_ok(c: &Cmp, x: u64, size: u64) -> bool {
    match *c {
        Cmp::Any => true,
        Cmp::Eq(o) => x == operand(o, size),
        Cmp::Ne(o) => x != operand(o, size),
        Cmp::Lt(o) => x < operand(o, size),
        Cmp::Le(o) => x <= operand(o, size),
        Cmp::Gt(o) => x > operand(o, size),
        Cmp::Ge(o) => x >= operand(o, size),
        Cmp::AllSet(m) => x & m == m,
        Cmp::NoneSet(m) => x & m == 0,
        Cmp::Range(lo, hi, incl) => x >= lo && (if incl { x <= hi } else { x < hi }),
    }
}

fn class_ok(c: Class, b: &[u8]) -> bool {
    let f: fn(&u8) -> bool = match c {
        Class::Printable => |c| (0x20..0x7f).contains(c),
        Class::Alnum => u8::is_ascii_alphanumeric,
        Class::UpperAlnum => |c| c.is_ascii_uppercase() || c.is_ascii_digit(),
        Class::Digit => u8::is_ascii_digit,
        Class::Hex => u8::is_ascii_hexdigit,
        Class::Graphic => u8::is_ascii_graphic,
        Class::Whitespace => u8::is_ascii_whitespace,
    };
    !b.is_empty() && b.iter().all(f)
}

/// Runs one node's own test at `off`; on a match also the evidence it carries, in bits.
fn run_test(v: &View, t: &Test, off: u64) -> (Ev, Option<Value>, f64) {
    let (ev, val) = run_test_inner(v, t, off);
    let bits = match ev {
        Ev::Match(end) => test_bits(v, t, val.as_ref(), off, end),
        _ => 0.0,
    };
    (ev, val, bits)
}

fn test_bits(v: &View, t: &Test, val: Option<&Value>, off: u64, end: u64) -> f64 {
    match t {
        Test::Num {
            kind, mask, cmps, ..
        } => num_bits(*kind, *mask, cmps, v.size()),
        Test::Bytes { alts, nocase } => {
            let n = end.saturating_sub(off) as f64;
            let per = if *nocase { 7.0 } else { 8.0 };
            (n * per - (alts.len() as f64).log2()).max(0.0)
        }
        Test::Search { alts, range } => {
            let at = val.and_then(Value::as_u64).unwrap_or(off);
            let n = end.saturating_sub(at) as f64;
            (n * 8.0 - ((*range as f64) + 1.0).log2() - (alts.len() as f64).log2()).max(0.0)
        }
        Test::Text { kind, class, len } => {
            let Some(c) = class else { return 0.0 };
            let n = match kind {
                TextKind::Cstring => {
                    let b = v.from(off);
                    let b = crate::util::pre(b, us(*len));
                    b.iter().position(|&x| x == 0).unwrap_or(b.len())
                }
                TextKind::Ascii => us(*len),
                TextKind::Utf16Le | TextKind::Utf16Be => 0,
            };
            n as f64 * class_bits(*c)
        }
    }
}

fn run_test_inner(v: &View, t: &Test, off: u64) -> (Ev, Option<Value>) {
    let size = v.size();
    match t {
        Test::Num {
            kind,
            mask,
            shift,
            cmps,
        } => {
            let w = kind.width();
            if off.saturating_add(w as u64) > size {
                return (Ev::NoMatch, None);
            }
            let Some(b) = v.at(off, w) else {
                return (Ev::Missing(Range::new(off, w as u64)), None);
            };
            let Some(mut x) = kind.read(b) else {
                return (Ev::NoMatch, None);
            };
            if let Some(m) = mask {
                x &= m;
            }
            x >>= shift;
            if cmps.iter().any(|c| cmp_ok(c, x, size)) {
                (Ev::Match(off + w as u64), Some(Value::U64(x)))
            } else {
                (Ev::NoMatch, None)
            }
        }
        Test::Bytes { alts, nocase } => {
            let mut missing: Option<Range> = None;
            for a in alts {
                let n = a.len() as u64;
                if off.saturating_add(n) > size {
                    continue;
                }
                match v.at(off, a.len()) {
                    Some(b) => {
                        let eq = if *nocase {
                            b.eq_ignore_ascii_case(a)
                        } else {
                            b == a.as_slice()
                        };
                        if eq {
                            return (
                                Ev::Match(off + n),
                                Some(Value::Str(String::from_utf8_lossy(b).into_owned())),
                            );
                        }
                    }
                    None => {
                        let r = Range::new(off, n);
                        missing = Some(missing.map_or(r, |m| if m.len >= n { m } else { r }));
                    }
                }
            }
            (missing.map_or(Ev::NoMatch, Ev::Missing), None)
        }
        Test::Search { alts, range } => {
            let longest = alts.iter().map(Vec::len).max().unwrap_or(0) as u64;
            let window_end = off.saturating_add(*range).saturating_add(longest).min(size);
            if off >= window_end {
                return (Ev::NoMatch, None);
            }
            let want = window_end - off;
            let have = v.from(off);
            let avail = crate::util::pre(have, us(want));
            let best = alts
                .iter()
                .filter_map(|a| crate::util::find(avail, a).map(|p| (p, a.len())))
                .min();
            if let Some((p, n)) = best {
                let at = off + p as u64;
                if p as u64 <= *range {
                    return (Ev::Match(at + n as u64), Some(Value::U64(at)));
                }
            }
            if (avail.len() as u64) < want {
                (Ev::Missing(Range::new(off, want)), None)
            } else {
                (Ev::NoMatch, None)
            }
        }
        Test::Text { kind, len, class } => {
            if off >= size {
                return (Ev::NoMatch, None);
            }
            let n = (*len).min(size - off);
            if *kind == TextKind::Ascii && n < *len {
                return (Ev::NoMatch, None);
            }
            let Some(b) = v.at(off, us(n)) else {
                return (Ev::Missing(Range::new(off, n)), None);
            };
            let text = match kind {
                TextKind::Ascii => {
                    if let Some(c) = class
                        && !class_ok(*c, b)
                    {
                        return (Ev::NoMatch, None);
                    }
                    String::from_utf8_lossy(b).trim().to_string()
                }
                TextKind::Cstring => {
                    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                    let raw = crate::util::pre(b, end);
                    if let Some(c) = class
                        && !class_ok(*c, raw)
                    {
                        return (Ev::NoMatch, None);
                    }
                    cstr(b)
                }
                TextKind::Utf16Le | TextKind::Utf16Be => {
                    utf16_field(b, 0, b.len() & !1, *kind == TextKind::Utf16Be).unwrap_or_default()
                }
            };
            (Ev::Match(off + n), Some(Value::Str(text)))
        }
    }
}

fn eval_node(v: &View, n: &Node, parent_end: u64, st: &mut State) -> Ev {
    let off = match resolve(v, &n.offset, n.relative, parent_end) {
        Ok(o) => o,
        Err(e) => return e,
    };
    let (ev, val, bits) = run_test(v, &n.test, off);
    let Ev::Match(end) = ev else { return ev };
    let mark = st.mark();
    st.root_bits.get_or_insert(bits);
    st.bits += bits;
    if let (Some(f), Some(val)) = (&n.field, val) {
        st.fields.push(Field {
            key: Cow::Owned(f.clone()),
            value: val,
        });
    }
    for (k, val) in &n.set {
        st.fields.retain(|f| f.key != k.as_str());
        st.fields.push(Field {
            key: Cow::Owned(k.clone()),
            value: Value::Str(val.clone()),
        });
    }
    if let Some(note) = &n.note {
        st.fields.push(Field {
            key: Cow::Borrowed("note"),
            value: Value::Str(note.clone()),
        });
    }
    if let Some(l) = &n.label {
        st.label = Some(l.clone());
    }
    if let Some(c) = n.confidence {
        st.confidence = Some(c);
    }
    for c in &n.children {
        let cm = st.mark();
        match eval_node(v, c, end, st) {
            Ev::Match(_) => {}
            Ev::NoMatch if c.optional => st.rollback(cm),
            Ev::NoMatch => {
                st.rollback(mark);
                return Ev::NoMatch;
            }
            Ev::Missing(_) if c.optional => st.rollback(cm),
            Ev::Missing(r) => {
                st.rollback(cm);
                st.missing.push(r);
            }
        }
    }
    Ev::Match(end)
}

/// Evaluates one rule.
pub(crate) fn eval_rule(v: &View, r: &Rule) -> RuleResult {
    let mut st = State::default();
    match eval_node(v, &r.root, 0, &mut st) {
        Ev::NoMatch => RuleResult::NoMatch,
        Ev::Missing(m) => RuleResult::Missing(m),
        Ev::Match(_) => {
            let mut conf = st
                .confidence
                .unwrap_or(r.confidence)
                .min(confidence_cap(st.root_bits.unwrap_or(0.0), st.bits));
            if !st.missing.is_empty() {
                conf = conf.saturating_sub(PARTIAL_PENALTY);
            }
            let label = st.label.unwrap_or_else(|| r.label.clone());
            RuleResult::Matched(Box::new(Hit {
                label: Cow::Owned(label),
                conf,
                fields: st.fields,
                need: st.missing,
                game: None,
                source: Cow::Owned(r.id.clone()),
                strength: r.strength,
                mime: r.mime.clone(),
                description: r.description.clone(),
                unverified: false,
            }))
        }
    }
}
