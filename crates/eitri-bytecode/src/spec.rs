//! The operation specification: every opcode variant of the
//! [typed bytecode](../../../docs/implementation/bytecode.md), as data.
//!
//! One row per variant gives its mnemonic, family, encoding format, the
//! role of each operand field, its structured semantics (operation,
//! overflow mode, signedness, width, precision, condition), its effects, and
//! whether it belongs to the first executable subset. Everything else is
//! derived from the table: the `Op` enum and its numbering (declaration
//! order, so assignments are deterministic), encoding and decoding, lengths,
//! `WIDE` legality, condition complements, and disassembly.
//!
//! Fault PCs: an instruction that faults reports its own PC, or the PC of
//! its `WIDE` prefix when it has one. A call does not fault itself for the
//! callee's faults; the callee's PC is innermost in the trace and the call
//! site appears as the next frame.

/// How an instruction's words are laid out. Bit 0 is the least significant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// `[ C:u8 | B:u8 | A:u8 | op ]`
    Abc,
    /// `[ X:u16 | A:u8 | op ]`; X takes the B column's role.
    ABx,
    /// `[ X:i16 | A:u8 | op ]`; X takes the B column's role.
    AsBx,
    /// `[ X:u24 | op ]`; X takes the A column's role.
    Ax,
    /// `[ X:i24 | op ]`; X takes the A column's role.
    SAx,
    /// `[ reserved:u24 | op ]` then an `i32` displacement word.
    JX,
    /// `[ reserved:u16 | A:u8 | op ]` then a `u32` constant index word.
    LdkX,
    /// `[ B:u8 | width:u8 | A:u8 | op ]` then a `u32` byte offset word; the
    /// width byte takes the C column's role.
    FieldX,
    /// The `WIDE` prefix: `[ C_hi:u8 | B_hi:u8 | A_hi:u8 | op ]`.
    Wide,
}

impl Format {
    /// Words in the instruction itself, not counting a `WIDE` prefix.
    pub const fn words(self) -> u32 {
        match self {
            Format::JX | Format::LdkX | Format::FieldX => 2,
            _ => 1,
        }
    }

    /// The role of the second word, for two-word forms.
    pub const fn ext_role(self) -> Role {
        match self {
            Format::JX => Role::Disp32,
            Format::LdkX => Role::Const32,
            Format::FieldX => Role::Offset32,
            _ => Role::None,
        }
    }
}

/// What an operand field means, and so its range and how it prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Unused: must be zero.
    None,
    /// A frame slot written.
    Dst,
    /// A frame slot read.
    Src,
    /// A frame slot read and written (a loop counter, `SEL`'s A).
    SrcDst,
    /// A signed 8-bit immediate.
    Imm8,
    /// An unsigned shift amount, checked against the width at compile time.
    Shift8,
    /// A signed word displacement from the end of the instruction.
    Disp8,
    Imm16,
    Disp16,
    Disp24,
    Disp32,
    /// A constant-pool index.
    Const16,
    Const32,
    /// A call-site descriptor index.
    Site16,
    /// A module-binding, capture, or layout index.
    Index8,
    /// An interpolation-plan index, widened like a slot but not a frame slot.
    FormatIndex,
    Index16,
    /// A field offset, scaled by the access width.
    Offset8,
    /// A byte offset.
    Offset32,
    /// Packed width and signedness of a conversion or wide field access.
    Width8,
}

impl Role {
    pub const fn is_slot(self) -> bool {
        matches!(self, Role::Dst | Role::Src | Role::SrcDst)
    }

    pub const fn is_widened(self) -> bool {
        self.is_slot() || matches!(self, Role::FormatIndex)
    }

    pub const fn is_displacement(self) -> bool {
        matches!(
            self,
            Role::Disp8 | Role::Disp16 | Role::Disp24 | Role::Disp32
        )
    }

    /// The inclusive range a field of this role encodes (slots: the short,
    /// un-widened range).
    pub const fn range(self) -> (i64, i64) {
        match self {
            Role::None => (0, 0),
            Role::Dst
            | Role::Src
            | Role::SrcDst
            | Role::Shift8
            | Role::Index8
            | Role::FormatIndex
            | Role::Offset8
            | Role::Width8 => (0, u8::MAX as i64),
            Role::Imm8 | Role::Disp8 => (i8::MIN as i64, i8::MAX as i64),
            Role::Imm16 | Role::Disp16 => (i16::MIN as i64, i16::MAX as i64),
            Role::Disp24 => (-(1 << 23), (1 << 23) - 1),
            Role::Disp32 => (i32::MIN as i64, i32::MAX as i64),
            Role::Const16 | Role::Site16 | Role::Index16 => (0, u16::MAX as i64),
            Role::Const32 | Role::Offset32 => (0, u32::MAX as i64),
        }
    }
}

/// Shift widths (and the width a narrow shift wraps to).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    W8,
    W16,
    W32,
    W64,
}

