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
use crate::spec::Op;

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
        Some(ValueType::I8) => "i8",
        Some(ValueType::U8) => "u8",
        Some(ValueType::U64) => "u64",
        Some(ValueType::F32) => "f32",
        Some(ValueType::F64) => "f64",
        Some(ValueType::Char) => "char",
        Some(ValueType::String) => "string",
        Some(ValueType::NullableString) => "nullable_string",
        Some(ValueType::Bool) => "bool",
        None => "void",
    }
}

fn mnemonic(op: Op) -> String {
    op.info().name.to_ascii_lowercase()
}

#[cfg(test)]
mod tests;
