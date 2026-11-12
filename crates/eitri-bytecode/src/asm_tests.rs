use eitri_base::{IndexVec, Span};

use crate::spec::{Cond, Test};
use crate::*;

const S: Span = Span::new(0, 0);
use ValueType::{Bool, I64};

fn sig(params: &[ValueType], results: &[ValueType]) -> Signature {
    Signature {
        params: params.to_vec(),
        results: results.to_vec(),
    }
}

/// Verify a function that calls only itself or nothing.
fn check(f: &Function) -> Result<(), VerifyError> {
    let functions: IndexVec<FunctionId, Signature> = IndexVec::from_raw(vec![f.signature.clone()]);
    let hosts = [sig(&[I64], &[]), sig(&[Bool], &[Bool])];
    verify(
        f,
        &Callees {
            functions: &functions,
            hosts: &hosts,
        },
    )
}

fn kind(f: &Function) -> ErrorKind {
    check(f).unwrap_err().kind
}

#[test]
fn the_worked_lowering() {
    // fn sum(a: i64, b: i64) -> i64 { return a + b }
    let mut asm = Assembler::new();
    asm.emit(Op::AddChkS, Operands::abc(0, 1, 2), S).unwrap();
    asm.ret(S).unwrap();
    let f = asm.finish("sum", sig(&[I64, I64], &[I64])).unwrap();
    assert_eq!(f.frame_size, 3);
    assert_eq!(
        disassemble(&f.code),
        "0000  ADD.CHK.S    r0, r1, r2\n0001  RET\n"
    );
    check(&f).unwrap();
}

/// A conditional branch over `filler` one-word instructions: forward when
/// `forward`, else a loop back over them. Returns the code.
fn branch_over(filler: usize, forward: bool) -> Function {
    let mut asm = Assembler::new();
    let target = asm.label();
    asm.load(1, 0, I64, S).unwrap();
    if !forward {
        asm.bind(target).unwrap();
    }
    if forward {
        asm.branch_if(Cond::Lt, 0, 1, target, S).unwrap();
    }
    for _ in 0..filler {
        asm.emit(Op::Mov, Operands::abc(2, 1, 0), S).unwrap();
    }
    if forward {
        asm.bind(target).unwrap();
    } else {
        asm.branch_if(Cond::Lt, 0, 1, target, S).unwrap();
    }
    asm.ret(S).unwrap();
    asm.finish("f", sig(&[I64], &[])).unwrap()
}

/// The opcode at word `pc`.
fn op_at(f: &Function, pc: usize) -> Op {
    decode(&f.code, pc).unwrap().op
}

#[test]
fn branches_are_short_exactly_when_they_fit() {
    // Forward: the displacement is the filler between branch and target.
    let f = branch_over(127, true);
    assert_eq!(op_at(&f, 1), Op::CbLt, "127 words: short");
    assert_eq!(f.code.len(), 1 + 1 + 127 + 1);
    check(&f).unwrap();
    let f = branch_over(128, true);
    // Long: the complement (LT a, b → LE b, a) skips a `J_X`.
    assert_eq!(op_at(&f, 1), Op::CbLe);
    assert_eq!(decode(&f.code, 1).unwrap().operands, Operands::abc(1, 0, 2));
    assert_eq!(op_at(&f, 2), Op::JX);
    assert_eq!(decode(&f.code, 2).unwrap().operands.ext, 128);
    check(&f).unwrap();

    // Backward: the displacement also covers the branch itself.
    let f = branch_over(127, false);
    let at = 1 + 127;
    assert_eq!(op_at(&f, at), Op::CbLt, "-128 fits");
    assert_eq!(decode(&f.code, at).unwrap().operands.c, -128);
    check(&f).unwrap();
    let f = branch_over(128, false);
    assert_eq!(op_at(&f, 1 + 128), Op::CbLe, "-129 does not");
    check(&f).unwrap();
}

