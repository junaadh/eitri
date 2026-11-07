use eitri_base::{IndexVec, Span};
use eitri_bytecode::oracle::{self, Arith, IntType};
use eitri_bytecode::spec::{Cond, Test};
use eitri_bytecode::{
    Assembler, CallSite, CallTarget, Callees, Function, FunctionId, Op, Operands, Signature,
    SlotRange, ValueType, disassemble, verify,
};

use super::*;

const S: Span = Span::new(0, 0);
use ValueType::{Bool, I64};

fn sig(params: &[ValueType], results: &[ValueType]) -> Signature {
    Signature {
        params: params.to_vec(),
        results: results.to_vec(),
    }
}

/// Records host calls; host 0 is `emit(i64)`, 1 is `fail()`, 2 is
/// `double(i64) -> i64`.
#[derive(Default)]
struct Recorder {
    effects: Vec<String>,
}

impl Host for Recorder {
    fn call(&mut self, cx: &mut Call<'_, '_>, index: u32) -> Result<(), HostError> {
        match index {
            0 => self.effects.push(format!("emit({})", cx.arg(0) as i64)),
            1 => return Err("requested by test".into()),
            2 => cx.set_result(0, (cx.arg(0) as i64 * 2) as u64),
            _ => unreachable!(),
        }
        Ok(())
    }
}

/// The PC of a script trace entry.
fn pc(location: &Location) -> u32 {
    match *location {
        Location::Script { pc, .. } => pc,
        Location::Host(index) => panic!("host {index} where a script frame was expected"),
    }
}

fn host_signatures() -> Vec<Signature> {
    vec![sig(&[I64], &[]), sig(&[], &[]), sig(&[I64], &[I64])]
}

/// Verify every function, then run function 0.
fn execute(
    functions: Vec<Function>,
    args: &[u64],
    limits: Limits,
    host: &mut Recorder,
) -> Result<Vec<u64>, Fault> {
    let functions: IndexVec<FunctionId, Function> = IndexVec::from_raw(functions);
    let signatures = IndexVec::from_raw(functions.iter().map(|f| f.signature.clone()).collect());
    let hosts = host_signatures();
    for f in functions.iter() {
        verify(
            f,
            &Callees {
                functions: &signatures,
                hosts: &hosts,
            },
        )
        .unwrap_or_else(|e| panic!("{}: {e}", f.name));
    }
    let result = run(
        &functions,
        FunctionId::from_u32(0),
        args,
        host,
        limits,
        &mut VmScratch::new(),
    );
    // The experimental threaded interpreter must agree on everything:
    // results, faults with traces, and host effects.
    let mut threaded_host = Recorder::default();
    let threaded = run_threaded(
        &functions,
        FunctionId::from_u32(0),
        args,
        &mut threaded_host,
        limits,
        &mut VmScratch::new(),
    );
    assert_eq!(threaded, result, "threaded interpreter disagrees");
    assert_eq!(
        threaded_host.effects,
        host.effects[host.effects.len() - threaded_host.effects.len()..]
    );
    result
}

/// `fn(a: i64, b: i64) -> T { return op(a, b) }` as one instruction.
fn binary(op: Op, result: ValueType) -> Function {
    let mut asm = Assembler::new();
    asm.emit(op, Operands::abc(0, 1, 2), S).unwrap();
    asm.ret(S).unwrap();
    asm.finish("f", sig(&[I64, I64], &[result])).unwrap()
}

fn samples() -> Vec<i64> {
    let mut values = vec![
        i64::MIN,
        i64::MIN + 1,
        -7,
        -2,
        -1,
        0,
        1,
        2,
        7,
        i64::MAX - 1,
        i64::MAX,
    ];
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..24 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        values.push(seed as i64 >> (seed % 60));
    }
    values
}

fn kind(fault: oracle::Fault) -> FaultKind {
    match fault {
        oracle::Fault::Overflow => FaultKind::Overflow,
        oracle::Fault::DivisionByZero => FaultKind::DivisionByZero,
        oracle::Fault::ShiftAmount => unreachable!("no shifts in the first subset"),
    }
}

