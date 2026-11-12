use crate::{FaultKind, Host, Limits, VmScratch};
use eitri_base::{IndexVec, Span};
use eitri_bytecode::spec::{FloatConversion, FloatOp, Precision, Sem};
use eitri_bytecode::{
    Assembler, Callees, Function, FunctionId, Op, Operands, Signature, ValueType as V, oracle,
    verify,
};

struct NoHost;
impl Host for NoHost {
    fn call(&mut self, _: &mut crate::Call<'_, '_>, _: u32) -> Result<(), crate::HostError> {
        panic!("no host call in scalar bytecode")
    }
}
fn function(op: Op, params: &[V], result: V, c: i64) -> IndexVec<FunctionId, Function> {
    let mut asm = Assembler::new();
    asm.emit(op, Operands::abc(0, 1, c), Span::new(7, 11))
        .unwrap();
    asm.ret(Span::new(12, 15)).unwrap();
    let f = asm
        .finish(
            "scalar",
            Signature {
                params: params.to_vec(),
                results: vec![result],
            },
        )
        .unwrap();
    let functions = IndexVec::from_raw(vec![f]);
    let signatures = IndexVec::from_raw(functions.iter().map(|f| f.signature.clone()).collect());
    verify(
        &functions[FunctionId::from_u32(0)],
        &Callees {
            functions: &signatures,
            hosts: &[],
        },
    )
    .unwrap();
    functions
}
fn run(functions: &IndexVec<FunctionId, Function>, args: &[u64]) -> Result<u64, FaultKind> {
    let mut scratch = VmScratch::new();
    let entry = FunctionId::from_u32(0);
    let a = crate::run(
        functions,
        entry,
        args,
        &mut NoHost,
        Limits::default(),
        &mut scratch,
    );
    let b = crate::run_threaded(
        functions,
        entry,
        args,
        &mut NoHost,
        Limits::default(),
        &mut scratch,
    );
    assert_eq!(a, b, "the two VMs disagree (including fault PCs)");
    a.map(|v| v[0]).map_err(|e| e.kind)
}
fn equal(ty: V, a: u64, b: u64) -> bool {
    a == b
        || match ty {
            V::F32 => f32::from_bits(a as u32).is_nan() && f32::from_bits(b as u32).is_nan(),
            V::F64 => f64::from_bits(a).is_nan() && f64::from_bits(b).is_nan(),
            _ => false,
        }
}