#[test]
fn test_branches_and_jumps_lay_out() {
    let mut asm = Assembler::new();
    let top = asm.label();
    let end = asm.label();
    asm.bind(top).unwrap();
    asm.branch_test(Test::False, 0, end, S);
    for _ in 0..40_000 {
        asm.emit(Op::Mov, Operands::abc(1, 0, 0), S).unwrap();
    }
    asm.jump(top, S);
    asm.bind(end).unwrap();
    asm.ret(S).unwrap();
    let f = asm.finish("f", sig(&[Bool], &[])).unwrap();
    // 40,000 words do not fit TB's 16 bits: TB.TRUE skips a J_X.
    assert_eq!(op_at(&f, 0), Op::TbTrue);
    assert_eq!(op_at(&f, 1), Op::JX);
    // The backward jump fits J's 24 bits.
    assert_eq!(op_at(&f, 3 + 40_000), Op::J);
    check(&f).unwrap();
}

#[test]
fn wide_slots_and_extended_followers() {
    let mut asm = Assembler::new();
    asm.load(255, 1, I64, S).unwrap();
    asm.load(256, 2, I64, S).unwrap();
    asm.emit(Op::AddChkS, Operands::abc(300, 255, 256), S)
        .unwrap();
    // More than 65,536 constants: the last load is `LDK_X` behind `WIDE`.
    for i in 0..65_537u64 {
        asm.load(1, 1_000_000 + i, I64, S).unwrap();
    }
    asm.load(400, 1_000_000 + 65_536, I64, S).unwrap();
    asm.load(401, 5_000_000, I64, S).unwrap();
    let skip = asm.label();
    asm.branch_if(Cond::Eq, 300, 400, skip, S).unwrap();
    asm.bind(skip).unwrap();
    asm.ret(S).unwrap();
    let f = asm.finish("f", sig(&[], &[])).unwrap();
    assert_eq!(f.frame_size, 402);
    assert_eq!(f.consts.len(), 65_538, "repeated constants are pooled once");
    let listing = disassemble(&f.code);
    assert!(listing.starts_with("0000  LDI          r255, #1\n0001  WIDE LDI     r256, #2\n"));
    assert!(listing.contains("WIDE ADD.CHK.S r300, r255, r256"));
    assert!(listing.contains("LDK_X        r1, k65536\n"));
    assert!(
        listing.contains("WIDE LDK_X   r400, k65536\n"),
        "a widened extended follower"
    );
    assert!(listing.contains("WIDE CB.EQ   r300, r400"));
    check(&f).unwrap();
}

#[test]
fn label_misuse_is_reported() {
    let mut asm = Assembler::new();
    let label = asm.label();
    asm.jump(label, S);
    assert_eq!(
        asm.finish("f", sig(&[], &[])).unwrap_err(),
        AsmError::UnboundLabel(label)
    );
    let label = asm.label();
    asm.bind(label).unwrap();
    assert_eq!(asm.bind(label), Err(AsmError::Rebound(label)));
    assert_eq!(
        asm.emit(Op::J, Operands::default(), S),
        Err(AsmError::NotPlain(Op::J))
    );
    assert_eq!(
        asm.branch_if(Cond::Gt, 0, 1, label, S),
        Err(AsmError::UnsupportedCondition(Cond::Gt))
    );
}

#[test]
fn spans_and_starts() {
    let mut asm = Assembler::new();
    asm.load(1, 70_000, I64, Span::new(10, 15)).unwrap();
    asm.emit(Op::Mov, Operands::abc(300, 1, 0), Span::new(20, 25))
        .unwrap();
    asm.ret(Span::new(30, 31)).unwrap();
    let f = asm.finish("f", sig(&[], &[])).unwrap();
    // LDK (1 word), WIDE MOV (2 words), RET.
    assert_eq!(f.code.len(), 4);
    assert!(f.is_start(0) && f.is_start(1) && !f.is_start(2) && f.is_start(3));
    assert_eq!(f.span_at(0), Some(Span::new(10, 15)));
    assert_eq!(f.span_at(1), Some(Span::new(20, 25)));
    assert_eq!(f.span_at(3), Some(Span::new(30, 31)));
}

