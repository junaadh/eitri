//! The logical instruction level: `.eits` held in memory. One [`Insn`] per
//! `.eits` instruction line, one [`Insn::Label`] per label.
//!
//! A recording [`Assembler`] (`Assembler<true>`) records it as it is
//! driven: lowering uses one only when something will use the IR
//! (printing, re-optimizing), so the first run of fresh source pays for no
//! extra layer, not even a check. The `.eits` parser builds it directly, and
//! [`assemble`] encodes it, recording again so the result carries its final
//! positions.

use eitri_base::Span;

use crate::asm::{AsmError, Assembler, Label};
use crate::encoding::Operands;
use crate::function::{CallSite, Function, Signature, ValueType};
use crate::spec::{Cond, Op, Test};

/// A branch, without its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    /// `CB`: compare two slots.
    Compare {
        cond: Cond,
        a: u32,
        b: u32,
    },
    /// `CBI`: compare a slot with an eight-bit immediate.
    CompareImm {
        cond: Cond,
        a: u32,
        imm: i64,
    },
    /// `TB`: test a `bool` slot.
    Test {
        test: Test,
        a: u32,
    },
    Jump,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Insn {
    /// Marks the next instruction as label `n`'s target.
    Label(u32),
    /// A literal: `LDI`, `LDK`, or `LDK_X`, as the assembler chooses.
    Load {
        dst: u32,
        value: u64,
        ty: ValueType,
    },
    /// Any instruction that is not a load, branch, call, or return.
    Plain {
        op: Op,
        operands: Operands,
    },
    Branch {
        branch: Branch,
        target: u32,
    },
    Call(CallSite),
    Ret,
}

/// An instruction and its source span. Once assembled, `pc` and `words`
/// locate its encoding (both instructions of a far branch); before, they
/// are zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    pub insn: Insn,
    pub span: Span,
    pub pc: u32,
    pub words: u32,
}

impl Item {
    pub fn new(insn: Insn, span: Span) -> Self {
        Item {
            insn,
            span,
            pc: 0,
            words: 0,
        }
    }
}

/// A function body at the logical level. Labels are numbered `0..labels`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Body {
    pub items: Vec<Item>,
    pub labels: u32,
}

/// Encode `body` as a function, and return it with the body as assembled
/// (positions filled in). `asm` is reusable scratch.
pub fn assemble(
    body: &Body,
    name: &str,
    signature: Signature,
    asm: &mut Assembler<true>,
) -> Result<(Function, Body), AsmError> {
    let labels: Vec<Label> = (0..body.labels).map(|_| asm.label()).collect();
    let built = (|| {
        for item in &body.items {
            let span = item.span;
            match item.insn {
                Insn::Label(n) => asm.bind(labels[n as usize])?,
                Insn::Load { dst, value, ty } => asm.load(dst, value, ty, span)?,
                Insn::Plain { op, operands } => asm.emit(op, operands, span)?,
                Insn::Branch { branch, target } => {
                    let label = labels[target as usize];
                    match branch {
                        Branch::Compare { cond, a, b } => asm.branch_if(cond, a, b, label, span)?,
                        Branch::CompareImm { cond, a, imm } => {
                            asm.branch_imm(cond, a, imm, label, span)?
                        }
                        Branch::Test { test, a } => asm.branch_test(test, a, label, span),
                        Branch::Jump => asm.jump(label, span),
                    }
                }
                Insn::Call(site) => asm.call(site, span)?,
                Insn::Ret => asm.ret(span)?,
            }
        }
        Ok(())
    })();
    match built {
        Ok(()) => asm
            .finish(name, signature)
            .map(|function| (function, asm.take_recorded().unwrap_or_default())),
        Err(error) => {
            // Leave the assembler empty for its next function.
            let _ = asm.finish(name, Signature::default());
            Err(error)
        }
    }
}
