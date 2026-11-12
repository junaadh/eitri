use crate::spec::{FloatCond, FloatOp, IntOp, Mode, Precision, Sem, Width};
use crate::{ValueType as V, oracle, scalar};

fn fault(value: oracle::Fault) -> scalar::ScalarFault {
    match value {
        oracle::Fault::Overflow => scalar::ScalarFault::Overflow,
        oracle::Fault::DivisionByZero => scalar::ScalarFault::DivisionByZero,
        oracle::Fault::ShiftAmount => scalar::ScalarFault::ShiftAmount,
    }
}

#[test]
fn narrow_arithmetic_exhaustively_matches_mathematical_models() {
    use oracle::Arith as A;
    for ty in [V::I8, V::U8] {
        let model = oracle::IntType {
            bits: 8,
            signed: ty.signed(),
        };
        for a in model.min()..=model.max() {
            for b in model.min()..=model.max() {
                for (operation, int, mode) in [
                    (A::Add, IntOp::Add, Mode::Wrap),
                    (A::Sub, IntOp::Sub, Mode::Wrap),
                    (A::Mul, IntOp::Mul, Mode::Wrap),
                    (A::WrapAdd, IntOp::Add, Mode::Wrap),
                    (A::WrapSub, IntOp::Sub, Mode::Wrap),
                    (A::WrapMul, IntOp::Mul, Mode::Wrap),
                    (
                        A::Div,
                        IntOp::Div,
                        if ty.signed() {
                            Mode::Signed
                        } else {
                            Mode::Unsigned
                        },
                    ),
                    (
                        A::Rem,
                        IntOp::Rem,
                        if ty.signed() {
                            Mode::Signed
                        } else {
                            Mode::Unsigned
                        },
                    ),
                ] {
                    let checked = !matches!(operation, A::WrapAdd | A::WrapSub | A::WrapMul);
                    let from = if matches!(operation, A::Div | A::Rem) && !ty.signed() {
                        V::U64
                    } else {
                        V::I64
                    };
                    let actual = scalar::eval(
                        Sem::Int {
                            op: int,
                            mode,
                            imm: false,
                        },
                        a as u64,
                        b as u64,
                        0,
                    )
                    .and_then(|word| scalar::convert(word, from, ty, checked));
                    let expected = oracle::arith(model, operation, a, b)
                        .map(|n| n as u64)
                        .map_err(fault);
                    assert_eq!(actual, expected, "{ty:?} {a} {operation:?} {b}");
                }
            }
        }
        for a in model.min()..=model.max() {
            for count in [-1, 0, 1, 7, 8, 63, 64, i64::MAX as i128, u64::MAX as i128] {
                for (operation, op) in [
                    (A::Shl, IntOp::Shl(Width::W8)),
                    (A::Shr, IntOp::Shr(Width::W8)),
                ] {
                    let mode = if ty.signed() {
                        Mode::Signed
                    } else {
                        Mode::Unsigned
                    };
                    assert_eq!(
                        scalar::eval(
                            Sem::Int {
                                op,
                                mode,
                                imm: false
                            },
                            a as u64,
                            count as u64,
                            0
                        ),
                        oracle::arith(model, operation, a, count)
                            .map(|n| n as u64)
                            .map_err(fault)
                    );
                }
            }
        }
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}
fn canonical(ty: V, word: u64) -> u64 {
    match ty {
        V::I8 => word as i8 as i64 as u64,
        V::U8 => word & 255,
        V::F32 => word & 0xffff_ffff,
        _ => word,
    }
}
fn equal_float(ty: V, a: u64, b: u64) -> bool {
    if ty == V::F32 {
        a == b || (f32::from_bits(a as u32).is_nan() && f32::from_bits(b as u32).is_nan())
    } else {
        a == b || (f64::from_bits(a).is_nan() && f64::from_bits(b).is_nan())
    }
}

#[test]
fn conversions_match_independent_ieee_field_models() {
    let types = [V::I8, V::U8, V::I64, V::U64, V::F32, V::F64];
    let words = [
        0,
        1,
        127,
        128,
        255,
        256,
        u64::MAX,
        i64::MIN as u64,
        0x8000_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0000,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff8_0000_0000_0000,
        0x43e0_0000_0000_0000,
        0x43f0_0000_0000_0000,
        127.9f64.to_bits(),
        (-0.5f64).to_bits(),
        f64::MIN_POSITIVE.to_bits(),
    ];
    let mut rng = Rng(0xd1b54a32d192ed03);
    for from in types {
        for to in types {
            for raw in words.into_iter().chain((0..2000).map(|_| rng.next())) {
                let word = canonical(from, raw);
                for checked in [true, false] {
                    if !checked && (!from.integer() || !to.integer()) {
                        continue;
                    }
                    let actual = scalar::convert(word, from, to, checked);
                    let expected = oracle::scalar::convert(word, from, to, checked).map_err(
                        |error| match error {
                            oracle::scalar::ConversionFault::Overflow => {
                                scalar::ScalarFault::Overflow
                            }
                            oracle::scalar::ConversionFault::Conversion => {
                                scalar::ScalarFault::Conversion
                            }
                        },
                    );
                    match (actual, expected) {
                        (Ok(a), Ok(b)) if !to.integer() => assert!(
                            equal_float(to, a, b),
                            "{from:?} {word:016x} -> {to:?}: {a:016x} != {b:016x}"
                        ),
                        _ => assert_eq!(
                            actual, expected,
                            "{from:?} {word:016x} -> {to:?}, checked={checked}"
                        ),
                    }
                }
            }
        }
    }
}

#[test]
fn float_arithmetic_matches_software_ieee_reference() {
    let mut rng = Rng(0xa0761d6478bd642f);
    for ty in [V::F32, V::F64] {
        let special = if ty == V::F32 {
            vec![
                0, 0x80000000, 1, 0x007fffff, 0x00800000, 0x3f000000, 0x3f800000, 0x3fc00000,
                0x7f7fffff, 0x7f800000, 0xff800000, 0x7fc00000,
            ]
        } else {
            vec![
                0,
                1u64 << 63,
                1,
                0x000fffffffffffff,
                0x0010000000000000,
                0x3fe0000000000000,
                0x3ff0000000000000,
                0x3ff8000000000000,
                0x7fefffffffffffff,
                0x7ff0000000000000,
                0xfff0000000000000,
                0x7ff8000000000000,
            ]
        };
        let pairs = special
            .iter()
            .flat_map(|&a| special.iter().map(move |&b| (a, b)))
            .chain((0..20_000).map(|_| (canonical(ty, rng.next()), canonical(ty, rng.next()))));
        for (a, b) in pairs {
            for op in [
                FloatOp::Add,
                FloatOp::Sub,
                FloatOp::Mul,
                FloatOp::Div,
                FloatOp::Rem,
                FloatOp::Neg,
            ] {
                let precision = if ty == V::F32 {
                    Precision::F32
                } else {
                    Precision::F64
                };
                let actual = scalar::eval(Sem::Float { op, precision }, a, b, 0).unwrap();
                let expected = oracle::scalar::float(op, ty, a, b);
                assert!(
                    equal_float(ty, actual, expected),
                    "{ty:?} {op:?} {a:016x} {b:016x}: {actual:016x} != {expected:016x}"
                );
            }
        }
    }
}

#[test]
fn every_float_condition_matches_partial_order_reference() {
    let values = [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        0.0,
        -0.0,
        1.0,
        -1.0,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f32::MIN_POSITIVE as f64,
        f32::from_bits(1) as f64,
    ];
    for precision in [Precision::F32, Precision::F64] {
        for a in values {
            for b in values {
                for cond in [
                    FloatCond::Eq,
                    FloatCond::Ne,
                    FloatCond::Lt,
                    FloatCond::Le,
                    FloatCond::Ult,
                    FloatCond::Ule,
                ] {
                    let (a, b) = if precision == Precision::F32 {
                        (a as f32 as f64, b as f32 as f64)
                    } else {
                        (a, b)
                    };
                    let (x, y) = if precision == Precision::F32 {
                        ((a as f32).to_bits() as u64, (b as f32).to_bits() as u64)
                    } else {
                        (a.to_bits(), b.to_bits())
                    };
                    assert_eq!(
                        scalar::eval(Sem::FloatSet { precision, cond }, x, y, 0).unwrap(),
                        u64::from(oracle::float_cond(cond, a, b))
                    );
                }
            }
        }
    }
}

#[test]
fn full_width_arithmetic_matches_exact_integer_models() {
    use oracle::Arith as A;
    let mut rng = Rng(0xe7037ed1a0b428db);
    for ty in [V::I64, V::U64] {
        let model = oracle::IntType {
            bits: 64,
            signed: ty.signed(),
        };
        for _ in 0..20_000 {
            let (a, b) = (rng.next(), rng.next());
            let numbers = if ty.signed() {
                (a as i64 as i128, b as i64 as i128)
            } else {
                (a as i128, b as i128)
            };
            for (operation, op, wrapping) in [
                (A::Add, IntOp::Add, false),
                (A::Sub, IntOp::Sub, false),
                (A::Mul, IntOp::Mul, false),
                (A::WrapAdd, IntOp::Add, true),
                (A::WrapSub, IntOp::Sub, true),
                (A::WrapMul, IntOp::Mul, true),
                (A::Div, IntOp::Div, false),
                (A::Rem, IntOp::Rem, false),
            ] {
                let mode = if wrapping {
                    Mode::Wrap
                } else if matches!(op, IntOp::Div | IntOp::Rem) {
                    if ty.signed() {
                        Mode::Signed
                    } else {
                        Mode::Unsigned
                    }
                } else if ty.signed() {
                    Mode::CheckedSigned
                } else {
                    Mode::CheckedUnsigned
                };
                assert_eq!(
                    scalar::eval(
                        Sem::Int {
                            op,
                            mode,
                            imm: false
                        },
                        a,
                        b,
                        0
                    ),
                    oracle::arith(model, operation, numbers.0, numbers.1)
                        .map(|n| n as u64)
                        .map_err(fault),
                    "{ty:?} {operation:?} {a:x} {b:x}"
                );
            }
        }
    }
}