#[test]
fn float_opcodes_execute_and_verify_at_both_precisions() {
    for &opcode in Op::ALL {
        let info = opcode.info();
        match info.sem {
            Sem::Float { op, precision } => {
                let ty = if precision == Precision::F32 {
                    V::F32
                } else {
                    V::F64
                };
                let params = if op == FloatOp::Neg {
                    vec![ty]
                } else {
                    vec![ty, ty]
                };
                let functions =
                    function(opcode, &params, ty, if op == FloatOp::Neg { 0 } else { 2 });
                let values = if ty == V::F32 {
                    vec![
                        0, 0x80000000, 1, 0x3fc00000, 0x7f7fffff, 0x7f800000, 0xff800000,
                        0x7fc00000,
                    ]
                } else {
                    vec![
                        0,
                        1u64 << 63,
                        1,
                        1.5f64.to_bits(),
                        f64::MAX.to_bits(),
                        f64::INFINITY.to_bits(),
                        f64::NEG_INFINITY.to_bits(),
                        f64::NAN.to_bits(),
                    ]
                };
                for &a in &values {
                    for &b in &values {
                        let args = if op == FloatOp::Neg {
                            vec![a]
                        } else {
                            vec![a, b]
                        };
                        let actual = run(&functions, &args).unwrap();
                        let expected = oracle::scalar::float(op, ty, a, b);
                        assert!(
                            equal(ty, actual, expected),
                            "{} {a:016x} {b:016x}",
                            info.name
                        );
                    }
                }
            }
            Sem::FloatSet { cond, precision } => {
                let ty = if precision == Precision::F32 {
                    V::F32
                } else {
                    V::F64
                };
                let functions = function(opcode, &[ty, ty], V::Bool, 2);
                for a in [
                    f64::NAN,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    0.0,
                    -0.0,
                    1.0,
                    -1.0,
                ] {
                    for b in [f64::NAN, 0.0, -0.0, 1.0, f64::INFINITY] {
                        let args = if ty == V::F32 {
                            vec![(a as f32).to_bits() as u64, (b as f32).to_bits() as u64]
                        } else {
                            vec![a.to_bits(), b.to_bits()]
                        };
                        assert_eq!(
                            run(&functions, &args),
                            Ok(u64::from(oracle::float_cond(cond, a, b)))
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

#[test]
fn conversion_opcodes_match_ieee_reference_and_report_faults() {
    let types = [V::I8, V::U8, V::I64, V::U64, V::F32, V::F64];
    for from in types {
        for to in types {
            if from == to {
                continue;
            }
            let format = eitri_bytecode::scalar::integer_format;
            let (op, c) = if from.integer() && to.integer() {
                (
                    Op::CvtChk,
                    format(to.bits(), to.signed()) | u8::from(from.signed()) << 3,
                )
            } else if from.integer() {
                (
                    if to == V::F32 {
                        Op::FcvtI2f32
                    } else {
                        Op::FcvtI2f64
                    },
                    format(from.bits(), from.signed()),
                )
            } else if to.integer() {
                (
                    if from == V::F32 {
                        Op::FcvtF32i
                    } else {
                        Op::FcvtF64i
                    },
                    format(to.bits(), to.signed()),
                )
            } else {
                (
                    if to == V::F32 {
                        Op::FcvtF64f32
                    } else {
                        Op::FcvtF32f64
                    },
                    0,
                )
            };
            let functions = function(op, &[from], to, c.into());
            let values = match from {
                V::I8 => vec![i8::MIN as i64 as u64, 0, 1, 127],
                V::U8 => vec![0, 1, 127, 128, 255],
                V::I64 => vec![i64::MIN as u64, (-1i64) as u64, 0, 1, i64::MAX as u64],
                V::U64 => vec![0, 1, 127, 128, 255, 256, u64::MAX],
                V::F32 => [0.0, -0.0, -0.5, 127.9, f32::MAX, f32::INFINITY, f32::NAN]
                    .map(|x| x.to_bits() as u64)
                    .to_vec(),
                V::F64 => [
                    0.0,
                    -0.0,
                    -0.5,
                    127.9,
                    2.0f64.powi(63),
                    2.0f64.powi(64),
                    f64::MAX,
                    f64::INFINITY,
                    f64::NAN,
                ]
                .map(f64::to_bits)
                .to_vec(),
                _ => unreachable!(),
            };
            for word in values {
                let actual = run(&functions, &[word]);
                let expected = oracle::scalar::convert(word, from, to, true).map_err(|e| match e {
                    oracle::scalar::ConversionFault::Overflow => FaultKind::Overflow,
                    oracle::scalar::ConversionFault::Conversion => FaultKind::Conversion,
                });
                match (&actual, &expected) {
                    (Ok(a), Ok(b)) => assert!(equal(to, *a, *b), "{from:?} {word:x} -> {to:?}"),
                    _ => assert_eq!(actual, expected, "{from:?} {word:x} -> {to:?}"),
                }
            }
        }
    }
}

#[test]
fn noncanonical_literals_and_conversion_descriptors_are_rejected() {
    for (ty, word) in [
        (V::I8, 128),
        (V::U8, 256),
        (V::Char, 0xd800),
        (V::Char, 0x110000),
        (V::F32, 1u64 << 32),
    ] {
        let mut asm = Assembler::new();
        asm.load(0, word, ty, Span::default()).unwrap();
        asm.ret(Span::default()).unwrap();
        let f = asm
            .finish(
                "bad",
                Signature {
                    params: vec![],
                    results: vec![ty],
                },
            )
            .unwrap();
        let signatures = IndexVec::from_raw(vec![f.signature.clone()]);
        assert!(
            verify(
                &f,
                &Callees {
                    functions: &signatures,
                    hosts: &[]
                }
            )
            .is_err()
        );
    }
    for op in [
        Op::CvtChk,
        Op::CvtWrap,
        Op::FcvtF64i,
        Op::FcvtI2f32,
        Op::FcvtF64f32,
    ] {
        for descriptor in [2, 16, 127, 255] {
            let from = if matches!(
                op.info().sem,
                Sem::FloatConvert(FloatConversion::F64ToInt | FloatConversion::F64ToF32)
            ) {
                V::F64
            } else {
                V::I64
            };
            let mut asm = Assembler::new();
            asm.emit(op, Operands::abc(0, 1, descriptor), Span::default())
                .unwrap();
            asm.ret(Span::default()).unwrap();
            let f = asm
                .finish(
                    "bad",
                    Signature {
                        params: vec![from],
                        results: vec![V::I64],
                    },
                )
                .unwrap();
            let signatures = IndexVec::from_raw(vec![f.signature.clone()]);
            assert!(
                verify(
                    &f,
                    &Callees {
                        functions: &signatures,
                        hosts: &[]
                    }
                )
                .is_err()
            );
        }
    }
}