impl Width {
    pub const fn bits(self) -> u32 {
        match self {
            Width::W8 => 8,
            Width::W16 => 16,
            Width::W32 => 32,
            Width::W64 => 64,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    Xor,
    Shl(Width),
    Shr(Width),
    Neg,
    Not,
}

/// Overflow and signedness: present only where the bits differ (D30).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Two's complement wrap-around.
    Wrap,
    /// Faults when the result does not fit `i64`.
    CheckedSigned,
    /// Faults when the result does not fit `u64`.
    CheckedUnsigned,
    /// Signed division, remainder, or shift.
    Signed,
    Unsigned,
    /// Bitwise, or a 64-bit left shift: the same bits for both.
    Bits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    F32,
    F64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatOp {
    Rem,
    Add,
    Sub,
    Mul,
    Div,
    Neg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatConversion {
    IntToF32,
    IntToF64,
    F32ToInt,
    F64ToInt,
    F32ToF64,
    F64ToF32,
}

/// Integer conditions. `CB` and `CSET` use the first six (the others swap
/// operands); `CBI` cannot swap and uses `EQ NE LT GT LO HI`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    Eq,
    Ne,
    Lt,
    Le,
    /// Unsigned lower.
    Lo,
    /// Unsigned lower or same.
    Ls,
    Gt,
    /// Unsigned higher.
    Hi,
}

impl Cond {
    pub fn eval(self, a: i64, b: i64) -> bool {
        let (ua, ub) = (a as u64, b as u64);
        match self {
            Cond::Eq => a == b,
            Cond::Ne => a != b,
            Cond::Lt => a < b,
            Cond::Le => a <= b,
            Cond::Lo => ua < ub,
            Cond::Ls => ua <= ub,
            Cond::Gt => a > b,
            Cond::Hi => ua > ub,
        }
    }

    /// The condition true exactly when `self` is false, and whether its
    /// operands are swapped: `!(a < b)` is `b <= a`. Defined for the
    /// register-form conditions; `CBI` adjusts its immediate instead.
    pub fn complement(self) -> Option<(Cond, bool)> {
        Some(match self {
            Cond::Eq => (Cond::Ne, false),
            Cond::Ne => (Cond::Eq, false),
            Cond::Lt => (Cond::Le, true),
            Cond::Le => (Cond::Lt, true),
            Cond::Lo => (Cond::Ls, true),
            Cond::Ls => (Cond::Lo, true),
            Cond::Gt | Cond::Hi => return None,
        })
    }
}

/// Float conditions. `Ne`, `Ult`, and `Ule` are also true when either
/// operand is NaN (unordered); the others are false on NaN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatCond {
    Eq,
    Ne,
    Lt,
    Le,
    Ult,
    Ule,
}

impl FloatCond {
    pub fn eval(self, a: f64, b: f64) -> bool {
        let unordered = a.is_nan() || b.is_nan();
        match self {
            FloatCond::Eq => a == b,
            FloatCond::Ne => a != b,
            FloatCond::Lt => a < b,
            FloatCond::Le => a <= b,
            FloatCond::Ult => unordered || a < b,
            FloatCond::Ule => unordered || a <= b,
        }
    }

    /// As for [`Cond::complement`]. Swapping alone is not a complement for
    /// floats: on NaN both `a < b` and `b <= a` are false.
    pub fn complement(self) -> (FloatCond, bool) {
        match self {
            FloatCond::Eq => (FloatCond::Ne, false),
            FloatCond::Ne => (FloatCond::Eq, false),
            FloatCond::Lt => (FloatCond::Ule, true),
            FloatCond::Le => (FloatCond::Ult, true),
            FloatCond::Ult => (FloatCond::Le, true),
            FloatCond::Ule => (FloatCond::Lt, true),
        }
    }
}

/// What `TB` tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Test {
    True,
    False,
    None,
    Some,
}

impl Test {
    pub fn complement(self) -> Test {
        match self {
            Test::True => Test::False,
            Test::False => Test::True,
            Test::None => Test::Some,
            Test::Some => Test::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addr {
    Field,
    Index,
    Module,
    Captured,
    Aggregate,
}

/// Bytes read or written in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    B1,
    B2,
    B4,
    B8,
}

/// How a narrow load extends into its canonical slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extend {
    None,
    Sign,
    Zero,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    Script,
    Host,
    Value,
}

/// What an instruction does, structurally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sem {
    Move,
    MovePair,
    LoadImm,
    LoadConst,
    StringAlloc,
    RecordAlloc,
    RecordGet,
    RecordSet,
    RecordEq,
    RecordDeepEq,
    FixedGet,
    FixedSet,
    RecordNull,
    RecordWrap,
    RecordUnwrap,
    StringLen,
    StringConcat,
    StringCompare,
    StringJoin,
    StringSlice,
    StringTrim,
    StringCase,
    NullableNull,
    NullableWrap,
    NullableUnwrap,
    NullableEq,
    StringNextChar,
    StringNextLine,
    StringReplace,
    StringContains,
    StringStarts,
    StringEnds,
    StringFind,
    StringFormat(crate::ValueType),
    StringInterpolate,
    Int {
        op: IntOp,
        mode: Mode,
        imm: bool,
    },
    Convert {
        checked: bool,
    },
    Float {
        op: FloatOp,
        precision: Precision,
    },
    FloatConvert(FloatConversion),
    Branch {
        cond: Cond,
        imm: bool,
    },
    FloatBranch {
        cond: FloatCond,
        precision: Precision,
    },
    TestBranch(Test),
    Set(Cond),
    FloatSet {
        cond: FloatCond,
        precision: Precision,
    },
    Select,
    Jump,
    Loop {
        signed: bool,
        inclusive: bool,
    },
    Load {
        mode: Addr,
        access: Access,
        extend: Extend,
    },
    Store {
        mode: Addr,
        access: Access,
    },
    LoadFieldFar,
    StoreFieldFar,
    Call(CallKind),
    Return,
    Trap,
    Wide,
}

/// What an instruction may do besides writing its destination slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effects(u8);

impl Effects {
    pub const fn contains(self, other: Effects) -> bool {
        self.0 & other.0 == other.0
    }
}

pub const NONE: Effects = Effects(0);
/// May fault, at this instruction's PC.
pub const FAULT: Effects = Effects(1);
/// May transfer control.
pub const BRANCH: Effects = Effects(1 << 1);
/// Polls the execution budget when it calls, or when it branches backward.
pub const POLL: Effects = Effects(1 << 2);
/// Calls a function.
pub const CALL: Effects = Effects(1 << 3);
/// Never falls through.
pub const END: Effects = Effects(1 << 4);
/// Reads the constant pool.
pub const CONST: Effects = Effects(1 << 5);
/// Reads memory outside the frame.
pub const READ: Effects = Effects(1 << 6);
/// Writes memory outside the frame.
pub const WRITE: Effects = Effects(1 << 7);

/// Allocation or host boundary relevant to root publication. This describes
/// opcode intent; helper-level allocation and GC polls need a separate audit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocationClass {
    None,
    MayGrowVm,
    HostBoundary,
    Managed,
}