#[test]
fn loops_reach_a_fixpoint() {
    // var i = 0; while i < n { i = i + 1 }; return i
    let mut asm = Assembler::new();
    let top = asm.label();
    let done = asm.label();
    asm.load(2, 0, I64, S).unwrap();
    asm.load(3, 1, I64, S).unwrap();
    asm.bind(top).unwrap();
    asm.branch_if(Cond::Le, 1, 2, done, S).unwrap();
    asm.emit(Op::AddChkS, Operands::abc(2, 2, 3), S).unwrap();
    asm.jump(top, S);
    asm.bind(done).unwrap();
    asm.emit(Op::Mov, Operands::abc(0, 2, 0), S).unwrap();
    asm.ret(S).unwrap();
    check(&asm.finish("count", sig(&[I64], &[I64])).unwrap()).unwrap();
}

#[test]
fn the_verifier_rejects_ill_typed_code() {
    let mut asm = Assembler::new();
    // Reading a slot nothing wrote.
    asm.emit(Op::AddChkS, Operands::abc(0, 1, 3), S).unwrap();
    asm.ret(S).unwrap();
    let f = asm.finish("f", sig(&[I64, I64, I64], &[I64])).unwrap();
    check(&f).unwrap();
    asm.emit(Op::AddChkS, Operands::abc(0, 1, 4), S).unwrap();
    asm.ret(S).unwrap();
    let mut f = asm.finish("f", sig(&[I64], &[I64])).unwrap();
    f.frame_size = 5;
    assert_eq!(kind(&f), ErrorKind::Uninitialized(4));

    // A bool where an integer is required.
    asm.emit(Op::AddChkS, Operands::abc(0, 1, 1), S).unwrap();
    asm.ret(S).unwrap();
    let f = asm.finish("f", sig(&[Bool], &[I64])).unwrap();
    assert_eq!(
        kind(&f),
        ErrorKind::WrongType {
            slot: 1,
            expected: I64,
            found: Bool
        }
    );

    // Returning without writing the result.
    asm.ret(S).unwrap();
    let f = asm.finish("f", sig(&[], &[I64])).unwrap();
    assert_eq!(kind(&f), ErrorKind::Uninitialized(0));

    // A bool literal that is not 0 or 1.
    asm.load(1, 2, Bool, S).unwrap();
    asm.ret(S).unwrap();
    assert_eq!(
        kind(&asm.finish("f", sig(&[], &[])).unwrap()),
        ErrorKind::BadBool(2)
    );

    // `!` on a bool is XOR with a bool 1; with an integer 1 it is mistyped.
    asm.load(2, 1, Bool, S).unwrap();
    asm.emit(Op::Xor, Operands::abc(0, 1, 2), S).unwrap();
    asm.ret(S).unwrap();
    check(&asm.finish("not", sig(&[Bool], &[Bool])).unwrap()).unwrap();
    asm.load(2, 1, I64, S).unwrap();
    asm.emit(Op::Xor, Operands::abc(0, 1, 2), S).unwrap();
    asm.ret(S).unwrap();
    assert!(matches!(
        kind(&asm.finish("not", sig(&[Bool], &[Bool])).unwrap()),
        ErrorKind::WrongType { slot: 2, .. }
    ));
}

/// `if c { r2 = <a> } else { r2 = <b> }`, then `then_read` reads r2.
fn join(a: ValueType, b: ValueType, then_read: bool) -> Function {
    let mut asm = Assembler::new();
    let otherwise = asm.label();
    let end = asm.label();
    asm.branch_test(Test::False, 0, otherwise, S);
    asm.load(2, 1, a, S).unwrap();
    asm.jump(end, S);
    asm.bind(otherwise).unwrap();
    asm.load(2, 0, b, S).unwrap();
    asm.bind(end).unwrap();
    if then_read {
        asm.emit(Op::Mov, Operands::abc(3, 2, 0), S).unwrap();
    }
    asm.ret(S).unwrap();
    asm.finish("f", sig(&[Bool], &[])).unwrap()
}

