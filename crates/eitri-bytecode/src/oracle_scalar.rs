//! Independent T013 reference arithmetic/conversion: integer mathematical
//! values and IEEE fields, not the VM's host casts or arithmetic handlers.
//! Fixed 34-word addition storage covers the entire finite binary64 range.

use crate::{ValueType, spec::FloatOp};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversionFault {
    Overflow,
    Conversion,
}

fn format(ty: ValueType) -> (u32, i32, u32) {
    if ty == ValueType::F32 {
        (23, 127, 8)
    } else {
        (52, 1023, 11)
    }
}
fn parts(ty: ValueType, word: u64) -> (bool, u128, i32, bool, bool) {
    let (fraction, bias, exponent_bits) = format(ty);
    let sign = word >> (fraction + exponent_bits) != 0;
    let exponent = ((word >> fraction) & ((1 << exponent_bits) - 1)) as i32;
    let mantissa = (word & ((1u64 << fraction) - 1)) as u128;
    let special = exponent == (1 << exponent_bits) - 1;
    (
        sign,
        if exponent == 0 {
            mantissa
        } else {
            mantissa | (1u128 << fraction)
        },
        if exponent == 0 {
            1 - bias - fraction as i32
        } else {
            exponent - bias - fraction as i32
        },
        special,
        special && mantissa != 0,
    )
}
fn infinity(ty: ValueType, sign: bool) -> u64 {
    let (f, _, e) = format(ty);
    (u64::from(sign) << (f + e)) | (((1u64 << e) - 1) << f)
}
fn nan(ty: ValueType) -> u64 {
    let (f, _, _) = format(ty);
    infinity(ty, false) | (1u64 << (f - 1))
}
fn zero(ty: ValueType, sign: bool) -> u64 {
    let (f, _, e) = format(ty);
    u64::from(sign) << (f + e)
}

/// Round an exact magnitude * 2^exponent to the destination IEEE format.
fn pack(ty: ValueType, sign: bool, magnitude: u128, exponent: i32) -> u64 {
    if magnitude == 0 {
        return zero(ty, sign);
    }
    let (fraction, bias, ebits) = format(ty);
    let top = 127 - magnitude.leading_zeros() as i32;
    let mut unit = (exponent + top).max(1 - bias) - fraction as i32;
    let shift = unit - exponent;
    let mut rounded = if shift > 128 {
        0
    } else if shift == 128 {
        u128::from(magnitude > (1u128 << 127))
    } else if shift > 0 {
        let q = magnitude >> shift;
        let rem = magnitude & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        q + u128::from(rem > half || (rem == half && q & 1 != 0))
    } else {
        magnitude << (-shift)
    };
    if rounded >= 1u128 << (fraction + 1) {
        rounded >>= 1;
        unit += 1;
    }
    if rounded == 0 {
        return zero(ty, sign);
    }
    let exponent = if rounded < 1u128 << fraction {
        0
    } else {
        unit + fraction as i32 + bias
    };
    if exponent >= (1 << ebits) - 1 {
        return infinity(ty, sign);
    }
    zero(ty, sign) | ((exponent as u64) << fraction) | (rounded as u64 & ((1u64 << fraction) - 1))
}

pub fn convert(
    word: u64,
    from: ValueType,
    to: ValueType,
    checked: bool,
) -> Result<u64, ConversionFault> {
    use ConversionFault::*;
    if from.integer() && to.integer() {
        let number = if from.signed() {
            word as i64 as i128
        } else {
            word as i128
        };
        let modulus = 1i128 << to.bits();
        let min = if to.signed() { -modulus / 2 } else { 0 };
        let max = if to.signed() {
            modulus / 2 - 1
        } else {
            modulus - 1
        };
        if checked && (number < min || number > max) {
            return Err(Overflow);
        }
        let low = number.rem_euclid(modulus);
        return Ok(if to.signed() && low > max {
            (low - modulus) as u64
        } else {
            low as u64
        });
    }
    if from.integer() {
        let n = if from.signed() {
            word as i64 as i128
        } else {
            word as i128
        };
        return Ok(pack(to, n < 0, n.unsigned_abs(), 0));
    }
    let (sign, magnitude, exponent, special, is_nan) = parts(from, word);
    if !to.integer() {
        return Ok(if is_nan {
            nan(to)
        } else if special {
            infinity(to, sign)
        } else {
            pack(to, sign, magnitude, exponent)
        });
    }
    if special {
        return Err(Conversion);
    }
    let magnitude = if exponent >= 0 {
        if exponent >= 128 || magnitude > (u128::MAX >> exponent) {
            return Err(Conversion);
        }
        magnitude << exponent
    } else if exponent <= -128 {
        0
    } else {
        magnitude >> (-exponent)
    };
    let limit = if to.signed() {
        if sign {
            1u128 << (to.bits() - 1)
        } else {
            (1u128 << (to.bits() - 1)) - 1
        }
    } else {
        (1u128 << to.bits()) - 1
    };
    if magnitude > limit || (!to.signed() && sign && magnitude != 0) {
        return Err(Conversion);
    }
    Ok(if sign {
        (-(magnitude as i128)) as u64
    } else {
        magnitude as u64
    })
}

