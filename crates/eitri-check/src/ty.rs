//! The types of the first slice.

use eitri_base::{Symbol, sym};

/// A checked type. `Error` marks an expression whose problem was already
/// reported; it is compatible with everything, so it causes no further
/// diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    I64,
    Bool,
    Void,
    Error,
}

impl Ty {
    /// The type's name as diagnostics quote it.
    pub fn quoted(self) -> &'static str {
        match self {
            Ty::I64 => "`i64`",
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
    if symbol == sym::I64 {
        TypeName::Supported(Ty::I64)
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