/// One row of the specification.
#[derive(Clone, Copy, Debug)]
pub struct OpInfo {
    pub name: &'static str,
    pub family: &'static str,
    pub format: Format,
    /// Roles of the A, B, C columns; see [`Format`] for where X lives.
    pub roles: [Role; 3],
    pub sem: Sem,
    pub effects: Effects,
    /// In the first executable subset. Others can be encoded and printed,
    /// but the verifier rejects them before anything runs.
    pub implemented: bool,
}

macro_rules! ops {
    ($(
        $variant:ident = $name:literal, $family:literal, $format:ident,
        [$a:ident, $b:ident, $c:ident], $sem:expr, $($effect:ident)|+, $implemented:literal, $allocation:ident;
    )*) => {
        /// An opcode: the family plus its dispatch fields. Its value is its
        /// position in the specification table.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Op {
            $($variant,)*
        }

        /// The specification table, indexed by opcode.
        pub static OPS: &[OpInfo] = &[$(OpInfo {
            name: $name,
            family: $family,
            format: Format::$format,
            roles: [Role::$a, Role::$b, Role::$c],
            sem: $sem,
            effects: Effects(0 $(| $effect.0)+),
            implemented: $implemented,
        },)*];

        /// Allocation classes are kept separate from the packed Effects bits.
        pub static ALLOCATION_CLASSES: &[AllocationClass] = &[$(AllocationClass::$allocation,)*];

        impl Op {
            pub fn allocation_class(self) -> AllocationClass {
                ALLOCATION_CLASSES[self as usize]
            }
            /// Every opcode, in numbering order.
            pub const ALL: &[Op] = &[$(Op::$variant,)*];
        }
    };
}