fn sum(ty: ValueType, a: (bool, u128, i32), b: (bool, u128, i32)) -> u64 {
    let low = a.2.min(b.2);
    let expand = |m: u128, e: i32| {
        let mut words = [0u64; 34];
        let shift = (e - low) as usize;
        for bit in 0..128 {
            if m >> bit & 1 != 0 {
                let position = shift + bit;
                words[position / 64] |= 1u64 << (position % 64);
            }
        }
        words
    };
    let mut x = expand(a.1, a.2);
    let mut y = expand(b.1, b.2);
    let order = x.iter().rev().cmp(y.iter().rev());
    let sign = if a.0 == b.0 || order.is_ge() {
        a.0
    } else {
        b.0
    };
    if a.0 != b.0 && order.is_lt() {
        std::mem::swap(&mut x, &mut y);
    }
    let mut carry = 0u128;
    for i in 0..34 {
        if a.0 == b.0 {
            let v = x[i] as u128 + y[i] as u128 + carry;
            x[i] = v as u64;
            carry = v >> 64;
        } else {
            let sub = y[i] as u128 + carry;
            let old = x[i] as u128;
            x[i] = old.wrapping_sub(sub) as u64;
            carry = u128::from(old < sub);
        }
    }
    let Some(high) = x.iter().rposition(|&v| v != 0) else {
        return zero(ty, a.0 && b.0);
    };
    let bits = high * 64 + 64 - x[high].leading_zeros() as usize;
    let drop = bits.saturating_sub(120);
    let mut magnitude = 0u128;
    let mut sticky = false;
    for (i, &word) in x.iter().enumerate() {
        for bit in 0..64 {
            if word >> bit & 1 != 0 {
                let position = i * 64 + bit;
                if position < drop {
                    sticky = true
                } else {
                    magnitude |= 1u128 << (position - drop);
                }
            }
        }
    }
    if sticky {
        magnitude |= 1;
    }
    pack(ty, sign, magnitude, low + drop as i32)
}

pub fn float(op: FloatOp, ty: ValueType, x: u64, y: u64) -> u64 {
    let (sa, ma, ea, ia, na) = parts(ty, x);
    let (sb, mb, eb, ib, nb) = parts(ty, y);
    if op == FloatOp::Neg {
        let (f, _, e) = format(ty);
        return x ^ (1u64 << (f + e));
    }
    if na || nb {
        return nan(ty);
    }
    match op {
        FloatOp::Add | FloatOp::Sub => {
            let sb = sb ^ (op == FloatOp::Sub);
            if ia || ib {
                return if ia && ib && sa != sb {
                    nan(ty)
                } else {
                    infinity(ty, if ia { sa } else { sb })
                };
            }
            sum(ty, (sa, ma, ea), (sb, mb, eb))
        }
        FloatOp::Mul => {
            if (ia && mb == 0) || (ib && ma == 0) {
                return nan(ty);
            }
            if ia || ib {
                return infinity(ty, sa ^ sb);
            }
            pack(ty, sa ^ sb, ma * mb, ea + eb)
        }
        FloatOp::Div => {
            if (ia && ib) || (ma == 0 && mb == 0) {
                return nan(ty);
            }
            if ia || mb == 0 {
                return infinity(ty, sa ^ sb);
            }
            if ib || ma == 0 {
                return zero(ty, sa ^ sb);
            }
            let (f, bias, _) = format(ty);
            let delta = (128 - ma.leading_zeros()) as i32 - (128 - mb.leading_zeros()) as i32;
            let below = if delta >= 0 {
                ma < (mb << delta)
            } else {
                (ma << (-delta)) < mb
            };
            let top = ea - eb + delta - i32::from(below);
            if top < 1 - bias - f as i32 - 1 {
                return zero(ty, sa ^ sb);
            }
            let unit = top.max(1 - bias) - f as i32;
            let shift = ea - eb - unit;
            let (n, d) = if shift >= 0 {
                (ma << shift, mb)
            } else {
                (ma, mb << (-shift))
            };
            let q = n / d;
            let rem = n % d;
            let q = q + u128::from(rem > d - rem || (rem == d - rem && q & 1 != 0));
            pack(ty, sa ^ sb, q, unit)
        }
        FloatOp::Rem => {
            if ia || mb == 0 {
                return nan(ty);
            }
            if ib {
                return x;
            }
            // Exact long division on significands with modular reduction;
            // keep quotient parity and compare twice the remainder to divisor.
            if ea < eb {
                let gap = eb - ea;
                if gap >= 128 || mb > u128::MAX >> gap {
                    return x;
                }
                let divisor = mb << gap;
                let q = ma / divisor;
                let rem = ma % divisor;
                if rem > divisor - rem || (rem == divisor - rem && q & 1 != 0) {
                    pack(ty, !sa, divisor - rem, ea)
                } else {
                    pack(ty, sa, rem, ea)
                }
            } else {
                let mut rem = ma % mb;
                let mut odd = (ma / mb) & 1 != 0;
                for _ in 0..ea - eb {
                    let twice = rem * 2;
                    odd = twice >= mb;
                    rem = twice % mb;
                }
                if rem > mb - rem || (rem == mb - rem && odd) {
                    pack(ty, !sa, mb - rem, eb)
                } else {
                    pack(ty, sa, rem, eb)
                }
            }
        }
        FloatOp::Neg => unreachable!(),
    }
}
