//! Eitri's typed bytecode: the machine-readable operation specification
//! (T003), instruction encoding and decoding, disassembly, and the
//! assembler and verifier that produce executable functions (T005).

mod asm;
mod disasm;
mod encoding;
mod function;
pub mod oracle;
pub mod spec;
mod verify;

pub use asm::{AsmError, Assembler, Label, imm_complement};
pub use disasm::disassemble;
pub use encoding::{DecodeError, Decoded, EncodeError, MAX_SLOT, Operands, decode, encode};
pub use function::{CallSite, CallTarget, Function, FunctionId, Signature, SlotRange, ValueType};
pub use spec::{OPS, Op, OpInfo};
pub use verify::{Callees, ErrorKind, VerifyError, verify};

#[cfg(test)]
mod asm_tests;
#[cfg(test)]
mod tests;
