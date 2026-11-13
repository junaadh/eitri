use crate::spec::*;
use crate::{DecodeError, EncodeError, Operands, decode, disassemble, encode};

#[test]
fn inventory_matches_the_manifest() {
    assert_eq!(Op::ALL.len(), 184, "the documented inventory");
    // Numbering is the table order, so it is deterministic.
    for (index, &op) in Op::ALL.iter().enumerate() {
        assert_eq!(op as usize, index);
        assert_eq!(Op::from_byte(index as u8), Some(op));
    }
    assert_eq!(Op::from_byte(184), None);
    let mut names: Vec<&str> = OPS.iter().map(|info| info.name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), OPS.len(), "mnemonics are unique");

    // Per-family counts agree with the opcode manifest table.
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/implementation/bytecode.md"
    ))
    .unwrap();
    let table = doc.split("### Opcode manifest").nth(1).unwrap();
    let mut rows = 0;
    for line in table.lines().skip_while(|l| !l.starts_with("| `")) {
        let Some(rest) = line.strip_prefix("| `") else {
            break;
        };
        let mut cells = rest.split(" | ");
        let families: Vec<&str> = cells
            .next()
            .unwrap()
            .split(", ")
            .map(|f| f.trim_matches('`'))
            .collect();
        let documented: usize = cells.next().unwrap().trim().parse().unwrap();
        let actual = OPS
            .iter()
            .filter(|info| families.contains(&info.family))
            .count();
        assert_eq!(actual, documented, "families {families:?}");
        rows += 1;
    }
    assert_eq!(rows, 19, "every manifest row was checked");
}

#[test]
fn scalar_extensions_preserve_m0_and_reject_managed_neighbors() {
    let implemented: Vec<&str> = OPS
        .iter()
        .filter(|info| info.implemented)
        .map(|info| info.name)
        .collect();
    for name in [
        "MOV",
        "LDI",
        "LDK",
        "ADD.WRAP",
        "ADD.CHK.S",
        "SUB.WRAP",
        "SUB.CHK.S",
        "MUL.WRAP",
        "MUL.CHK.S",
        "DIV.S",
        "REM.S",
        "XOR",
        "ADDI.WRAP",
        "ADDI.CHK.S",
        "SUBI.WRAP",
        "SUBI.CHK.S",
        "MULI.CHK.S",
        "NEG.CHK.S",
        "CB.EQ",
        "CB.NE",
        "CB.LT",
        "CB.LE",
        "CBI.EQ",
        "CBI.NE",
        "CBI.LT",
        "CBI.GT",
        "TB.TRUE",
        "TB.FALSE",
        "CSET.EQ",
        "CSET.NE",
        "CSET.LT",
        "CSET.LE",
        "J",
        "J_X",
        "LDK_X",
        "CALL.SCRIPT",
        "CALL.HOST",
        "RET",
        "WIDE",
    ] {
        assert!(implemented.contains(&name), "missing M0 operation {name}");
    }
    assert!(
        Op::CvtChk.info().implemented
            && Op::FaddF32.info().implemented
            && Op::FcsetFuleF64.info().implemented
    );
    assert!(!Op::ShlS16.info().implemented && !Op::LdfU1.info().implemented);
}

#[test]
fn effects_are_consistent_with_semantics() {
    for info in OPS {
        let e = info.effects;
        let control = matches!(
            info.sem,
            Sem::Branch { .. }
                | Sem::FloatBranch { .. }
                | Sem::TestBranch(_)
                | Sem::Jump
                | Sem::Loop { .. }
        );
        assert_eq!(e.contains(BRANCH), control, "{}", info.name);
        // Every taken backward branch and every call polls the budget.
        assert_eq!(
            e.contains(POLL),
            control || e.contains(CALL),
            "{}",
            info.name
        );
        if let Sem::Int { mode, op, imm } = info.sem {
            let checks = matches!(mode, Mode::CheckedSigned | Mode::CheckedUnsigned)
                || matches!(op, IntOp::Div | IntOp::Rem)
                || (matches!(op, IntOp::Shl(_) | IntOp::Shr(_)) && !imm);
            assert_eq!(e.contains(FAULT), checks, "{}", info.name);
        }
        assert_eq!(
            e.contains(CONST),
            matches!(info.sem, Sem::LoadConst),
            "{}",
            info.name
        );
    }
}

