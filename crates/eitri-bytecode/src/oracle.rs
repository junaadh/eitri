//! The semantic oracle (T004): reference meanings of operations, written
//! independently of the interpreter's handlers so the two can check each
//! other.
//!
//! Integer operations compute the exact mathematical result in `i128`, then
//! range-check it against the operand type; the handlers instead use Rust's
//! `checked_*` and `wrapping_*` on 64-bit values. The model is parameterized
//! by width and signedness, so narrow types (T013) can be checked
//! exhaustively at eight bits. Float conditions are decided from the
//! partial order, independently of `FloatCond::eval`. No performance claims:
//! this is for correctness only.

use crate::spec::{Cond, FloatCond};

/// A run-time fault, by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// `integer overflow`.
    Overflow,
    /// `division by zero`.
    DivisionByZero,
    /// A shift amount outside `0..bits`.
    ShiftAmount,
}

/// An integer type: its width in bits and signedness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntType {
    pub bits: u32,
    pub signed: bool,
}

impl IntType {
    pub const I64: IntType = IntType {
        bits: 64,
        signed: true,
    };
    pub const U64: IntType = IntType {
        bits: 64,
        signed: false,
    };
    pub const I8: IntType = IntType {
        bits: 8,
        signed: true,
    };
    pub const U8: IntType = IntType {
        bits: 8,
        signed: false,
    };

    pub fn min(self) -> i128 {
        if self.signed {
            -(1i128 << (self.bits - 1))
        } else {
            0
        }
    }

    pub fn max(self) -> i128 {
        if self.signed {
            (1i128 << (self.bits - 1)) - 1
        } else {
            (1i128 << self.bits) - 1
        }
    }

    pub fn contains(self, value: i128) -> bool {
        (self.min()..=self.max()).contains(&value)
    }

    /// `value` reduced modulo 2^bits into the type's range.
    pub fn wrap(self, value: i128) -> i128 {
        let modulus = 1i128 << self.bits;
        let low = value.rem_euclid(modulus);
        if self.signed && low > self.max() {
            low - modulus
        } else {
            low
        }
    }

    fn checked(self, value: i128) -> Result<i128, Fault> {
        if self.contains(value) {
            Ok(value)
        } else {
            Err(Fault::Overflow)
        }
    }
}

/// Integer arithmetic of the language on values of type `ty` (given as
/// their mathematical values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arith {
    Add,
    Sub,
    Mul,
    WrapAdd,
    WrapSub,
    WrapMul,
    /// Truncates toward zero.
    Div,
    /// Takes the sign of the dividend.
    Rem,
    Shl,
    Shr,
}

pub fn arith(ty: IntType, op: Arith, a: i128, b: i128) -> Result<i128, Fault> {
    debug_assert!(ty.contains(a) && (matches!(op, Arith::Shl | Arith::Shr) || ty.contains(b)));
    match op {
        Arith::Add => ty.checked(a + b),
        Arith::Sub => ty.checked(a - b),
        Arith::Mul if !ty.signed => {
            let value = (a as u128) * (b as u128);
            if value > ty.max() as u128 {
                Err(Fault::Overflow)
            } else {
                Ok(value as i128)
            }
        }
        Arith::Mul => ty.checked(a * b),
        Arith::WrapAdd => Ok(ty.wrap(a + b)),
        Arith::WrapSub => Ok(ty.wrap(a - b)),
        Arith::WrapMul if !ty.signed => Ok(((a as u128 * b as u128) & ty.max() as u128) as i128),
        Arith::WrapMul => Ok(ty.wrap(a * b)),
        Arith::Div | Arith::Rem => {
            if b == 0 {
                return Err(Fault::DivisionByZero);
            }
            // Truncating division, defined from magnitudes.
            let magnitude = a.abs() / b.abs();
            let quotient = if (a < 0) != (b < 0) {
                -magnitude
            } else {
                magnitude
            };
            if op == Arith::Div {
                ty.checked(quotient)
            } else {
                // The remainder always fits: |r| < |b|. `MIN % -1` is 0.
                Ok(a - quotient * b)
            }
        }
        Arith::Shl | Arith::Shr => {
            if !(0..ty.bits as i128).contains(&b) {
                return Err(Fault::ShiftAmount);
            }
            Ok(if op == Arith::Shl {
                // Bits shifted out are dropped; not checked for overflow.
                ty.wrap(a << b)
            } else {
                // Arithmetic for signed, logical for unsigned: floor division.
                a >> b
            })
        }
    }
}

/// Checked negation.
pub fn neg(ty: IntType, a: i128) -> Result<i128, Fault> {
    ty.checked(-a)
}

