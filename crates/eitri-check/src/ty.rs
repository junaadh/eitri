//! The types of the first slice.

use eitri_base::{Symbol, sym};

/// A checked type. `Error` marks an expression whose problem was already
/// reported; it is compatible with everything, so it causes no further
/// diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    I64,
    I8,
    U8,
    U64,
    F32,
    F64,
    Char,
    String,
    OptionalString,
    OptionalI64,
    OptionalBool,
    OptionalI8,
    OptionalU8,
    OptionalU64,
    OptionalF32,
    OptionalF64,
    OptionalChar,
    OptionalStruct(eitri_syntax::ast::StructId),
    OptionalEnum(eitri_syntax::ast::EnumId),
    Struct(eitri_syntax::ast::StructId),
    Class(eitri_syntax::ast::StructId),
    Enum(eitri_syntax::ast::EnumId),
    /// A find result: null or a nonnegative UTF-8 byte offset.
    FindOffset,
    Bool,
    Void,
    Error,
}

impl Ty {
    /// Shared semantic description; recovery errors are never executable types.
    pub fn descriptor(self) -> Option<eitri_types::TypeDesc> {
        use eitri_types::{ScalarKind as S, TypeDesc};
        Some(TypeDesc::Scalar(match self {
            Self::I64 => S::I64,
            Self::I8 => S::I8,
            Self::U8 => S::U8,
            Self::U64 => S::U64,
            Self::F32 => S::F32,
            Self::F64 => S::F64,
            Self::Char => S::Char,
            Self::Bool => S::Bool,
            Self::String => return Some(TypeDesc::String),
            Self::Struct(_)
            | Self::Class(_)
            | Self::Enum(_)
            | Self::OptionalStruct(_)
            | Self::OptionalEnum(_) => {
                return None;
            }
            Self::FindOffset
            | Self::OptionalString
            | Self::OptionalI64
            | Self::OptionalBool
            | Self::OptionalI8
            | Self::OptionalU8
            | Self::OptionalU64
            | Self::OptionalF32
            | Self::OptionalF64
            | Self::OptionalChar => {
                return None;
            }
            Self::Void => return Some(TypeDesc::Void),
            Self::Error => return None,
        }))
    }

    pub fn tagged_optional_inner(self) -> Option<Self> {
        Some(match self {
            Self::OptionalI64 => Self::I64,
            Self::OptionalBool => Self::Bool,
            Self::OptionalI8 => Self::I8,
            Self::OptionalU8 => Self::U8,
            Self::OptionalU64 => Self::U64,
            Self::OptionalF32 => Self::F32,
            Self::OptionalF64 => Self::F64,
            Self::OptionalChar => Self::Char,
            Self::OptionalStruct(id) => Self::Struct(id),
            Self::OptionalEnum(id) => Self::Enum(id),
            _ => return None,
        })
    }

    pub fn optional_of(self) -> Option<Self> {
        Some(match self {
            Self::String => Self::OptionalString,
            Self::I64 => Self::OptionalI64,
            Self::Bool => Self::OptionalBool,
            Self::I8 => Self::OptionalI8,
            Self::U8 => Self::OptionalU8,
            Self::U64 => Self::OptionalU64,
            Self::F32 => Self::OptionalF32,
            Self::F64 => Self::OptionalF64,
            Self::Char => Self::OptionalChar,
            Self::Struct(id) => Self::OptionalStruct(id),
            Self::Enum(id) => Self::OptionalEnum(id),
            _ => return None,
        })
    }

    pub fn integer(self) -> bool {
        matches!(self, Self::I8 | Self::U8 | Self::I64 | Self::U64)
    }
    pub fn float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }
    pub fn numeric(self) -> bool {
        self.integer() || self.float()
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
    /// The type's name as diagnostics quote it.
    pub fn quoted(self) -> &'static str {
        match self {
            Ty::I64 => "`i64`",
            Ty::I8 => "`i8`",
            Ty::U8 => "`u8`",
            Ty::U64 => "`u64`",
            Ty::F32 => "`f32`",
            Ty::F64 => "`f64`",
            Ty::Char => "`char`",
            Ty::String => "`string`",
            Ty::OptionalString => "`?string`",
            Ty::OptionalI64 => "`?i64`",
            Ty::OptionalBool => "`?bool`",
            Ty::OptionalI8 => "`?i8`",
            Ty::OptionalU8 => "`?u8`",
            Ty::OptionalU64 => "`?u64`",
            Ty::OptionalF32 => "`?f32`",
            Ty::OptionalF64 => "`?f64`",
            Ty::OptionalChar => "`?char`",
            Ty::OptionalStruct(_) => "an optional struct",
            Ty::OptionalEnum(_) => "an optional enum",
            Ty::Class(_) => "a class",
            Ty::Struct(_) => "a struct",
            Ty::Enum(_) => "an enum",
            Ty::FindOffset => "`?i64`",
            Ty::Bool => "`bool`",
            Ty::Void => "`void`",
            Ty::Error => "an erroneous type",
        }
    }
}

/// How a written type name resolves.
pub(crate) enum TypeName {
    Supported(Ty),
    /// A predeclared type outside the first slice, such as `u8`.
    Unsupported,
    Unknown,
}

pub(crate) fn type_name(symbol: Symbol) -> TypeName {
    if symbol == sym::I8 {
        TypeName::Supported(Ty::I8)
    } else if symbol == sym::U8 {
        TypeName::Supported(Ty::U8)
    } else if symbol == sym::U64 {
        TypeName::Supported(Ty::U64)
    } else if symbol == sym::F32 {
        TypeName::Supported(Ty::F32)
    } else if symbol == sym::F64 {
        TypeName::Supported(Ty::F64)
    } else if symbol == sym::CHAR {
        TypeName::Supported(Ty::Char)
    } else if symbol == sym::I64 {
        TypeName::Supported(Ty::I64)
    } else if symbol == sym::STRING {
        TypeName::Supported(Ty::String)
    } else if symbol == sym::BOOL {
        TypeName::Supported(Ty::Bool)
    } else if symbol == sym::VOID {
        TypeName::Supported(Ty::Void)
    } else if symbol.is_type_name() {
        TypeName::Unsupported
    } else {
        TypeName::Unknown
    }
}