#[test]
fn joins_must_agree_only_where_read() {
    check(&join(I64, I64, true)).unwrap();
    assert_eq!(kind(&join(I64, Bool, true)), ErrorKind::Conflict(2));
    // A temporary reused with another type is fine if nobody reads it.
    check(&join(I64, Bool, false)).unwrap();

    // Written on one path only.
    let mut asm = Assembler::new();
    let end = asm.label();
    asm.branch_test(Test::False, 0, end, S);
    asm.load(2, 1, I64, S).unwrap();
    asm.bind(end).unwrap();
    asm.emit(Op::Mov, Operands::abc(3, 2, 0), S).unwrap();
    asm.ret(S).unwrap();
    assert_eq!(
        kind(&asm.finish("f", sig(&[Bool], &[])).unwrap()),
        ErrorKind::Uninitialized(2)
    );
}

fn assembled_return() -> Function {
    let mut asm = Assembler::new();
    asm.load(300, 70_000, I64, S).unwrap(); // WIDE LDK: 2 words
    asm.ret(S).unwrap();
    asm.finish("f", sig(&[], &[])).unwrap()
}

#[test]
fn the_verifier_rejects_malformed_code() {
    // A branch into the `WIDE` follower, and one into an operand word.
    for target in [1, 4] {
        let mut f = assembled_return();
        let mut code = f.code.to_vec();
        let mut jump = Vec::new();
        encode(Op::J, Operands::abc(0, 0, 0), &mut jump).unwrap();
        // Prepend nothing: replace RET with `J` to `target` and append an
        // LDK_X whose operand word sits at pc 4.
        code.pop();
        let j_at = code.len() as i64;
        jump.clear();
        encode(Op::J, Operands::abc(target - (j_at + 1), 0, 0), &mut jump).unwrap();
        code.extend(&jump);
        let ldkx_at = code.len();
        encode(
            Op::LdkX,
            Operands {
                a: 1,
                ext: 0,
                ..Operands::default()
            },
            &mut code,
        )
        .unwrap();
        encode(Op::Ret, Operands::default(), &mut code).unwrap();
        f.code = code.into_boxed_slice();
        let mut starts = vec![0u64; 1];
        for at in [0, j_at as usize, ldkx_at, ldkx_at + 2] {
            starts[0] |= 1 << at;
        }
        f.starts = starts.into_boxed_slice();
        f.literal_types = vec![(0, I64), (ldkx_at as u32, I64)].into_boxed_slice();
        assert_eq!(kind(&f), ErrorKind::BadTarget(target), "target {target}");
    }

    // An opcode outside the first subset.
    let mut f = assembled_return();
    let mut code = Vec::new();
    encode(Op::ShlS16, Operands::abc(0, 0, 0), &mut code).unwrap();
    encode(Op::Ret, Operands::default(), &mut code).unwrap();
    f.code = code.into_boxed_slice();
    f.starts = vec![0b11].into_boxed_slice();
    assert_eq!(kind(&f), ErrorKind::Unimplemented(Op::ShlS16));

    // Falling off the end, and an empty function.
    let mut asm = Assembler::new();
    asm.emit(Op::Mov, Operands::abc(1, 0, 0), S).unwrap();
    assert_eq!(
        kind(&asm.finish("f", sig(&[I64], &[])).unwrap()),
        ErrorKind::FallsOffEnd
    );
    assert_eq!(
        kind(&asm.finish("f", sig(&[], &[])).unwrap()),
        ErrorKind::FallsOffEnd
    );

    // A slot outside the frame, a wrong start map, an unknown word.
    let mut f = assembled_return();
    f.frame_size = 300;
    assert_eq!(kind(&f), ErrorKind::SlotOutsideFrame(300));
    let mut f = assembled_return();
    f.starts = vec![0b0111].into_boxed_slice();
    assert_eq!(kind(&f), ErrorKind::StartMapMismatch);
    let mut f = assembled_return();
    f.code = vec![250].into_boxed_slice();
    assert!(matches!(
        kind(&f),
        ErrorKind::Decode(DecodeError::UnknownOpcode(250))
    ));
}