#[test]
fn every_condition_has_an_exact_complement() {
    let ints = [
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
    for cond in [Cond::Eq, Cond::Ne, Cond::Lt, Cond::Le, Cond::Lo, Cond::Ls] {
        let (complement, swap) = cond.complement().unwrap();
        for &a in &ints {
            for &b in &ints {
                let (x, y) = if swap { (b, a) } else { (a, b) };
                assert_eq!(!cond.eval(a, b), complement.eval(x, y), "{cond:?} {a} {b}");
            }
        }
    }
    let floats = [
        f64::NAN,
        f64::NEG_INFINITY,
        -1.5,
        -0.0,
        0.0,
        1.5,
        f64::INFINITY,
        f64::MIN_POSITIVE,
    ];
    let conds = [
        FloatCond::Eq,
        FloatCond::Ne,
        FloatCond::Lt,
        FloatCond::Le,
        FloatCond::Ult,
        FloatCond::Ule,
    ];
    for cond in conds {
        let (complement, swap) = cond.complement();
        assert!(conds.contains(&complement), "the complement is in the set");
        for &a in &floats {
            for &b in &floats {
                let (x, y) = if swap { (b, a) } else { (a, b) };
                assert_eq!(!cond.eval(a, b), complement.eval(x, y), "{cond:?} {a} {b}");
                // f32 precision gives the same truth table.
                let (a32, b32) = (a as f32 as f64, b as f32 as f64);
                let (x32, y32) = if swap { (b32, a32) } else { (a32, b32) };
                assert_eq!(!cond.eval(a32, b32), complement.eval(x32, y32));
            }
        }
    }
    // NaN: the ordered conditions are false, the unordered ones true.
    assert!(!FloatCond::Lt.eval(f64::NAN, 1.0) && !FloatCond::Le.eval(1.0, f64::NAN));
    assert!(FloatCond::Ult.eval(f64::NAN, 1.0) && FloatCond::Ne.eval(f64::NAN, f64::NAN));
    // -0.0 equals 0.0.
    assert!(FloatCond::Eq.eval(-0.0, 0.0));
    for test in [Test::True, Test::False, Test::None, Test::Some] {
        assert_eq!(test.complement().complement(), test);
        assert_ne!(test.complement(), test);
    }
    // Every condition the register-form families use has its complement
    // in the same family.
    for info in OPS {
        let in_family = |name: String| OPS.iter().any(|o| o.name == name);
        match info.sem {
            Sem::Branch { cond, imm: false } | Sem::Set(cond) => {
                let (complement, _) = cond.complement().unwrap();
                let name = info.name.replace(cond_name(cond), cond_name(complement));
                assert!(in_family(name), "{}", info.name);
            }
            _ => {}
        }
    }
}

fn cond_name(cond: Cond) -> &'static str {
    match cond {
        Cond::Eq => "EQ",
        Cond::Ne => "NE",
        Cond::Lt => "LT",
        Cond::Le => "LE",
        Cond::Lo => "LO",
        Cond::Ls => "LS",
        Cond::Gt => "GT",
        Cond::Hi => "HI",
    }
}

/// Boundary values for a role.
fn samples(role: Role) -> Vec<i64> {
    let (min, max) = role.range();
    let mut values = vec![min, max, (min + max) / 2];
    if role.is_widened() {
        values.extend([255, 256, 65_535]);
    }
    values
}

#[test]
fn every_opcode_round_trips_at_its_boundaries() {
    for &op in Op::ALL {
        if op == Op::Wide {
            continue;
        }
        let info = op.info();
        let roles = info.roles;
        let ext = info.format.ext_role();
        for &a in &samples(roles[0]) {
            for &b in &samples(roles[1]) {
                for &c in &samples(roles[2]) {
                    for &e in &samples(ext) {
                        let operands = Operands { a, b, c, ext: e };
                        let wide = [a, b, c]
                            .iter()
                            .zip(roles)
                            .any(|(&v, r)| r.is_widened() && v > 255);
                        let mut code = Vec::new();
                        let result = encode(op, operands, &mut code);
                        if wide && !op.wide_legal() {
                            assert!(matches!(result, Err(EncodeError::NotWidenable { .. })));
                            continue;
                        }
                        result.unwrap_or_else(|e| panic!("{} {operands:?}: {e:?}", info.name));
                        let decoded = decode(&code, 0).unwrap();
                        assert_eq!(decoded.op, op);
                        assert_eq!(decoded.operands, operands, "{}", info.name);
                        assert_eq!(decoded.wide, wide);
                        assert_eq!(decoded.words as usize, code.len());
                        assert_eq!(decoded.words, info.format.words() + u32::from(wide));
                    }
                }
            }
        }
    }
}

#[test]
fn out_of_range_operands_are_rejected() {
    let error = |op, operands| {
        let mut code = Vec::new();
        encode(op, operands, &mut code).unwrap_err()
    };
    assert!(matches!(
        error(Op::CbEq, Operands::abc(1, 2, 128)),
        EncodeError::OutOfRange {
            role: Role::Disp8,
            ..
        }
    ));
    assert!(matches!(
        error(Op::Ldi, Operands::abc(1, 40_000, 0)),
        EncodeError::OutOfRange {
            role: Role::Imm16,
            ..
        }
    ));
    assert!(matches!(
        error(Op::Mov, Operands::abc(65_536, 0, 0)),
        EncodeError::OutOfRange {
            role: Role::Dst,
            ..
        }
    ));
    assert!(matches!(
        error(Op::Mov, Operands::abc(1, 2, 3)),
        EncodeError::OutOfRange {
            role: Role::None,
            ..
        }
    ));
    assert!(matches!(
        error(Op::J, Operands::abc(1 << 23, 0, 0)),
        EncodeError::OutOfRange {
            role: Role::Disp24,
            ..
        }
    ));
}

