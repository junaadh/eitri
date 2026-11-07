//! Disassembly: the words actually emitted, one instruction per line, with
//! operands printed by role and branch targets resolved.

use std::fmt::Write;

use crate::encoding::decode;
use crate::spec::Role;

/// Disassemble `code`. Undecodable words print as `.word` and the listing
/// continues with the next word, so a listing never hides bytes.
pub fn disassemble(code: &[u32]) -> String {
    let mut out = String::new();
    let mut pc = 0;
    while pc < code.len() {
        match decode(code, pc) {
            Ok(insn) => {
                let info = insn.op.info();
                let end = pc as i64 + insn.words as i64;
                let mut operands = Vec::new();
                let columns = [insn.operands.a, insn.operands.b, insn.operands.c];
                for (&role, &value) in info.roles.iter().zip(&columns) {
                    if let Some(text) = operand(role, value, end) {
                        operands.push(text);
                    }
                }
                if let Some(text) = operand(info.format.ext_role(), insn.operands.ext, end) {
                    operands.push(text);
                }
                let name = if insn.wide {
                    format!("WIDE {}", info.name)
                } else {
                    info.name.to_owned()
                };
                let line = format!("{pc:04}  {name:<12} {}", operands.join(", "));
                let _ = writeln!(out, "{}", line.trim_end());
                pc += insn.words as usize;
            }
            Err(error) => {
                let _ = writeln!(out, "{pc:04}  .word {:#010x}  ; {error:?}", code[pc]);
                pc += 1;
            }
        }
    }
    out
}

fn operand(role: Role, value: i64, end: i64) -> Option<String> {
    Some(match role {
        Role::None => return None,
        Role::Dst | Role::Src | Role::SrcDst => format!("r{value}"),
        Role::Imm8 | Role::Imm16 | Role::Shift8 => format!("#{value}"),
        Role::Disp8 | Role::Disp16 | Role::Disp24 | Role::Disp32 => {
            format!("-> {:04}", end + value)
        }
        Role::Const16 | Role::Const32 => format!("k{value}"),
        Role::Site16 => format!("site{value}"),
        Role::Index8 | Role::Index16 => format!("@{value}"),
        Role::Offset8 | Role::Offset32 => format!("+{value}"),
        Role::Width8 => format!("w{value:#04x}"),
    })
}
