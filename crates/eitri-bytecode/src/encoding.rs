//! Encoding and decoding instructions to and from code words, driven by the
//! specification's formats and operand roles.

use crate::spec::{Format, Op, Role};

/// The values of an instruction's operand fields, by column: A, B, C as the
/// specification names them (X lives in the column its format says), and
/// `ext` for the second word of a two-word form. Slots may exceed 255; the
/// encoder adds the `WIDE` prefix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Operands {
    pub a: i64,
    pub b: i64,
    pub c: i64,
    pub ext: i64,
}

impl Operands {
    pub fn abc(a: i64, b: i64, c: i64) -> Self {
        Operands { a, b, c, ext: 0 }
    }

    fn get(&self, column: usize) -> i64 {
        [self.a, self.b, self.c][column]
    }

    fn set(&mut self, column: usize, value: i64) {
        match column {
            0 => self.a = value,
            1 => self.b = value,
            _ => self.c = value,
        }
    }
}

/// The largest slot index an instruction can name: 16 bits with `WIDE`.
pub const MAX_SLOT: i64 = u16::MAX as i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// A field's value does not fit its role.
    OutOfRange {
        column: usize,
        role: Role,
        value: i64,
    },
    /// A slot above 255 on an instruction that cannot take `WIDE`.
    NotWidenable { column: usize, value: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// No opcode has this number.
    UnknownOpcode(u8),
    /// The instruction runs past the end of the code.
    Truncated,
    /// A reserved field is not zero.
    Reserved,
    /// `WIDE` before an instruction that cannot take it, or a nonzero high
    /// byte for a field that is not a slot.
    IllegalWide,
}

/// A decoded instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub op: Op,
    pub operands: Operands,
    /// Words, including a `WIDE` prefix.
    pub words: u32,
    pub wide: bool,
}

/// Bit positions of the A, B, C columns in a short-form word.
const SHIFT: [u32; 3] = [8, 16, 24];

/// Where each column of `format` sits in the first word: shift and bits.
const fn layout(format: Format) -> [(u32, u32); 3] {
    match format {
        Format::Abc => [(8, 8), (16, 8), (24, 8)],
        Format::ABx | Format::AsBx => [(8, 8), (16, 16), (0, 0)],
        Format::Ax | Format::SAx => [(8, 24), (0, 0), (0, 0)],
        Format::LdkX => [(8, 8), (0, 0), (0, 0)],
        // `[ B | width | A | op ]`: the width byte is the C column.
        Format::FieldX => [(8, 8), (24, 8), (16, 8)],
        Format::JX | Format::Wide => [(0, 0); 3],
    }
}

fn signed(role: Role) -> bool {
    role.range().0 < 0
}

/// Append the encoding of `op` with `operands` to `out`.
/// One operand field of a narrow (un-`WIDE`) instruction: where it goes
/// and the values it accepts without a prefix.
#[derive(Clone, Copy)]
struct Field {
    shift: u32,
    mask: u32,
    min: i64,
    max: i64,
}

/// Everything [`encode`] needs about an opcode's narrow form, derived once
/// at compile time from the specification.
#[derive(Clone, Copy)]
struct Encoder {
    fields: [Field; 3],
    ext: (i64, i64),
    two_words: bool,
}

static ENCODERS: [Encoder; 256] = {
    let none = Field {
        shift: 0,
        mask: 0,
        min: 0,
        max: 0,
    };
    let mut table = [Encoder {
        fields: [none; 3],
        ext: (0, 0),
        two_words: false,
    }; 256];
    let mut i = 0;
    while i < crate::spec::OPS.len() {
        let info = &crate::spec::OPS[i];
        let layout = layout(info.format);
        let mut fields = [none; 3];
        let mut column = 0;
        while column < 3 {
            let role = info.roles[column];
            let (shift, bits) = layout[column];
            // Narrow slots only; anything wider takes the general path.
            let (min, max) = if role.is_widened() {
                (0, u8::MAX as i64)
            } else {
                role.range()
            };
            let mask = if bits == 32 {
                u32::MAX
            } else {
                (1 << bits) - 1
            };
            fields[column] = Field {
                shift,
                mask,
                min,
                max,
            };
            column += 1;
        }
        table[i] = Encoder {
            fields,
            ext: info.format.ext_role().range(),
            two_words: info.format.words() == 2,
        };
        i += 1;
    }
    table
};

/// Encode one instruction, adding a `WIDE` prefix if a slot needs one.
///
/// The common case (every field in its narrow range) packs the word from a
/// precomputed table with no per-role dispatch (E5/L2); anything else,
/// including every error, goes through [`encode_general`], so both paths
/// accept and produce exactly the same words.
#[inline]
pub fn encode(op: Op, operands: Operands, out: &mut Vec<u32>) -> Result<(), EncodeError> {
    let encoder = &ENCODERS[op as usize];
    let values = [operands.a, operands.b, operands.c];
    let mut word = op as u32;
    let mut narrow = operands.ext >= encoder.ext.0 && operands.ext <= encoder.ext.1;
    for (field, value) in encoder.fields.iter().zip(values) {
        narrow &= value >= field.min && value <= field.max;
        word |= (value as u32 & field.mask) << field.shift;
    }
    if !narrow {
        return encode_general(op, operands, out);
    }
    out.push(word);
    if encoder.two_words {
        out.push(operands.ext as u32);
    }
    Ok(())
}