#[test]
fn arithmetic_agrees_with_the_oracle() {
    let ops = [
        (Op::AddChkS, Arith::Add),
        (Op::SubChkS, Arith::Sub),
        (Op::MulChkS, Arith::Mul),
        (Op::AddWrap, Arith::WrapAdd),
        (Op::SubWrap, Arith::WrapSub),
        (Op::MulWrap, Arith::WrapMul),
        (Op::DivS, Arith::Div),
        (Op::RemS, Arith::Rem),
    ];
    let values = samples();
    for (op, arith) in ops {
        for &a in &values {
            for &b in &values {
                let got = execute(
                    vec![binary(op, I64)],
                    &[a as u64, b as u64],
                    Limits::default(),
                    &mut Recorder::default(),
                );
                let want = oracle::arith(IntType::I64, arith, a as i128, b as i128);
                match (got, want) {
                    (Ok(r), Ok(w)) => assert_eq!(r[0] as i64 as i128, w, "{op:?} {a} {b}"),
                    (Err(f), Err(w)) => {
                        assert_eq!(f.kind, kind(w), "{op:?} {a} {b}");
                        assert_eq!(
                            f.trace,
                            [Location::Script {
                                function: FunctionId::from_u32(0),
                                pc: 0
                            }]
                        );
                    }
                    (got, want) => panic!("{op:?} {a} {b}: vm {got:?}, oracle {want:?}"),
                }
            }
        }
    }
    // Negation.
    let mut asm = Assembler::new();
    asm.emit(Op::NegChkS, Operands::abc(0, 1, 0), S).unwrap();
    asm.ret(S).unwrap();
    let neg = asm.finish("neg", sig(&[I64], &[I64])).unwrap();
    for &a in &values {
        let got = execute(
            vec![neg.clone()],
            &[a as u64],
            Limits::default(),
            &mut Recorder::default(),
        );
        match (got, oracle::neg(IntType::I64, a as i128)) {
            (Ok(r), Ok(w)) => assert_eq!(r[0] as i64 as i128, w),
            (Err(f), Err(w)) => assert_eq!(f.kind, kind(w)),
            (got, want) => panic!("neg {a}: vm {got:?}, oracle {want:?}"),
        }
    }
}

/// The same operations through `WIDE`-prefixed instructions: operands in
/// slots above 255, so the handlers' wide expansion is checked too.
#[test]
fn widened_operations_agree_with_the_oracle() {
    let widened = |op: Op, result: ValueType| {
        let mut asm = Assembler::new();
        asm.emit(Op::Mov, Operands::abc(300, 1, 0), S).unwrap();
        asm.emit(Op::Mov, Operands::abc(301, 2, 0), S).unwrap();
        asm.emit(op, Operands::abc(302, 300, 301), S).unwrap();
        asm.emit(Op::Mov, Operands::abc(0, 302, 0), S).unwrap();
        asm.ret(S).unwrap();
        asm.finish("wide", sig(&[I64, I64], &[result])).unwrap()
    };
    let values = samples();
    for (op, arith) in [
        (Op::AddChkS, Arith::Add),
        (Op::MulWrap, Arith::WrapMul),
        (Op::DivS, Arith::Div),
        (Op::RemS, Arith::Rem),
    ] {
        let f = widened(op, I64);
        assert!(disassemble(&f.code).contains("WIDE"));
        for &a in &values {
            for &b in &values {
                let got = execute(
                    vec![f.clone()],
                    &[a as u64, b as u64],
                    Limits::default(),
                    &mut Recorder::default(),
                );
                match (
                    got,
                    oracle::arith(IntType::I64, arith, a as i128, b as i128),
                ) {
                    (Ok(r), Ok(w)) => assert_eq!(r[0] as i64 as i128, w, "{op:?} {a} {b}"),
                    (Err(f), Err(w)) => assert_eq!(f.kind, kind(w), "{op:?} {a} {b}"),
                    (got, want) => panic!("{op:?} {a} {b}: vm {got:?}, oracle {want:?}"),
                }
            }
        }
    }
    let f = widened(Op::CsetLt, ValueType::Bool);
    for &a in &values {
        for &b in &values {
            let got = execute(
                vec![f.clone()],
                &[a as u64, b as u64],
                Limits::default(),
                &mut Recorder::default(),
            );
            assert_eq!(
                got.unwrap()[0],
                u64::from(oracle::int_cond(Cond::Lt, a, b)),
                "{a} < {b}"
            );
        }
    }
}