/// Integer comparison conditions on 64-bit slot contents, decided from the
/// mathematical values of the signed or unsigned reading.
pub fn int_cond(cond: Cond, a: i64, b: i64) -> bool {
    let (sa, sb) = (a as i128, b as i128);
    let (ua, ub) = (a as u64 as i128, b as u64 as i128);
    match cond {
        Cond::Eq => sa == sb,
        Cond::Ne => sa != sb,
        Cond::Lt => sa < sb,
        Cond::Le => sa <= sb,
        Cond::Gt => sa > sb,
        Cond::Lo => ua < ub,
        Cond::Ls => ua <= ub,
        Cond::Hi => ua > ub,
    }
}

/// Float conditions from the IEEE partial order: `None` means unordered.
pub fn float_cond(cond: FloatCond, a: f64, b: f64) -> bool {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let order = a.partial_cmp(&b);
    match cond {
        FloatCond::Eq => order == Some(Equal),
        FloatCond::Ne => order != Some(Equal),
        FloatCond::Lt => order == Some(Less),
        FloatCond::Le => matches!(order, Some(Less | Equal)),
        FloatCond::Ult => order != Some(Greater) && order != Some(Equal),
        FloatCond::Ule => order != Some(Greater),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const I64_EDGES: [i64; 9] = [
        i64::MIN,
        i64::MIN + 1,
        -2,
        -1,
        0,
        1,
        2,
        i64::MAX - 1,
        i64::MAX,
    ];

    #[test]
    fn i64_checked_and_wrapping_boundaries() {
        let ty = IntType::I64;
        for &a in &I64_EDGES {
            for &b in &I64_EDGES {
                let (x, y) = (a as i128, b as i128);
                let expect = |r: Option<i64>| r.map(|v| v as i128).ok_or(Fault::Overflow);
                assert_eq!(arith(ty, Arith::Add, x, y), expect(a.checked_add(b)));
                assert_eq!(arith(ty, Arith::Sub, x, y), expect(a.checked_sub(b)));
                assert_eq!(arith(ty, Arith::Mul, x, y), expect(a.checked_mul(b)));
                assert_eq!(
                    arith(ty, Arith::WrapAdd, x, y),
                    Ok(a.wrapping_add(b) as i128)
                );
                assert_eq!(
                    arith(ty, Arith::WrapSub, x, y),
                    Ok(a.wrapping_sub(b) as i128)
                );
                assert_eq!(
                    arith(ty, Arith::WrapMul, x, y),
                    Ok(a.wrapping_mul(b) as i128)
                );
                if b == 0 {
                    assert_eq!(arith(ty, Arith::Div, x, y), Err(Fault::DivisionByZero));
                    assert_eq!(arith(ty, Arith::Rem, x, y), Err(Fault::DivisionByZero));
                } else {
                    assert_eq!(arith(ty, Arith::Div, x, y), expect(a.checked_div(b)));
                    // Unlike Rust's checked_rem, `MIN % -1` is 0, not an error.
                    assert_eq!(arith(ty, Arith::Rem, x, y), Ok(a.wrapping_rem(b) as i128));
                }
            }
            assert_eq!(
                neg(ty, a as i128),
                a.checked_neg().map(|v| v as i128).ok_or(Fault::Overflow)
            );
        }
        // The first slice's documented cases.
        assert_eq!(arith(ty, Arith::Div, -7, 2), Ok(-3));
        assert_eq!(arith(ty, Arith::Rem, -7, 2), Ok(-1));
        assert_eq!(arith(ty, Arith::Rem, 7, -2), Ok(1));
        assert_eq!(arith(ty, Arith::Rem, i64::MIN as i128, -1), Ok(0));
        assert_eq!(
            arith(ty, Arith::Div, i64::MIN as i128, -1),
            Err(Fault::Overflow)
        );
    }

    /// Every pair of 8-bit values, every operation, against Rust's own
    /// fixed-width arithmetic: the narrow model is exact.
    #[test]
    fn eight_bit_models_are_exhaustive() {
        for ty in [IntType::I8, IntType::U8] {
            for a in ty.min()..=ty.max() {
                for b in ty.min()..=ty.max() {
                    for op in [
                        Arith::Add,
                        Arith::Sub,
                        Arith::Mul,
                        Arith::WrapAdd,
                        Arith::WrapSub,
                        Arith::WrapMul,
                        Arith::Div,
                        Arith::Rem,
                        Arith::Shl,
                        Arith::Shr,
                    ] {
                        let got = arith(ty, op, a, b);
                        let want = if ty.signed {
                            reference_i8(op, a as i8, b as i8)
                        } else {
                            reference_u8(op, a as u8, b as u8)
                        };
                        assert_eq!(got, want, "{ty:?} {op:?} {a} {b}");
                    }
                }
                let want = if ty.signed {
                    (a as i8).checked_neg().map(i128::from)
                } else {
                    (a as u8).checked_neg().map(i128::from)
                };
                assert_eq!(neg(ty, a), want.ok_or(Fault::Overflow));
            }
        }
    }

    fn reference_i8(op: Arith, a: i8, b: i8) -> Result<i128, Fault> {
        let r = match op {
            Arith::Add => a.checked_add(b),
            Arith::Sub => a.checked_sub(b),
            Arith::Mul => a.checked_mul(b),
            Arith::WrapAdd => Some(a.wrapping_add(b)),
            Arith::WrapSub => Some(a.wrapping_sub(b)),
            Arith::WrapMul => Some(a.wrapping_mul(b)),
            Arith::Div | Arith::Rem if b == 0 => return Err(Fault::DivisionByZero),
            Arith::Div => a.checked_div(b),
            Arith::Rem => Some(a.wrapping_rem(b)),
            Arith::Shl | Arith::Shr if !(0..8).contains(&b) => return Err(Fault::ShiftAmount),
            Arith::Shl => Some(a.wrapping_shl(b as u32)),
            Arith::Shr => Some(a >> b),
        };
        r.map(i128::from).ok_or(Fault::Overflow)
    }

    fn reference_u8(op: Arith, a: u8, b: u8) -> Result<i128, Fault> {
        let r = match op {
            Arith::Add => a.checked_add(b),
            Arith::Sub => a.checked_sub(b),
            Arith::Mul => a.checked_mul(b),
            Arith::WrapAdd => Some(a.wrapping_add(b)),
            Arith::WrapSub => Some(a.wrapping_sub(b)),
            Arith::WrapMul => Some(a.wrapping_mul(b)),
            Arith::Div | Arith::Rem if b == 0 => return Err(Fault::DivisionByZero),
            Arith::Div => a.checked_div(b),
            Arith::Rem => a.checked_rem(b),
            Arith::Shl | Arith::Shr if b >= 8 => return Err(Fault::ShiftAmount),
            Arith::Shl => Some(a.wrapping_shl(b as u32)),
            Arith::Shr => Some(a >> b),
        };
        r.map(i128::from).ok_or(Fault::Overflow)
    }

    #[test]
    fn integer_conditions_agree_with_the_specification() {
        for &a in &I64_EDGES {
            for &b in &I64_EDGES {
                for cond in [
                    Cond::Eq,
                    Cond::Ne,
                    Cond::Lt,
                    Cond::Le,
                    Cond::Gt,
                    Cond::Lo,
                    Cond::Ls,
                    Cond::Hi,
                ] {
                    assert_eq!(int_cond(cond, a, b), cond.eval(a, b), "{cond:?} {a} {b}");
                }
            }
        }
    }

    /// The full float truth table: NaN, infinities, both zeros, subnormals.
    #[test]
    fn float_truth_table() {
        let values = [
            f64::NAN,
            -f64::NAN,
            f64::NEG_INFINITY,
            f64::MIN,
            -1.0,
            -f64::MIN_POSITIVE,
            -5e-324,
            -0.0,
            0.0,
            5e-324,
            f64::MIN_POSITIVE,
            1.0,
            f64::MAX,
            f64::INFINITY,
        ];
        let conds = [
            FloatCond::Eq,
            FloatCond::Ne,
            FloatCond::Lt,
            FloatCond::Le,
            FloatCond::Ult,
            FloatCond::Ule,
        ];
        for &a in &values {
            for &b in &values {
                for cond in conds {
                    assert_eq!(float_cond(cond, a, b), cond.eval(a, b), "{cond:?} {a} {b}");
                }
                let unordered = a.is_nan() || b.is_nan();
                assert_eq!(
                    float_cond(FloatCond::Ne, a, b),
                    !float_cond(FloatCond::Eq, a, b)
                );
                if unordered {
                    assert!(!float_cond(FloatCond::Lt, a, b) && float_cond(FloatCond::Ult, a, b));
                }
            }
        }
        assert!(float_cond(FloatCond::Eq, -0.0, 0.0));
        assert!(!float_cond(FloatCond::Eq, f64::NAN, f64::NAN));
        assert!(float_cond(FloatCond::Lt, f64::NEG_INFINITY, f64::MIN));
    }
}

#[path = "oracle_scalar.rs"]
pub mod scalar;
