//! T013 scalar operations. Safe slot-bit operations shared by the two VMs;
//! the independent reference model lives in `oracle`, not in this module.

use crate::ValueType;
use crate::spec::{FloatConversion, FloatOp, IntOp, Mode, Precision, Sem};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarFault {
    Overflow,
    DivisionByZero,
    ShiftAmount,
    Conversion,
}

/// CVT descriptor: width code 0/1/2/3 = 8/16/32/64, destination signed
/// in bit 2, source signed in bit 3. Other bits are reserved zero.
pub fn integer_format(bits: u32, signed: bool) -> u8 {
    (match bits {
        8 => 0,
        16 => 1,
        32 => 2,
        64 => 3,
        _ => unreachable!(),
    }) | (u8::from(signed) << 2)
}
pub fn format_type(format: u8) -> Option<ValueType> {
    if format & !7 != 0 {
        return None;
    }
    match (format & 3, format & 4 != 0) {
        (0, true) => Some(ValueType::I8),
        (0, false) => Some(ValueType::U8),
        (3, true) => Some(ValueType::I64),
        (3, false) => Some(ValueType::U64),
        _ => None,
    }
}
pub fn narrow(word: u64, bits: u32, signed: bool) -> u64 {
    if bits == 64 {
        word
    } else if signed {
        ((word << (64 - bits)) as i64 >> (64 - bits)) as u64
    } else {
        word & ((1u64 << bits) - 1)
    }
}

pub fn convert(
    word: u64,
    from: ValueType,
    to: ValueType,
    checked: bool,
) -> Result<u64, ScalarFault> {
    use ValueType::*;
    if from.integer() && to.integer() {
        if !checked {
            return Ok(narrow(word, to.bits(), to.signed()));
        }
        let number = if from.signed() {
            word as i64 as i128
        } else {
            word as i128
        };
        let (min, max) = if to.signed() {
            (-(1i128 << (to.bits() - 1)), (1i128 << (to.bits() - 1)) - 1)
        } else {
            (0, (1i128 << to.bits()) - 1)
        };
        if number < min || number > max {
            Err(ScalarFault::Overflow)
        } else {
            Ok(number as u64)
        }
    } else if from.integer() {
        match to {
            F32 => Ok(if from.signed() {
                (word as i64 as f32).to_bits() as u64
            } else {
                (word as f32).to_bits() as u64
            }),
            F64 => Ok(if from.signed() {
                (word as i64 as f64).to_bits()
            } else {
                (word as f64).to_bits()
            }),
            _ => unreachable!("verified numeric conversion"),
        }
    } else if to.integer() {
        let value = if from == F32 {
            f32::from_bits(word as u32) as f64
        } else {
            f64::from_bits(word)
        };
        let value = value.trunc();
        // Exclusive powers-of-two upper bounds avoid rounding MAX upward
        // to a float which the host's saturating casts would incorrectly accept.
        let upper = f64::from_bits(((1023 + to.bits() - u32::from(to.signed())) as u64) << 52);
        let lower = if to.signed() { -upper } else { 0.0 };
        if !value.is_finite() || value < lower || value >= upper {
            return Err(ScalarFault::Conversion);
        }
        Ok(if to.signed() {
            value as i64 as u64
        } else {
            value as u64
        })
    } else {
        Ok(match (from, to) {
            (F32, F64) => (f32::from_bits(word as u32) as f64).to_bits(),
            (F64, F32) => (f64::from_bits(word) as f32).to_bits() as u64,
            _ => word,
        })
    }
}

/// IEEE remainder (nearest quotient, ties even), as specified for `%` on
/// floats. The fmod remainder and adjustment are exact for finite operands;
/// quotient parity comes from modulo twice the divisor without forming x/y.
macro_rules! remainder {
    ($name:ident,$ty:ty) => {
        pub fn $name(x: $ty, y: $ty) -> $ty {
            if !x.is_finite() || y == 0.0 || y.is_nan() {
                return <$ty>::NAN;
            }
            if y.is_infinite() {
                return x;
            }
            let (a, b) = (x.abs(), y.abs());
            let mut r = a % b;
            let odd = if b <= <$ty>::MAX / 2.0 {
                a % (b * 2.0) >= b
            } else {
                a >= b
            };
            if r > b - r || (r == b - r && odd) {
                r -= b;
            }
            if x.is_sign_negative() { -r } else { r }
        }
    };
}
remainder!(remainder32, f32);
remainder!(remainder64, f64);