#[test]
fn wide_legality_table() {
    let illegal: Vec<&str> = Op::ALL
        .iter()
        .filter(|op| !op.wide_legal())
        .map(|op| op.name())
        .collect();
    assert_eq!(
        illegal,
        [
            "J",
            "J_X",
            "CALL.SCRIPT",
            "CALL.HOST",
            "CALL.VALUE",
            "RET",
            "WIDE"
        ]
    );
    // Every other opcode names at least one slot.
    assert!(
        Op::ALL
            .iter()
            .filter(|op| op.wide_legal())
            .all(|op| { op.info().roles.iter().any(|r| r.is_slot()) })
    );
}

#[test]
fn malformed_code_is_rejected() {
    let wide = Op::Wide as u32 | 1 << 8;
    // WIDE before an instruction that cannot take it.
    assert_eq!(
        decode(&[wide, Op::J as u32], 0),
        Err(DecodeError::IllegalWide)
    );
    assert_eq!(
        decode(&[wide, Op::Wide as u32], 0),
        Err(DecodeError::IllegalWide)
    );
    // A high byte for a non-slot field: CB's C is a displacement.
    let wide_c = Op::Wide as u32 | 1 << 24;
    assert_eq!(
        decode(&[wide_c, Op::CbEq as u32], 0),
        Err(DecodeError::IllegalWide)
    );
    // WIDE as the last word, and a two-word form cut short.
    assert_eq!(decode(&[wide], 0), Err(DecodeError::Truncated));
    assert_eq!(decode(&[Op::LdkX as u32], 0), Err(DecodeError::Truncated));
    // Reserved bits.
    assert_eq!(
        decode(&[Op::Ret as u32 | 1 << 8], 0),
        Err(DecodeError::Reserved)
    );
    assert_eq!(
        decode(&[Op::Mov as u32 | 1 << 24], 0),
        Err(DecodeError::Reserved)
    );
    assert_eq!(
        decode(&[Op::JX as u32 | 1 << 31, 0], 0),
        Err(DecodeError::Reserved)
    );
    // Opcodes past the table.
    assert_eq!(decode(&[200], 0), Err(DecodeError::UnknownOpcode(200)));
}

#[test]
fn disassembly_shows_the_emitted_words() {
    let mut code = Vec::new();
    let mut emit = |op, a, b, c, ext| encode(op, Operands { a, b, c, ext }, &mut code).unwrap();
    emit(Op::Ldi, 0, -5, 0, 0);
    emit(Op::AddChkS, 1, 0, 0, 0);
    emit(Op::CbLt, 1, 0, -3, 0);
    emit(Op::Mov, 300, 1, 0, 0);
    emit(Op::LdkX, 2, 0, 0, 70_000);
    emit(Op::CallHost, 0, 7, 0, 0);
    emit(Op::JX, 0, 0, 0, -9);
    emit(Op::Ret, 0, 0, 0, 0);
    code.push(250);
    assert_eq!(
        disassemble(&code),
        "\
0000  LDI          r0, #-5
0001  ADD.CHK.S    r1, r0, r0
0002  CB.LT        r1, r0, -> 0000
0003  WIDE MOV     r300, r1
0005  LDK_X        r2, k70000
0007  CALL.HOST    site7
0008  J_X          -> 0001
0010  RET
0011  .word 0x000000fa  ; UnknownOpcode(250)
"
    );
}

/// The table-driven fast encoder (E5/L2) and the specification-driven one
/// agree on every opcode: same words for valid operands, same errors.
#[test]
fn fast_encoder_matches_the_general_one() {
    use crate::encoding::{Operands, encode, encode_general};
    let edges = [
        i64::MIN,
        -(1 << 24) - 1,
        -(1 << 23),
        -32_769,
        -32_768,
        -129,
        -128,
        -1,
        0,
        1,
        2,
        127,
        128,
        255,
        256,
        32_767,
        65_535,
        65_536,
        (1 << 23) - 1,
        1 << 24,
        u32::MAX as i64,
        i64::MAX,
    ];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut pick = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        edges[(seed % edges.len() as u64) as usize]
    };
    for (byte, info) in OPS.iter().enumerate() {
        let op = Op::from_byte(byte as u8).unwrap();
        for _ in 0..2_000 {
            let operands = Operands {
                a: pick(),
                b: pick(),
                c: pick(),
                ext: pick(),
            };
            let (mut fast, mut general) = (Vec::new(), Vec::new());
            let a = encode(op, operands, &mut fast);
            let b = encode_general(op, operands, &mut general);
            assert_eq!(a, b, "{} {operands:?}", info.name);
            assert_eq!(fast, general, "{} {operands:?}", info.name);
        }
    }
}
