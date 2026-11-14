//! Executable functions: code words and everything they refer to.

use eitri_base::Span;

eitri_base::newtype_index! {
    /// A function in one compiled program.
    pub struct FunctionId;
}

/// The type a slot holds, as far as bytecode is concerned. The first slice
/// has only untagged 64-bit scalars; a `bool` is 0 or 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
    I64,
    I8,
    U8,
    U64,
    F32,
    F64,
    Char,
    String,
    NullableString,
    /// A managed record whose layout is indexed in the program's shape table.
    Record(u32),
    NullableRecord(u32),
    Bool,
}

impl ValueType {
    pub fn is_managed(self) -> bool {
        matches!(
            self,
            Self::String | Self::NullableString | Self::Record(_) | Self::NullableRecord(_)
        )
    }
    pub fn integer(self) -> bool {
        matches!(self, Self::I8 | Self::U8 | Self::I64 | Self::U64)
    }
    pub fn signed(self) -> bool {
        matches!(self, Self::I8 | Self::I64)
    }
    pub fn bits(self) -> u32 {
        match self {
            Self::I8 | Self::U8 => 8,
            Self::F32 | Self::Char => 32,
            _ => 64,
        }
    }
    pub fn canonical(self, word: u64) -> bool {
        match self {
            Self::String | Self::NullableString | Self::Record(_) | Self::NullableRecord(_) => {
                false
            }
            Self::Bool => word <= 1,
            Self::I8 => word == (word as i8 as i64 as u64),
            Self::U8 => word <= 255,
            Self::F32 => word >> 32 == 0,
            Self::Char => u32::try_from(word).ok().and_then(char::from_u32).is_some(),
            _ => true,
        }
    }
}

/// Parameter and result types. Results occupy slots `0..results.len()` and
/// parameters the slots after them, so a callee's frame starts with its
/// result span, then its arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signature {
    pub params: Vec<ValueType>,
    /// Empty for `void`.
    pub results: Vec<ValueType>,
}

impl Signature {
    /// First parameter slot.
    pub fn param_base(&self) -> u32 {
        self.results.len() as u32
    }
}

/// A contiguous run of frame slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotRange {
    pub first: u32,
    pub count: u32,
}

impl SlotRange {
    pub fn end(self) -> u32 {
        self.first + self.count
    }

    pub fn slots(self) -> std::ops::Range<u32> {
        self.first..self.end()
    }

    pub fn slots_usize(self) -> std::ops::Range<usize> {
        self.first as usize..self.end() as usize
    }

    pub fn overlaps(self, other: SlotRange) -> bool {
        self.count > 0 && other.count > 0 && self.first < other.end() && other.first < self.end()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallTarget {
    Script(FunctionId),
    /// An index into the runtime's host function table.
    Host(u32),
}

/// Everything a `CALL` needs besides its opcode: the target, the caller's
/// argument window (in declaration order), and where the result goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallSite {
    pub target: CallTarget,
    pub args: SlotRange,
    pub result: SlotRange,
}

/// One assembled function. Immutable once built; execution state lives
/// elsewhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    /// For traces: the function's name, or `<module>`.
    pub name: Box<str>,
    pub signature: Signature,
    /// False when the source signature contains aggregate values.
    pub reentry_safe: bool,
    pub frame_size: u32,
    pub code: Box<[u32]>,
    /// One-slot constants, by `LDK`/`LDK_X` index.
    pub consts: Box<[u64]>,
    /// UTF-8 literal bytes owned by this function's compiled artifact.
    pub strings: Box<[Box<str>]>,
    pub sites: Box<[CallSite]>,
    /// Program-wide record field types indexed by nominal declaration ID.
    pub record_shapes: Box<[Box<[ValueType]>]>,
    /// Cold root publication metadata, indexed independently of call sites.
    pub roots: crate::FunctionRoots,
    /// The type each literal load (`LDI`, `LDK`, `LDK_X`) produces, by PC,
    /// sorted: a literal `1` may be an `i64` or the `true` of `b ^ 1`.
    pub literal_types: Box<[(u32, ValueType)]>,
    /// One bit per code word: set where an instruction (or its `WIDE`
    /// prefix) starts. Branches may only target these.
    pub starts: Box<[u64]>,
    /// The source span of each instruction, by start PC, sorted.
    pub spans: Box<[(u32, Span)]>,
}

impl Function {
    /// Compare executable contents while treating map IDs as local indexes.
    pub fn equivalent(&self, other: &Self) -> bool {
        self.name == other.name
            && self.signature == other.signature
            && self.reentry_safe == other.reentry_safe
            && self.frame_size == other.frame_size
            && self.code == other.code
            && self.consts == other.consts
            && self.strings == other.strings
            && self.sites == other.sites
            && self.record_shapes == other.record_shapes
            && self.literal_types == other.literal_types
            && self.starts == other.starts
            && self.spans == other.spans
            && self.roots.equivalent(&other.roots)
    }

    pub fn is_start(&self, pc: usize) -> bool {
        pc < self.code.len() && self.starts[pc / 64] >> (pc % 64) & 1 == 1
    }

    /// The source span of the instruction starting at `pc`.
    pub fn span_at(&self, pc: u32) -> Option<Span> {
        let index = self.spans.partition_point(|&(start, _)| start <= pc);
        index.checked_sub(1).map(|i| self.spans[i].1)
    }

    pub fn literal_type(&self, pc: u32) -> Option<ValueType> {
        self.literal_types
            .binary_search_by_key(&pc, |&(at, _)| at)
            .ok()
            .map(|i| self.literal_types[i].1)
    }
}