/// Immediate forms (`ALUI`, `CBI`) against the oracle, with the operand
/// in a narrow slot and in a widened one.
#[test]
fn immediate_operations_agree_with_the_oracle() {
    let imms = [-128i64, -7, -1, 0, 1, 2, 7, 127];
    let values = samples();
    for slot in [1u32, 300] {
        let lead = |asm: &mut Assembler| {
            if slot != 1 {
                asm.emit(Op::Mov, Operands::abc(slot.into(), 1, 0), S)
                    .unwrap();
            }
        };
        for (op, arith) in [
            (Op::AddiChkS, Arith::Add),
            (Op::SubiChkS, Arith::Sub),
            (Op::MuliChkS, Arith::Mul),
            (Op::AddiWrap, Arith::WrapAdd),
            (Op::SubiWrap, Arith::WrapSub),
        ] {
            for k in imms {
                let mut asm = Assembler::new();
                lead(&mut asm);
                asm.emit(op, Operands::abc(0, slot.into(), k), S).unwrap();
                asm.ret(S).unwrap();
                let f = asm.finish("alui", sig(&[I64], &[I64])).unwrap();
                for &a in &values {
                    let got = execute(
                        vec![f.clone()],
                        &[a as u64],
                        Limits::default(),
                        &mut Recorder::default(),
                    );
                    match (
                        got,
                        oracle::arith(IntType::I64, arith, a as i128, k as i128),
                    ) {
                        (Ok(r), Ok(w)) => assert_eq!(r[0] as i64 as i128, w, "{op:?} {a} {k}"),
                        (Err(f), Err(w)) => assert_eq!(f.kind, kind(w), "{op:?} {a} {k}"),
                        (got, want) => panic!("{op:?} {a} {k}: vm {got:?}, oracle {want:?}"),
                    }
                }
            }
        }
        for cond in [Cond::Eq, Cond::Ne, Cond::Lt, Cond::Gt] {
            // Both the short form and the complemented long form.
            for k in imms
                .into_iter()
                .filter(|&k| eitri_bytecode::imm_complement(cond, k).is_some())
            {
                for far in [false, true] {
                    let mut asm = Assembler::new();
                    lead(&mut asm);
                    let yes = asm.label();
                    asm.branch_imm(cond, slot, k, yes, S).unwrap();
                    asm.load(0, 0, ValueType::Bool, S).unwrap();
                    if far {
                        // Push the target past an eight-bit displacement.
                        for _ in 0..200 {
                            asm.emit(Op::Mov, Operands::abc(2, 1, 0), S).unwrap();
                        }
                    }
                    asm.ret(S).unwrap();
                    asm.bind(yes).unwrap();
                    asm.load(0, 1, ValueType::Bool, S).unwrap();
                    asm.ret(S).unwrap();
                    let f = asm.finish("cbi", sig(&[I64], &[Bool])).unwrap();
                    for &a in &values {
                        let got = execute(
                            vec![f.clone()],
                            &[a as u64],
                            Limits::default(),
                            &mut Recorder::default(),
                        );
                        let want = match cond {
                            Cond::Gt => a > k,
                            _ => oracle::int_cond(cond, a, k),
                        };
                        assert_eq!(
                            got.unwrap()[0],
                            u64::from(want),
                            "{cond:?} {a} {k} far {far}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn comparisons_agree_with_the_oracle() {
    let values = samples();
    let csets = [
        (Op::CsetEq, Cond::Eq),
        (Op::CsetNe, Cond::Ne),
        (Op::CsetLt, Cond::Lt),
        (Op::CsetLe, Cond::Le),
    ];
    for (op, cond) in csets {
        // CSET and the matching compare-and-branch must agree with each other
        // and with the oracle.
        let mut asm = Assembler::new();
        let taken = asm.label();
        asm.branch_if(cond, 1, 2, taken, S).unwrap();
        asm.load(0, 0, Bool, S).unwrap();
        asm.ret(S).unwrap();
        asm.bind(taken).unwrap();
        asm.load(0, 1, Bool, S).unwrap();
        asm.ret(S).unwrap();
        let branch = asm.finish("branch", sig(&[I64, I64], &[Bool])).unwrap();
        for &a in &values {
            for &b in &values {
                let args = [a as u64, b as u64];
                let want = u64::from(oracle::int_cond(cond, a, b));
                let set = execute(
                    vec![binary(op, Bool)],
                    &args,
                    Limits::default(),
                    &mut Recorder::default(),
                );
                let jumped = execute(
                    vec![branch.clone()],
                    &args,
                    Limits::default(),
                    &mut Recorder::default(),
                );
                assert_eq!(set.unwrap()[0], want, "{op:?} {a} {b}");
                assert_eq!(jumped.unwrap()[0], want, "branch {cond:?} {a} {b}");
            }
        }
    }
}

/// `fn count(n: i64) -> i64`: recursion `n` deep, `count(0) = 0`.
fn countdown() -> Function {
    let mut asm = Assembler::new();
    let base_case = asm.label();
    asm.load(2, 0, I64, S).unwrap();
    asm.branch_if(Cond::Eq, 1, 2, base_case, S).unwrap();
    asm.load(3, 1, I64, S).unwrap();
    asm.emit(Op::SubChkS, Operands::abc(4, 1, 3), S).unwrap();
    asm.call(
        CallSite {
            target: CallTarget::Script(FunctionId::from_u32(0)),
            args: SlotRange { first: 4, count: 1 },
            result: SlotRange { first: 5, count: 1 },
        },
        S,
    )
    .unwrap();
    asm.emit(Op::AddChkS, Operands::abc(0, 5, 3), S).unwrap();
    asm.ret(S).unwrap();
    asm.bind(base_case).unwrap();
    asm.emit(Op::Mov, Operands::abc(0, 2, 0), S).unwrap();
    asm.ret(S).unwrap();
    asm.finish("count", sig(&[I64], &[I64])).unwrap()
}

#[test]
fn recursion_uses_vm_frames_not_the_rust_stack() {
    let limits = Limits {
        budget: u64::MAX,
        depth: 200_000,
    };
    // Deep enough to overflow a Rust stack if frames recursed in Rust;
    // shallower under Miri, where it still grows the slot and frame stacks.
    let deep: u64 = if cfg!(miri) { 1_000 } else { 100_000 };
    let result = execute(vec![countdown()], &[deep], limits, &mut Recorder::default());
    assert_eq!(result.unwrap(), [deep]);

    // The depth limit faults at the call, with the whole chain in the trace.
    let limits = Limits {
        budget: u64::MAX,
        depth: 3,
    };
    let fault = execute(vec![countdown()], &[10], limits, &mut Recorder::default()).unwrap_err();
    assert_eq!(fault.kind, FaultKind::Depth);
    assert_eq!(
        fault.trace.len(),
        4,
        "the faulting call plus three suspended frames"
    );
    let call_pc = pc(&fault.trace[0]);
    assert!(fault.trace.iter().all(|l| pc(l) == call_pc));
}

/// `while i < n { i += 1; emit(i) }`: n backward branches and n calls.
fn emit_loop() -> Function {
    let mut asm = Assembler::new();
    let top = asm.label();
    let done = asm.label();
    asm.load(1, 0, I64, S).unwrap();
    asm.load(2, 1, I64, S).unwrap();
    asm.bind(top).unwrap();
    asm.branch_if(Cond::Le, 0, 1, done, S).unwrap();
    asm.emit(Op::AddChkS, Operands::abc(1, 1, 2), S).unwrap();
    asm.call(
        CallSite {
            target: CallTarget::Host(0),
            args: SlotRange { first: 1, count: 1 },
            result: SlotRange { first: 3, count: 0 },
        },
        S,
    )
    .unwrap();
    asm.jump(top, S);
    asm.bind(done).unwrap();
    asm.ret(S).unwrap();
    asm.finish("loop", sig(&[I64], &[])).unwrap()
}

#[test]
fn calls_and_backward_branches_share_the_budget() {
    // Three iterations: three host calls and three backward jumps.
    let mut host = Recorder::default();
    let exact = Limits {
        budget: 6,
        depth: 10,
    };
    assert!(execute(vec![emit_loop()], &[3], exact, &mut host).is_ok());
    assert_eq!(host.effects, ["emit(1)", "emit(2)", "emit(3)"]);

    // One step short: the last backward jump faults, after every effect
    // before it.
    let mut host = Recorder::default();
    let short = Limits {
        budget: 5,
        depth: 10,
    };
    let fault = execute(vec![emit_loop()], &[3], short, &mut host).unwrap_err();
    assert_eq!(fault.kind, FaultKind::Budget);
    assert_eq!(host.effects, ["emit(1)", "emit(2)", "emit(3)"]);
    let jump_pc = pc(&fault.trace[0]);
    assert_eq!(emit_loop().code[jump_pc as usize] as u8, Op::J as u8);

    // Two steps short: the third call faults before it happens.
    let mut host = Recorder::default();
    let fault = execute(
        vec![emit_loop()],
        &[3],
        Limits {
            budget: 4,
            depth: 10,
        },
        &mut host,
    )
    .unwrap_err();
    assert_eq!(fault.kind, FaultKind::Budget);
    assert_eq!(host.effects, ["emit(1)", "emit(2)"]);
}

#[test]
fn faults_report_the_right_location() {
    // main calls inner; inner overflows in a widened add.
    let mut asm = Assembler::new();
    asm.emit(Op::Mov, Operands::abc(300, 1, 0), S).unwrap();
    asm.emit(Op::AddChkS, Operands::abc(0, 300, 300), S)
        .unwrap();
    asm.ret(S).unwrap();
    let inner = asm.finish("inner", sig(&[I64], &[I64])).unwrap();
    let mut asm = Assembler::new();
    asm.load(1, i64::MAX as u64, I64, S).unwrap();
    asm.call(
        CallSite {
            target: CallTarget::Script(FunctionId::from_u32(1)),
            args: SlotRange { first: 1, count: 1 },
            result: SlotRange { first: 2, count: 1 },
        },
        S,
    )
    .unwrap();
    asm.ret(S).unwrap();
    let main = asm.finish("main", sig(&[], &[])).unwrap();
    let fault = execute(
        vec![main, inner],
        &[],
        Limits::default(),
        &mut Recorder::default(),
    )
    .unwrap_err();
    assert_eq!(fault.kind, FaultKind::Overflow);
    assert_eq!(
        fault.trace,
        [
            // The WIDE prefix of the add, after the 2-word WIDE MOV.
            Location::Script {
                function: FunctionId::from_u32(1),
                pc: 2
            },
            // main's CALL, after its 1-word LDK.
            Location::Script {
                function: FunctionId::from_u32(0),
                pc: 1
            },
        ]
    );
}

#[test]
fn host_calls_borrow_their_windows_and_can_fault() {
    let mut asm = Assembler::new();
    asm.load(1, 21, I64, S).unwrap();
    asm.call(
        CallSite {
            target: CallTarget::Host(2),
            args: SlotRange { first: 1, count: 1 },
            result: SlotRange { first: 0, count: 1 },
        },
        S,
    )
    .unwrap();
    asm.call(
        CallSite {
            target: CallTarget::Host(0),
            args: SlotRange { first: 0, count: 1 },
            result: SlotRange { first: 0, count: 0 },
        },
        S,
    )
    .unwrap();
    asm.call(
        CallSite {
            target: CallTarget::Host(1),
            args: SlotRange { first: 0, count: 0 },
            result: SlotRange { first: 0, count: 0 },
        },
        S,
    )
    .unwrap();
    asm.ret(S).unwrap();
    let f = asm.finish("f", sig(&[], &[I64])).unwrap();
    let mut host = Recorder::default();
    let fault = execute(vec![f], &[], Limits::default(), &mut host).unwrap_err();
    assert_eq!(host.effects, ["emit(42)"]);
    assert_eq!(fault.kind, FaultKind::Host("requested by test".into()));
    assert_eq!(
        fault.trace,
        [
            Location::Host(1),
            Location::Script {
                function: FunctionId::from_u32(0),
                pc: 3
            }
        ]
    );
}

#[test]
fn scratch_is_reused_without_growth() {
    let functions: IndexVec<FunctionId, Function> = IndexVec::from_raw(vec![countdown()]);
    let mut scratch = VmScratch::new();
    let mut host = Recorder::default();
    let limits = Limits {
        budget: u64::MAX,
        depth: 1000,
    };
    run(
        &functions,
        FunctionId::from_u32(0),
        &[500],
        &mut host,
        limits,
        &mut scratch,
    )
    .unwrap();
    let retained = scratch.heap_bytes();
    for _ in 0..3 {
        let result = run(
            &functions,
            FunctionId::from_u32(0),
            &[500],
            &mut host,
            limits,
            &mut scratch,
        );
        assert_eq!(result.unwrap(), [500]);
        assert_eq!(scratch.heap_bytes(), retained);
    }
}

#[test]
fn bool_not_and_tests() {
    // `!b` is `b ^ true`; `TB` branches on it.
    let mut asm = Assembler::new();
    let yes = asm.label();
    asm.load(2, 1, Bool, S).unwrap();
    asm.emit(Op::Xor, Operands::abc(3, 1, 2), S).unwrap();
    asm.branch_test(Test::True, 3, yes, S);
    asm.load(0, 10, I64, S).unwrap();
    asm.ret(S).unwrap();
    asm.bind(yes).unwrap();
    asm.load(0, 20, I64, S).unwrap();
    asm.ret(S).unwrap();
    let f = asm.finish("f", sig(&[Bool], &[I64])).unwrap();
    assert_eq!(
        execute(
            vec![f.clone()],
            &[0],
            Limits::default(),
            &mut Recorder::default()
        )
        .unwrap(),
        [20]
    );
    assert_eq!(
        execute(vec![f], &[1], Limits::default(), &mut Recorder::default()).unwrap(),
        [10]
    );
}
