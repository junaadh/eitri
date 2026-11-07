//! `.eits`: the text form of bytecode, printed from functions and parsed
//! back into them. Specified in
//! [Bytecode: text form](../../../docs/implementation/bytecode.md#text-form-eits).
//!
//! The text is at the assembler's level: literal loads, branches to labels,
//! and calls with their callee and argument slots. The assembler chooses the
//! encoding (`WIDE` prefixes, `LDI`/`LDK`/`LDK_X`, short or far branches),
//! exactly as for compiled code, so printing compiled functions and
//! assembling the text gives the same words. Annotated printing adds the
//! PCs, words, call-site indices, and source spans as comments.
//!
//! Both directions are driven by the operation specification: an opcode's
//! lowercased name is its mnemonic, and its operand roles say what text its
//! operands take. Literal loads, branches, calls, and `RET` have their own
//! forms because the assembler owns their encoding.

mod parse;
mod print;

pub use parse::{Parsed, TextError, parse};
pub use print::{PrintOptions, print};

use crate::function::{Signature, ValueType};
use crate::spec::{Cond, Op, Test};

/// A host function as the text names it: its qualified name, such as
/// `bench.emit`, and its signature. A call site's host index selects one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDecl {
    pub name: String,
    pub signature: Signature,
}

/// The entry function's name. Not a script identifier, so it cannot clash.
pub const MODULE: &str = "<module>";

fn type_name(ty: Option<ValueType>) -> &'static str {
    match ty {
        Some(ValueType::I64) => "i64",
        Some(ValueType::Bool) => "bool",
        None => "void",
    }
}

fn mnemonic(op: Op) -> String {
    op.info().name.to_ascii_lowercase()
}

/// The logical branch a far form (the complement over a `J_X`) encodes:
/// every complement is its own inverse.
fn uncomplement(op: Op, a: i64, b: i64) -> Option<(Op, i64, i64)> {
    use crate::asm::{compare_imm_op, compare_op, imm_complement, test_op};
    use crate::spec::Sem;
    Some(match op.info().sem {
        Sem::Branch { cond, imm: false } => {
            let (cond, swap): (Cond, bool) = cond.complement()?;
            let (a, b) = if swap { (b, a) } else { (a, b) };
            (compare_op(cond), a, b)
        }
        Sem::Branch { cond, imm: true } => {
            let (cond, imm) = imm_complement(cond, b)?;
            (compare_imm_op(cond), a, imm)
        }
        Sem::TestBranch(test) => {
            let test: Test = test.complement();
            (test_op(test), a, b)
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