/// Execute a non-control scalar operation on slot bits. Descriptor `c`
/// is an immediate for ALUI/CVT, or the right operand for register forms.
pub fn eval(sem: Sem, x: u64, y: u64, c: u8) -> Result<u64, ScalarFault> {
    use ScalarFault::*;
    match sem {
        Sem::Convert { checked } => {
            let to = format_type(c & 7).expect("verified conversion format");
            let from = if c & 8 != 0 {
                ValueType::I64
            } else {
                ValueType::U64
            };
            convert(x, from, to, checked)
        }
        Sem::FloatConvert(kind) => {
            let int = format_type(c).expect("verified integer format");
            use FloatConversion::*;
            let (from, to) = match kind {
                IntToF32 => (int, ValueType::F32),
                IntToF64 => (int, ValueType::F64),
                F32ToInt => (ValueType::F32, int),
                F64ToInt => (ValueType::F64, int),
                F32ToF64 => (ValueType::F32, ValueType::F64),
                F64ToF32 => (ValueType::F64, ValueType::F32),
            };
            convert(x, from, to, true)
        }
        Sem::Int { op, mode, imm } => {
            let y = if imm {
                if matches!(op, IntOp::Shl(_) | IntOp::Shr(_)) {
                    c as u64
                } else {
                    c as i8 as i64 as u64
                }
            } else {
                y
            };
            let (a, b) = (x as i64, y as i64);
            let unsigned = matches!(mode, Mode::CheckedUnsigned | Mode::Unsigned);
            let checked = matches!(mode, Mode::CheckedSigned | Mode::CheckedUnsigned);
            Ok(match op {
                IntOp::Add if checked => if unsigned {
                    x.checked_add(y)
                } else {
                    a.checked_add(b).map(|v| v as u64)
                }
                .ok_or(Overflow)?,
                IntOp::Sub if checked => if unsigned {
                    x.checked_sub(y)
                } else {
                    a.checked_sub(b).map(|v| v as u64)
                }
                .ok_or(Overflow)?,
                IntOp::Mul if checked => if unsigned {
                    x.checked_mul(y)
                } else {
                    a.checked_mul(b).map(|v| v as u64)
                }
                .ok_or(Overflow)?,
                IntOp::Add => x.wrapping_add(y),
                IntOp::Sub => x.wrapping_sub(y),
                IntOp::Mul => x.wrapping_mul(y),
                IntOp::Div | IntOp::Rem => {
                    if y == 0 {
                        return Err(DivisionByZero);
                    }
                    if unsigned {
                        if op == IntOp::Div { x / y } else { x % y }
                    } else if op == IntOp::Div {
                        a.checked_div(b).ok_or(Overflow)? as u64
                    } else {
                        a.wrapping_rem(b) as u64
                    }
                }
                IntOp::And => x & y,
                IntOp::Or => x | y,
                IntOp::Xor => x ^ y,
                IntOp::Not => !x,
                IntOp::Neg => {
                    if checked {
                        if unsigned {
                            0u64.checked_sub(x)
                        } else {
                            a.checked_neg().map(|v| v as u64)
                        }
                        .ok_or(Overflow)?
                    } else {
                        x.wrapping_neg()
                    }
                }
                IntOp::Shl(width) | IntOp::Shr(width) => {
                    let bits = width.bits();
                    if y >= bits as u64 {
                        return Err(ShiftAmount);
                    }
                    if matches!(op, IntOp::Shl(_)) {
                        narrow(x << y, bits, mode == Mode::Signed)
                    } else if unsigned {
                        x >> y
                    } else {
                        (a >> y) as u64
                    }
                }
            })
        }
        Sem::Float { op, precision } => {
            macro_rules! float {
                ($ty:ty,$x:expr,$y:expr) => {{
                    let (a, b): ($ty, $ty) = ($x, $y);
                    match op {
                        FloatOp::Add => a + b,
                        FloatOp::Sub => a - b,
                        FloatOp::Mul => a * b,
                        FloatOp::Div => a / b,
                        FloatOp::Neg => -a,
                        FloatOp::Rem => {
                            if precision == Precision::F32 {
                                remainder32(a as f32, b as f32) as $ty
                            } else {
                                remainder64(a as f64, b as f64) as $ty
                            }
                        }
                    }
                }};
            }
            Ok(if precision == Precision::F32 {
                float!(f32, f32::from_bits(x as u32), f32::from_bits(y as u32)).to_bits() as u64
            } else {
                float!(f64, f64::from_bits(x), f64::from_bits(y)).to_bits()
            })
        }
        Sem::FloatSet { cond, precision } => Ok(u64::from(if precision == Precision::F32 {
            cond.eval(
                f32::from_bits(x as u32) as f64,
                f32::from_bits(y as u32) as f64,
            )
        } else {
            cond.eval(f64::from_bits(x), f64::from_bits(y))
        })),
        Sem::Set(cond) => Ok(u64::from(cond.eval(x as i64, y as i64))),
        _ => unreachable!("verified scalar instruction"),
    }
}