#[test]
fn calls_are_checked_against_signatures() {
    // A host call `emit(i64) -> void` from a `void` function whose one
    // parameter is in r0 (no result slots come first).
    let host_emit = CallSite {
        target: CallTarget::Host(0),
        args: SlotRange { first: 0, count: 1 },
        result: SlotRange { first: 1, count: 0 },
    };
    let mut asm = Assembler::new();
    asm.call(host_emit, S).unwrap();
    asm.ret(S).unwrap();
    check(&asm.finish("f", sig(&[I64], &[])).unwrap()).unwrap();

    // The wrong argument type.
    asm.call(host_emit, S).unwrap();
    asm.ret(S).unwrap();
    assert!(matches!(
        kind(&asm.finish("f", sig(&[Bool], &[])).unwrap()),
        ErrorKind::WrongType { slot: 0, .. }
    ));

    // A recursive script call writes its typed result.
    let recurse = CallSite {
        target: CallTarget::Script(FunctionId::from_u32(0)),
        args: SlotRange { first: 1, count: 1 },
        result: SlotRange { first: 0, count: 1 },
    };
    asm.call(recurse, S).unwrap();
    asm.ret(S).unwrap();
    check(&asm.finish("f", sig(&[I64], &[I64])).unwrap()).unwrap();

    // Windows that do not match the callee, or a callee that does not exist.
    let wrong_count = CallSite {
        args: SlotRange { first: 1, count: 2 },
        ..host_emit
    };
    asm.call(wrong_count, S).unwrap();
    asm.ret(S).unwrap();
    assert_eq!(
        kind(&asm.finish("f", sig(&[I64, I64], &[])).unwrap()),
        ErrorKind::CallShape
    );
    let missing = CallSite {
        target: CallTarget::Host(9),
        ..host_emit
    };
    asm.call(missing, S).unwrap();
    asm.ret(S).unwrap();
    assert_eq!(
        kind(&asm.finish("f", sig(&[I64], &[])).unwrap()),
        ErrorKind::NoCallee
    );
}

/// Found by the bytecode fuzzer (`crates/eitri/tests/fuzz.rs`): the
/// threaded handlers trust that a call's opcode matches its site's target,
/// and the typed pass seeds the parameters into the frame, so both must be
/// rejected rather than reach a panic.
#[test]
fn call_kinds_and_frame_sizes_are_checked() {
    let host_emit = CallSite {
        target: CallTarget::Host(0),
        args: SlotRange { first: 0, count: 1 },
        result: SlotRange { first: 0, count: 0 },
    };
    let mut asm = Assembler::new();
    asm.call(host_emit, S).unwrap();
    asm.ret(S).unwrap();
    let f = asm.finish("f", sig(&[I64], &[])).unwrap();
    check(&f).unwrap();
    for (op, site) in [
        (Op::CallScript, host_emit),
        (
            Op::CallHost,
            CallSite {
                target: CallTarget::Script(FunctionId::from_u32(0)),
                ..host_emit
            },
        ),
    ] {
        let mut wrong = f.clone();
        let mut code = wrong.code.to_vec();
        code[0] = (code[0] & !0xff) | op as u32;
        wrong.code = code.into_boxed_slice();
        wrong.sites = vec![site].into_boxed_slice();
        assert_eq!(kind(&wrong), ErrorKind::CallKind, "{op:?}");
    }

    // One slot short of the result and parameter.
    let mut asm = Assembler::new();
    asm.emit(Op::Mov, Operands::abc(0, 1, 0), S).unwrap();
    asm.ret(S).unwrap();
    let mut small = asm.finish("f", sig(&[I64], &[I64])).unwrap();
    check(&small).unwrap();
    small.frame_size = 1;
    assert_eq!(kind(&small), ErrorKind::FrameTooSmall);
}