/// The specification-driven encoder: handles `WIDE` and reports errors.
pub fn encode_general(op: Op, operands: Operands, out: &mut Vec<u32>) -> Result<(), EncodeError> {
    let info = op.info();
    let layout = layout(info.format);
    let mut low = operands;
    let mut high = [0u32; 3];
    for (column, &role) in info.roles.iter().enumerate() {
        let value = operands.get(column);
        if role.is_widened() {
            if !(0..=MAX_SLOT).contains(&value) {
                return Err(EncodeError::OutOfRange {
                    column,
                    role,
                    value,
                });
            }
            if value > u8::MAX as i64 {
                if !op.wide_legal() {
                    return Err(EncodeError::NotWidenable { column, value });
                }
                high[column] = (value >> 8) as u32;
                low.set(column, value & 0xff);
            }
            continue;
        }
        let (min, max) = role.range();
        if value < min || value > max {
            return Err(EncodeError::OutOfRange {
                column,
                role,
                value,
            });
        }
    }
    let ext_role = info.format.ext_role();
    let (min, max) = ext_role.range();
    if operands.ext < min || operands.ext > max {
        return Err(EncodeError::OutOfRange {
            column: 3,
            role: ext_role,
            value: operands.ext,
        });
    }

    if high != [0; 3] {
        out.push(Op::Wide as u32 | high[0] << 8 | high[1] << 16 | high[2] << 24);
    }
    let mut word = op as u32;
    for (column, &(shift, bits)) in layout.iter().enumerate() {
        if bits == 0 {
            continue;
        }
        let mask = if bits == 32 {
            u32::MAX
        } else {
            (1 << bits) - 1
        };
        word |= (low.get(column) as u32 & mask) << shift;
    }
    out.push(word);
    if info.format.words() == 2 {
        out.push(operands.ext as u32);
    }
    Ok(())
}

/// Decode the instruction at `pc`.
pub fn decode(code: &[u32], pc: usize) -> Result<Decoded, DecodeError> {
    let first = *code.get(pc).ok_or(DecodeError::Truncated)?;
    let op = Op::from_byte(first as u8).ok_or(DecodeError::UnknownOpcode(first as u8))?;
    if op != Op::Wide {
        let (operands, words) = decode_plain(op, code, pc)?;
        return Ok(Decoded {
            op,
            operands,
            words,
            wide: false,
        });
    }
    let follower_word = *code.get(pc + 1).ok_or(DecodeError::Truncated)?;
    let follower = Op::from_byte(follower_word as u8)
        .ok_or(DecodeError::UnknownOpcode(follower_word as u8))?;
    if !follower.wide_legal() {
        return Err(DecodeError::IllegalWide);
    }
    let (mut operands, words) = decode_plain(follower, code, pc + 1)?;
    for (column, &role) in follower.info().roles.iter().enumerate() {
        let high = (first >> SHIFT[column]) & 0xff;
        if high == 0 {
            continue;
        }
        if !role.is_widened() {
            return Err(DecodeError::IllegalWide);
        }
        operands.set(column, operands.get(column) | (high as i64) << 8);
    }
    Ok(Decoded {
        op: follower,
        operands,
        words: words + 1,
        wide: true,
    })
}

fn decode_plain(op: Op, code: &[u32], pc: usize) -> Result<(Operands, u32), DecodeError> {
    let info = op.info();
    let word = code[pc];
    let mut operands = Operands::default();
    let mut used = 0xffu32;
    for (column, &(shift, bits)) in layout(info.format).iter().enumerate() {
        if bits == 0 {
            continue;
        }
        let mask = (1u32 << bits) - 1;
        used |= mask << shift;
        let raw = (word >> shift) & mask;
        let role = info.roles[column];
        let value = if signed(role) {
            // Sign-extend from `bits`.
            ((raw << (32 - bits)) as i32 >> (32 - bits)) as i64
        } else {
            raw as i64
        };
        if role == Role::None && value != 0 {
            return Err(DecodeError::Reserved);
        }
        operands.set(column, value);
    }
    if word & !used != 0 {
        return Err(DecodeError::Reserved);
    }
    let words = info.format.words();
    if words == 2 {
        let ext = *code.get(pc + 1).ok_or(DecodeError::Truncated)?;
        operands.ext = if signed(info.format.ext_role()) {
            ext as i32 as i64
        } else {
            ext as i64
        };
    }
    Ok((operands, words))
}