ops! {
    Mov = "MOV", "MOV", Abc, [Dst, Src, None], Sem::Move, NONE, true, None;
    Mov2 = "MOV2", "MOV2", Abc, [Dst, Src, Src], Sem::MovePair, NONE, false, None;
    Ldi = "LDI", "LDI", AsBx, [Dst, Imm16, None], Sem::LoadImm, NONE, true, None;
    Ldk = "LDK", "LDK", ABx, [Dst, Const16, None], Sem::LoadConst, CONST, true, None;
    AddWrap = "ADD.WRAP", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Add, mode: Mode::Wrap, imm: false }, NONE, true, None;
    AddChkS = "ADD.CHK.S", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Add, mode: Mode::CheckedSigned, imm: false }, FAULT, true, None;
    AddChkU = "ADD.CHK.U", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Add, mode: Mode::CheckedUnsigned, imm: false }, FAULT, true, None;
    SubWrap = "SUB.WRAP", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Sub, mode: Mode::Wrap, imm: false }, NONE, true, None;
    SubChkS = "SUB.CHK.S", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Sub, mode: Mode::CheckedSigned, imm: false }, FAULT, true, None;
    SubChkU = "SUB.CHK.U", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Sub, mode: Mode::CheckedUnsigned, imm: false }, FAULT, true, None;
    MulWrap = "MUL.WRAP", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Mul, mode: Mode::Wrap, imm: false }, NONE, true, None;
    MulChkS = "MUL.CHK.S", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Mul, mode: Mode::CheckedSigned, imm: false }, FAULT, true, None;
    MulChkU = "MUL.CHK.U", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Mul, mode: Mode::CheckedUnsigned, imm: false }, FAULT, true, None;
    DivS = "DIV.S", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Div, mode: Mode::Signed, imm: false }, FAULT, true, None;
    DivU = "DIV.U", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Div, mode: Mode::Unsigned, imm: false }, FAULT, true, None;
    RemS = "REM.S", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Rem, mode: Mode::Signed, imm: false }, FAULT, true, None;
    RemU = "REM.U", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Rem, mode: Mode::Unsigned, imm: false }, FAULT, true, None;
    And = "AND", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::And, mode: Mode::Bits, imm: false }, NONE, true, None;
    Or = "OR", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Or, mode: Mode::Bits, imm: false }, NONE, true, None;
    Xor = "XOR", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Xor, mode: Mode::Bits, imm: false }, NONE, true, None;
    ShlS8 = "SHL.S8", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shl(Width::W8), mode: Mode::Signed, imm: false }, FAULT, true, None;
    ShlS16 = "SHL.S16", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shl(Width::W16), mode: Mode::Signed, imm: false }, FAULT, false, None;
    ShlS32 = "SHL.S32", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shl(Width::W32), mode: Mode::Signed, imm: false }, FAULT, false, None;
    ShlU8 = "SHL.U8", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shl(Width::W8), mode: Mode::Unsigned, imm: false }, FAULT, true, None;
    ShlU16 = "SHL.U16", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shl(Width::W16), mode: Mode::Unsigned, imm: false }, FAULT, false, None;
    ShlU32 = "SHL.U32", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shl(Width::W32), mode: Mode::Unsigned, imm: false }, FAULT, false, None;
    Shl64 = "SHL.64", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shl(Width::W64), mode: Mode::Bits, imm: false }, FAULT, true, None;
    ShrS8 = "SHR.S8", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shr(Width::W8), mode: Mode::Signed, imm: false }, FAULT, true, None;
    ShrS16 = "SHR.S16", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shr(Width::W16), mode: Mode::Signed, imm: false }, FAULT, false, None;
    ShrS32 = "SHR.S32", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shr(Width::W32), mode: Mode::Signed, imm: false }, FAULT, false, None;
    ShrS64 = "SHR.S64", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shr(Width::W64), mode: Mode::Signed, imm: false }, FAULT, true, None;
    ShrU8 = "SHR.U8", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shr(Width::W8), mode: Mode::Unsigned, imm: false }, FAULT, true, None;
    ShrU16 = "SHR.U16", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shr(Width::W16), mode: Mode::Unsigned, imm: false }, FAULT, false, None;
    ShrU32 = "SHR.U32", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shr(Width::W32), mode: Mode::Unsigned, imm: false }, FAULT, false, None;
    ShrU64 = "SHR.U64", "ALU", Abc, [Dst, Src, Src], Sem::Int { op: IntOp::Shr(Width::W64), mode: Mode::Unsigned, imm: false }, FAULT, true, None;
    AddiWrap = "ADDI.WRAP", "ALUI", Abc, [Dst, Src, Imm8], Sem::Int { op: IntOp::Add, mode: Mode::Wrap, imm: true }, NONE, true, None;
    AddiChkS = "ADDI.CHK.S", "ALUI", Abc, [Dst, Src, Imm8], Sem::Int { op: IntOp::Add, mode: Mode::CheckedSigned, imm: true }, FAULT, true, None;
    SubiWrap = "SUBI.WRAP", "ALUI", Abc, [Dst, Src, Imm8], Sem::Int { op: IntOp::Sub, mode: Mode::Wrap, imm: true }, NONE, true, None;
    SubiChkS = "SUBI.CHK.S", "ALUI", Abc, [Dst, Src, Imm8], Sem::Int { op: IntOp::Sub, mode: Mode::CheckedSigned, imm: true }, FAULT, true, None;
    AddiChkU = "ADDI.CHK.U", "ALUI", Abc, [Dst, Src, Imm8], Sem::Int { op: IntOp::Add, mode: Mode::CheckedUnsigned, imm: true }, FAULT, true, None;
    MuliChkS = "MULI.CHK.S", "ALUI", Abc, [Dst, Src, Imm8], Sem::Int { op: IntOp::Mul, mode: Mode::CheckedSigned, imm: true }, FAULT, true, None;
    Andi = "ANDI", "ALUI", Abc, [Dst, Src, Imm8], Sem::Int { op: IntOp::And, mode: Mode::Bits, imm: true }, NONE, true, None;
    ShliS8 = "SHLI.S8", "ALUI", Abc, [Dst, Src, Shift8], Sem::Int { op: IntOp::Shl(Width::W8), mode: Mode::Signed, imm: true }, NONE, true, None;
    ShliS16 = "SHLI.S16", "ALUI", Abc, [Dst, Src, Shift8], Sem::Int { op: IntOp::Shl(Width::W16), mode: Mode::Signed, imm: true }, NONE, false, None;
    ShliS32 = "SHLI.S32", "ALUI", Abc, [Dst, Src, Shift8], Sem::Int { op: IntOp::Shl(Width::W32), mode: Mode::Signed, imm: true }, NONE, false, None;
    ShliU8 = "SHLI.U8", "ALUI", Abc, [Dst, Src, Shift8], Sem::Int { op: IntOp::Shl(Width::W8), mode: Mode::Unsigned, imm: true }, NONE, true, None;
    ShliU16 = "SHLI.U16", "ALUI", Abc, [Dst, Src, Shift8], Sem::Int { op: IntOp::Shl(Width::W16), mode: Mode::Unsigned, imm: true }, NONE, false, None;
    ShliU32 = "SHLI.U32", "ALUI", Abc, [Dst, Src, Shift8], Sem::Int { op: IntOp::Shl(Width::W32), mode: Mode::Unsigned, imm: true }, NONE, false, None;
    Shli64 = "SHLI.64", "ALUI", Abc, [Dst, Src, Shift8], Sem::Int { op: IntOp::Shl(Width::W64), mode: Mode::Bits, imm: true }, NONE, true, None;
    ShriS = "SHRI.S", "ALUI", Abc, [Dst, Src, Shift8], Sem::Int { op: IntOp::Shr(Width::W64), mode: Mode::Signed, imm: true }, NONE, true, None;
    ShriU = "SHRI.U", "ALUI", Abc, [Dst, Src, Shift8], Sem::Int { op: IntOp::Shr(Width::W64), mode: Mode::Unsigned, imm: true }, NONE, true, None;
    NegWrap = "NEG.WRAP", "UN", Abc, [Dst, Src, None], Sem::Int { op: IntOp::Neg, mode: Mode::Wrap, imm: false }, NONE, true, None;
    NegChkS = "NEG.CHK.S", "UN", Abc, [Dst, Src, None], Sem::Int { op: IntOp::Neg, mode: Mode::CheckedSigned, imm: false }, FAULT, true, None;
    NegChkU = "NEG.CHK.U", "UN", Abc, [Dst, Src, None], Sem::Int { op: IntOp::Neg, mode: Mode::CheckedUnsigned, imm: false }, FAULT, true, None;
    Not = "NOT", "UN", Abc, [Dst, Src, None], Sem::Int { op: IntOp::Not, mode: Mode::Bits, imm: false }, NONE, true, None;
    CvtWrap = "CVT.WRAP", "CVT", Abc, [Dst, Src, Width8], Sem::Convert { checked: false }, NONE, true, None;
    CvtChk = "CVT.CHK", "CVT", Abc, [Dst, Src, Width8], Sem::Convert { checked: true }, FAULT, true, None;
    FaddF32 = "FADD.F32", "FALU", Abc, [Dst, Src, Src], Sem::Float { op: FloatOp::Add, precision: Precision::F32 }, NONE, true, None;
    FaddF64 = "FADD.F64", "FALU", Abc, [Dst, Src, Src], Sem::Float { op: FloatOp::Add, precision: Precision::F64 }, NONE, true, None;
    FsubF32 = "FSUB.F32", "FALU", Abc, [Dst, Src, Src], Sem::Float { op: FloatOp::Sub, precision: Precision::F32 }, NONE, true, None;
    FsubF64 = "FSUB.F64", "FALU", Abc, [Dst, Src, Src], Sem::Float { op: FloatOp::Sub, precision: Precision::F64 }, NONE, true, None;
    FmulF32 = "FMUL.F32", "FALU", Abc, [Dst, Src, Src], Sem::Float { op: FloatOp::Mul, precision: Precision::F32 }, NONE, true, None;
    FmulF64 = "FMUL.F64", "FALU", Abc, [Dst, Src, Src], Sem::Float { op: FloatOp::Mul, precision: Precision::F64 }, NONE, true, None;
    FdivF32 = "FDIV.F32", "FALU", Abc, [Dst, Src, Src], Sem::Float { op: FloatOp::Div, precision: Precision::F32 }, NONE, true, None;
    FdivF64 = "FDIV.F64", "FALU", Abc, [Dst, Src, Src], Sem::Float { op: FloatOp::Div, precision: Precision::F64 }, NONE, true, None;
    FnegF32 = "FNEG.F32", "FALU", Abc, [Dst, Src, None], Sem::Float { op: FloatOp::Neg, precision: Precision::F32 }, NONE, true, None;
    FnegF64 = "FNEG.F64", "FALU", Abc, [Dst, Src, None], Sem::Float { op: FloatOp::Neg, precision: Precision::F64 }, NONE, true, None;
    FcvtI2f32 = "FCVT.I2F32", "FCVT", Abc, [Dst, Src, Width8], Sem::FloatConvert(FloatConversion::IntToF32), NONE, true, None;
    FcvtI2f64 = "FCVT.I2F64", "FCVT", Abc, [Dst, Src, Width8], Sem::FloatConvert(FloatConversion::IntToF64), NONE, true, None;
    FcvtF32i = "FCVT.F32I", "FCVT", Abc, [Dst, Src, Width8], Sem::FloatConvert(FloatConversion::F32ToInt), FAULT, true, None;
    FcvtF64i = "FCVT.F64I", "FCVT", Abc, [Dst, Src, Width8], Sem::FloatConvert(FloatConversion::F64ToInt), FAULT, true, None;
    FcvtF32f64 = "FCVT.F32F64", "FCVT", Abc, [Dst, Src, Width8], Sem::FloatConvert(FloatConversion::F32ToF64), NONE, true, None;
    FcvtF64f32 = "FCVT.F64F32", "FCVT", Abc, [Dst, Src, Width8], Sem::FloatConvert(FloatConversion::F64ToF32), NONE, true, None;
    CbEq = "CB.EQ", "CB", Abc, [Src, Src, Disp8], Sem::Branch { cond: Cond::Eq, imm: false }, BRANCH | POLL, true, None;
    CbNe = "CB.NE", "CB", Abc, [Src, Src, Disp8], Sem::Branch { cond: Cond::Ne, imm: false }, BRANCH | POLL, true, None;
    CbLt = "CB.LT", "CB", Abc, [Src, Src, Disp8], Sem::Branch { cond: Cond::Lt, imm: false }, BRANCH | POLL, true, None;
    CbLe = "CB.LE", "CB", Abc, [Src, Src, Disp8], Sem::Branch { cond: Cond::Le, imm: false }, BRANCH | POLL, true, None;
    CbLo = "CB.LO", "CB", Abc, [Src, Src, Disp8], Sem::Branch { cond: Cond::Lo, imm: false }, BRANCH | POLL, true, None;
    CbLs = "CB.LS", "CB", Abc, [Src, Src, Disp8], Sem::Branch { cond: Cond::Ls, imm: false }, BRANCH | POLL, true, None;
    CbiEq = "CBI.EQ", "CBI", Abc, [Src, Imm8, Disp8], Sem::Branch { cond: Cond::Eq, imm: true }, BRANCH | POLL, true, None;
    CbiNe = "CBI.NE", "CBI", Abc, [Src, Imm8, Disp8], Sem::Branch { cond: Cond::Ne, imm: true }, BRANCH | POLL, true, None;
    CbiLt = "CBI.LT", "CBI", Abc, [Src, Imm8, Disp8], Sem::Branch { cond: Cond::Lt, imm: true }, BRANCH | POLL, true, None;
    CbiGt = "CBI.GT", "CBI", Abc, [Src, Imm8, Disp8], Sem::Branch { cond: Cond::Gt, imm: true }, BRANCH | POLL, true, None;
    CbiLo = "CBI.LO", "CBI", Abc, [Src, Imm8, Disp8], Sem::Branch { cond: Cond::Lo, imm: true }, BRANCH | POLL, true, None;
    CbiHi = "CBI.HI", "CBI", Abc, [Src, Imm8, Disp8], Sem::Branch { cond: Cond::Hi, imm: true }, BRANCH | POLL, true, None;
    FcbFeqF32 = "FCB.FEQ.F32", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Eq, precision: Precision::F32 }, BRANCH | POLL, false, None;
    FcbFneF32 = "FCB.FNE.F32", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Ne, precision: Precision::F32 }, BRANCH | POLL, false, None;
    FcbFltF32 = "FCB.FLT.F32", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Lt, precision: Precision::F32 }, BRANCH | POLL, false, None;
    FcbFleF32 = "FCB.FLE.F32", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Le, precision: Precision::F32 }, BRANCH | POLL, false, None;
    FcbFultF32 = "FCB.FULT.F32", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Ult, precision: Precision::F32 }, BRANCH | POLL, false, None;
    FcbFuleF32 = "FCB.FULE.F32", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Ule, precision: Precision::F32 }, BRANCH | POLL, false, None;
    FcbFeqF64 = "FCB.FEQ.F64", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Eq, precision: Precision::F64 }, BRANCH | POLL, false, None;
    FcbFneF64 = "FCB.FNE.F64", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Ne, precision: Precision::F64 }, BRANCH | POLL, false, None;
    FcbFltF64 = "FCB.FLT.F64", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Lt, precision: Precision::F64 }, BRANCH | POLL, false, None;
    FcbFleF64 = "FCB.FLE.F64", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Le, precision: Precision::F64 }, BRANCH | POLL, false, None;
    FcbFultF64 = "FCB.FULT.F64", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Ult, precision: Precision::F64 }, BRANCH | POLL, false, None;
    FcbFuleF64 = "FCB.FULE.F64", "FCB", Abc, [Src, Src, Disp8], Sem::FloatBranch { cond: FloatCond::Ule, precision: Precision::F64 }, BRANCH | POLL, false, None;
    TbTrue = "TB.TRUE", "TB", AsBx, [Src, Disp16, None], Sem::TestBranch(Test::True), BRANCH | POLL, true, None;
    TbFalse = "TB.FALSE", "TB", AsBx, [Src, Disp16, None], Sem::TestBranch(Test::False), BRANCH | POLL, true, None;
    TbNone = "TB.NONE", "TB", AsBx, [Src, Disp16, None], Sem::TestBranch(Test::None), BRANCH | POLL, false, None;
    TbSome = "TB.SOME", "TB", AsBx, [Src, Disp16, None], Sem::TestBranch(Test::Some), BRANCH | POLL, false, None;
    CsetEq = "CSET.EQ", "CSET", Abc, [Dst, Src, Src], Sem::Set(Cond::Eq), NONE, true, None;
    CsetNe = "CSET.NE", "CSET", Abc, [Dst, Src, Src], Sem::Set(Cond::Ne), NONE, true, None;
    CsetLt = "CSET.LT", "CSET", Abc, [Dst, Src, Src], Sem::Set(Cond::Lt), NONE, true, None;
    CsetLe = "CSET.LE", "CSET", Abc, [Dst, Src, Src], Sem::Set(Cond::Le), NONE, true, None;
    CsetLo = "CSET.LO", "CSET", Abc, [Dst, Src, Src], Sem::Set(Cond::Lo), NONE, true, None;
    CsetLs = "CSET.LS", "CSET", Abc, [Dst, Src, Src], Sem::Set(Cond::Ls), NONE, true, None;
    FcsetFeqF32 = "FCSET.FEQ.F32", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Eq, precision: Precision::F32 }, NONE, true, None;
    FcsetFneF32 = "FCSET.FNE.F32", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Ne, precision: Precision::F32 }, NONE, true, None;
    FcsetFltF32 = "FCSET.FLT.F32", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Lt, precision: Precision::F32 }, NONE, true, None;
    FcsetFleF32 = "FCSET.FLE.F32", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Le, precision: Precision::F32 }, NONE, true, None;
    FcsetFultF32 = "FCSET.FULT.F32", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Ult, precision: Precision::F32 }, NONE, true, None;
    FcsetFuleF32 = "FCSET.FULE.F32", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Ule, precision: Precision::F32 }, NONE, true, None;
    FcsetFeqF64 = "FCSET.FEQ.F64", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Eq, precision: Precision::F64 }, NONE, true, None;
    FcsetFneF64 = "FCSET.FNE.F64", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Ne, precision: Precision::F64 }, NONE, true, None;
    FcsetFltF64 = "FCSET.FLT.F64", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Lt, precision: Precision::F64 }, NONE, true, None;
    FcsetFleF64 = "FCSET.FLE.F64", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Le, precision: Precision::F64 }, NONE, true, None;
    FcsetFultF64 = "FCSET.FULT.F64", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Ult, precision: Precision::F64 }, NONE, true, None;
    FcsetFuleF64 = "FCSET.FULE.F64", "FCSET", Abc, [Dst, Src, Src], Sem::FloatSet { cond: FloatCond::Ule, precision: Precision::F64 }, NONE, true, None;
    Sel = "SEL", "SEL", Abc, [SrcDst, Src, Src], Sem::Select, NONE, false, None;
    J = "J", "J", SAx, [Disp24, None, None], Sem::Jump, BRANCH | POLL | END, true, None;
    JX = "J_X", "J_X", JX, [None, None, None], Sem::Jump, BRANCH | POLL | END, true, None;
    LoopSEx = "LOOP.S.EX", "LOOP", AsBx, [SrcDst, Disp16, None], Sem::Loop { signed: true, inclusive: false }, BRANCH | POLL, false, None;
    LoopSIn = "LOOP.S.IN", "LOOP", AsBx, [SrcDst, Disp16, None], Sem::Loop { signed: true, inclusive: true }, BRANCH | POLL, false, None;
    LoopUEx = "LOOP.U.EX", "LOOP", AsBx, [SrcDst, Disp16, None], Sem::Loop { signed: false, inclusive: false }, BRANCH | POLL, false, None;
    LoopUIn = "LOOP.U.IN", "LOOP", AsBx, [SrcDst, Disp16, None], Sem::Loop { signed: false, inclusive: true }, BRANCH | POLL, false, None;
    Ldf8 = "LDF.8", "LD", Abc, [Dst, Src, Offset8], Sem::Load { mode: Addr::Field, access: Access::B8, extend: Extend::None }, READ | FAULT, false, None;
    LdfS4 = "LDF.S4", "LD", Abc, [Dst, Src, Offset8], Sem::Load { mode: Addr::Field, access: Access::B4, extend: Extend::Sign }, READ | FAULT, false, None;
    LdfS2 = "LDF.S2", "LD", Abc, [Dst, Src, Offset8], Sem::Load { mode: Addr::Field, access: Access::B2, extend: Extend::Sign }, READ | FAULT, false, None;
    LdfS1 = "LDF.S1", "LD", Abc, [Dst, Src, Offset8], Sem::Load { mode: Addr::Field, access: Access::B1, extend: Extend::Sign }, READ | FAULT, false, None;
    LdfU4 = "LDF.U4", "LD", Abc, [Dst, Src, Offset8], Sem::Load { mode: Addr::Field, access: Access::B4, extend: Extend::Zero }, READ | FAULT, false, None;
    LdfU2 = "LDF.U2", "LD", Abc, [Dst, Src, Offset8], Sem::Load { mode: Addr::Field, access: Access::B2, extend: Extend::Zero }, READ | FAULT, false, None;
    LdfU1 = "LDF.U1", "LD", Abc, [Dst, Src, Offset8], Sem::Load { mode: Addr::Field, access: Access::B1, extend: Extend::Zero }, READ | FAULT, false, None;
    Ldx8 = "LDX.8", "LD", Abc, [Dst, Src, Src], Sem::Load { mode: Addr::Index, access: Access::B8, extend: Extend::None }, READ | FAULT, false, None;
    LdxS4 = "LDX.S4", "LD", Abc, [Dst, Src, Src], Sem::Load { mode: Addr::Index, access: Access::B4, extend: Extend::Sign }, READ | FAULT, false, None;
    LdxS2 = "LDX.S2", "LD", Abc, [Dst, Src, Src], Sem::Load { mode: Addr::Index, access: Access::B2, extend: Extend::Sign }, READ | FAULT, false, None;
    LdxS1 = "LDX.S1", "LD", Abc, [Dst, Src, Src], Sem::Load { mode: Addr::Index, access: Access::B1, extend: Extend::Sign }, READ | FAULT, false, None;
    LdxU4 = "LDX.U4", "LD", Abc, [Dst, Src, Src], Sem::Load { mode: Addr::Index, access: Access::B4, extend: Extend::Zero }, READ | FAULT, false, None;
    LdxU2 = "LDX.U2", "LD", Abc, [Dst, Src, Src], Sem::Load { mode: Addr::Index, access: Access::B2, extend: Extend::Zero }, READ | FAULT, false, None;
    LdxU1 = "LDX.U1", "LD", Abc, [Dst, Src, Src], Sem::Load { mode: Addr::Index, access: Access::B1, extend: Extend::Zero }, READ | FAULT, false, None;
    Ldm = "LDM", "LD", ABx, [Dst, Index16, None], Sem::Load { mode: Addr::Module, access: Access::B8, extend: Extend::None }, READ, false, None;
    Ldc = "LDC", "LD", ABx, [Dst, Index16, None], Sem::Load { mode: Addr::Captured, access: Access::B8, extend: Extend::None }, READ, false, None;
    Lda = "LDA", "LD", Abc, [Dst, Src, Index8], Sem::Load { mode: Addr::Aggregate, access: Access::B8, extend: Extend::None }, READ | FAULT, false, None;
    Stf8 = "STF.8", "ST", Abc, [Src, Src, Offset8], Sem::Store { mode: Addr::Field, access: Access::B8 }, WRITE | FAULT, false, None;
    Stf4 = "STF.4", "ST", Abc, [Src, Src, Offset8], Sem::Store { mode: Addr::Field, access: Access::B4 }, WRITE | FAULT, false, None;
    Stf2 = "STF.2", "ST", Abc, [Src, Src, Offset8], Sem::Store { mode: Addr::Field, access: Access::B2 }, WRITE | FAULT, false, None;
    Stf1 = "STF.1", "ST", Abc, [Src, Src, Offset8], Sem::Store { mode: Addr::Field, access: Access::B1 }, WRITE | FAULT, false, None;
    Stx8 = "STX.8", "ST", Abc, [Src, Src, Src], Sem::Store { mode: Addr::Index, access: Access::B8 }, WRITE | FAULT, false, None;
    Stx4 = "STX.4", "ST", Abc, [Src, Src, Src], Sem::Store { mode: Addr::Index, access: Access::B4 }, WRITE | FAULT, false, None;
    Stx2 = "STX.2", "ST", Abc, [Src, Src, Src], Sem::Store { mode: Addr::Index, access: Access::B2 }, WRITE | FAULT, false, None;
    Stx1 = "STX.1", "ST", Abc, [Src, Src, Src], Sem::Store { mode: Addr::Index, access: Access::B1 }, WRITE | FAULT, false, None;
    Stm = "STM", "ST", ABx, [Src, Index16, None], Sem::Store { mode: Addr::Module, access: Access::B8 }, WRITE, false, None;
    Stc = "STC", "ST", ABx, [Src, Index16, None], Sem::Store { mode: Addr::Captured, access: Access::B8 }, WRITE, false, None;
    Sta = "STA", "ST", Abc, [Src, Src, Index8], Sem::Store { mode: Addr::Aggregate, access: Access::B8 }, WRITE | FAULT, false, None;
    LdkX = "LDK_X", "LDK_X", LdkX, [Dst, None, None], Sem::LoadConst, CONST, true, None;
    LdfX = "LDF_X", "LDF_X", FieldX, [Dst, Src, Width8], Sem::LoadFieldFar, READ | FAULT, false, None;
    StfX = "STF_X", "STF_X", FieldX, [Src, Src, Width8], Sem::StoreFieldFar, WRITE | FAULT, false, None;
    CallScript = "CALL.SCRIPT", "CALL", ABx, [None, Site16, None], Sem::Call(CallKind::Script), CALL | FAULT | POLL, true, MayGrowVm;
    CallHost = "CALL.HOST", "CALL", ABx, [None, Site16, None], Sem::Call(CallKind::Host), CALL | FAULT | POLL, true, HostBoundary;
    CallValue = "CALL.VALUE", "CALL", ABx, [None, Site16, None], Sem::Call(CallKind::Value), CALL | FAULT | POLL, false, HostBoundary;
    Ret = "RET", "RET", Ax, [None, None, None], Sem::Return, END, true, None;
    Trap = "TRAP", "TRAP", ABx, [Src, Site16, None], Sem::Trap, FAULT | END, false, None;
    Wide = "WIDE", "WIDE", Wide, [None, None, None], Sem::Wide, NONE, true, None;
    FremF32 = "FREM.F32", "FALU", Abc, [Dst, Src, Src], Sem::Float { op: FloatOp::Rem, precision: Precision::F32 }, NONE, true, None;
    FremF64 = "FREM.F64", "FALU", Abc, [Dst, Src, Src], Sem::Float { op: FloatOp::Rem, precision: Precision::F64 }, NONE, true, None;
    StrNew = "STR.NEW", "STR", ABx, [Dst, Index16, None], Sem::StringAlloc, FAULT, true, Managed;
    StrLen = "STR.LEN", "STR", Abc, [Dst, Src, None], Sem::StringLen, NONE, true, None;
    StrConcat = "STR.CONCAT", "STR", Abc, [Dst, Src, Src], Sem::StringConcat, FAULT, true, Managed;
    StrCompare = "STR.COMPARE", "STR", Abc, [Dst, Src, Src], Sem::StringCompare, NONE, true, None;
    StrJoin = "STR.JOIN", "STR", Abc, [Dst, Src, Src], Sem::StringJoin, FAULT, true, Managed;
    NullableEq = "NULLABLE.EQ", "STR", Abc, [Dst, Src, Src], Sem::NullableEq, NONE, true, None;
    NullableNull = "NULLABLE.NULL", "STR", Abc, [Dst, None, None], Sem::NullableNull, FAULT, true, Managed;
    NullableWrap = "NULLABLE.WRAP", "STR", Abc, [Dst, Src, None], Sem::NullableWrap, NONE, true, None;
    NullableUnwrap = "NULLABLE.UNWRAP", "STR", Abc, [Dst, Src, None], Sem::NullableUnwrap, FAULT, true, None;
    StrNextChar = "STR.NEXT.CHAR", "STR", Abc, [Dst, Src, SrcDst], Sem::StringNextChar, FAULT, true, None;
    StrNextLine = "STR.NEXT.LINE", "STR", Abc, [Dst, Src, SrcDst], Sem::StringNextLine, FAULT, true, Managed;
    StrUpper = "STR.UPPER", "STR", Abc, [Dst, Src, None], Sem::StringCase, FAULT, true, Managed;
    StrLower = "STR.LOWER", "STR", Abc, [Dst, Src, None], Sem::StringCase, FAULT, true, Managed;
    StrReplace = "STR.REPLACE", "STR", Abc, [Dst, Src, None], Sem::StringReplace, FAULT, true, Managed;
    StrTrim = "STR.TRIM", "STR", Abc, [Dst, Src, None], Sem::StringTrim, FAULT, true, Managed;
    StrTrimStart = "STR.TRIM.START", "STR", Abc, [Dst, Src, None], Sem::StringTrim, FAULT, true, Managed;
    StrTrimEnd = "STR.TRIM.END", "STR", Abc, [Dst, Src, None], Sem::StringTrim, FAULT, true, Managed;
    StrSlice = "STR.SLICE", "STR", Abc, [Dst, Src, None], Sem::StringSlice, FAULT, true, Managed;
    StrContains = "STR.CONTAINS", "STR", Abc, [Dst, Src, Src], Sem::StringContains, NONE, true, None;
    StrStarts = "STR.STARTS", "STR", Abc, [Dst, Src, Src], Sem::StringStarts, NONE, true, None;
    StrEnds = "STR.ENDS", "STR", Abc, [Dst, Src, Src], Sem::StringEnds, NONE, true, None;
    StrFind = "STR.FIND", "STR", Abc, [Dst, Src, Src], Sem::StringFind, NONE, true, None;
    StrFmtI64 = "STR.FMT.I64", "STR", Abc, [Dst, Src, None], Sem::StringFormat(crate::ValueType::I64), FAULT, true, Managed;
    StrFmtI8 = "STR.FMT.I8", "STR", Abc, [Dst, Src, None], Sem::StringFormat(crate::ValueType::I8), FAULT, true, Managed;
    StrFmtU8 = "STR.FMT.U8", "STR", Abc, [Dst, Src, None], Sem::StringFormat(crate::ValueType::U8), FAULT, true, Managed;
    StrFmtU64 = "STR.FMT.U64", "STR", Abc, [Dst, Src, None], Sem::StringFormat(crate::ValueType::U64), FAULT, true, Managed;
    StrFmtF32 = "STR.FMT.F32", "STR", Abc, [Dst, Src, None], Sem::StringFormat(crate::ValueType::F32), FAULT, true, Managed;
    StrFmtF64 = "STR.FMT.F64", "STR", Abc, [Dst, Src, None], Sem::StringFormat(crate::ValueType::F64), FAULT, true, Managed;
    StrFmtBool = "STR.FMT.BOOL", "STR", Abc, [Dst, Src, None], Sem::StringFormat(crate::ValueType::Bool), FAULT, true, Managed;
    StrFmtChar = "STR.FMT.CHAR", "STR", Abc, [Dst, Src, None], Sem::StringFormat(crate::ValueType::Char), FAULT, true, Managed;
    StrInterpolate = "STR.INTERPOLATE", "STR", Abc, [Dst, Src, FormatIndex], Sem::StringInterpolate, FAULT, true, Managed;
    RecordNew = "RECORD.NEW", "RECORD", Abc, [Dst, Src, Index8], Sem::RecordAlloc, FAULT, true, Managed;
    RecordGet = "RECORD.GET", "RECORD", Abc, [Dst, Src, Index8], Sem::RecordGet, FAULT, true, None;
    RecordSet = "RECORD.SET", "RECORD", Abc, [Src, Src, Index8], Sem::RecordSet, FAULT, true, None;
    RecordNull = "RECORD.NULL", "RECORD", Abc, [Dst, None, Index8], Sem::RecordNull, FAULT, true, Managed;
    RecordWrap = "RECORD.WRAP", "RECORD", Abc, [Dst, Src, None], Sem::RecordWrap, NONE, true, None;
    RecordUnwrap = "RECORD.UNWRAP", "RECORD", Abc, [Dst, Src, None], Sem::RecordUnwrap, FAULT, true, None;
    RecordEq = "RECORD.EQ", "RECORD", Abc, [Dst, Src, Src], Sem::RecordEq, NONE, true, None;
    FixedGet = "FIXED.GET", "FIXED", Abc, [Dst, Src, Index8], Sem::FixedGet, FAULT, true, None;
    RecordDeepEq = "RECORD.DEEP.EQ", "RECORD", Abc, [Dst, Src, Src], Sem::RecordDeepEq, FAULT, true, MayGrowVm;
    FixedSet = "FIXED.SET", "FIXED", Abc, [Src, Src, Index8], Sem::FixedSet, FAULT, true, None;

}

/// The opcode byte has room for 256 handlers.
const _: () = assert!(Op::ALL.len() <= 256);

impl Op {
    pub fn info(self) -> &'static OpInfo {
        &OPS[self as usize]
    }

    pub fn from_byte(byte: u8) -> Option<Op> {
        Op::ALL.get(byte as usize).copied()
    }

    pub fn name(self) -> &'static str {
        self.info().name
    }

    /// Whether a `WIDE` prefix may precede this instruction: it has a slot
    /// field to widen, and it is not `WIDE`, a jump, a call, or a return.
    pub fn wide_legal(self) -> bool {
        let info = self.info();
        !matches!(self, Op::Wide | Op::J | Op::JX | Op::Ret)
            && !matches!(info.sem, Sem::Call(_))
            && info.roles.iter().any(|role| role.is_slot())
    }
}
