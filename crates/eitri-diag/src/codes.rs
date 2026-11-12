//! The catalog of every diagnostic code, for every layer, in one place.
//!
//! Producers name a variant (`Code::Lexical`), never a number, so a code is
//! defined once and its number, summary, and layer cannot drift apart. The
//! catalog lives in `eitri-diag` because every reporting layer already
//! depends on it. A test checks it against the table in
//! [the first slice](../../../docs/implementation/first-slice.md#diagnostic-codes),
//! and T047 generates the published catalog from [`Code::ALL`].

use std::fmt;

macro_rules! codes {
    ($(
        $(#[$section_meta:meta])*
        $layer:literal {
            $($(#[$meta:meta])* $code:ident = $number:literal: $summary:literal,)*
        }
    )*) => {
        /// A stable diagnostic code, displayed as `E` and four digits.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Code {
            $($($(#[$meta])* $code,)*)*
        }

        impl Code {
            /// Every code, in catalog order.
            pub const ALL: &[Code] = &[$($(Code::$code,)*)*];

            pub const fn number(self) -> u16 {
                match self { $($(Code::$code => $number,)*)* }
            }

            /// What the code covers, as the catalog states it.
            pub const fn summary(self) -> &'static str {
                match self { $($(Code::$code => $summary,)*)* }
            }

            /// The layer whose section defines the code.
            pub const fn layer(self) -> &'static str {
                match self { $($(Code::$code => $layer,)*)* }
            }

            pub fn from_number(number: u16) -> Option<Code> {
                Self::ALL.iter().copied().find(|code| code.number() == number)
            }
        }
    };
}

codes! {
    "implementation" {
        /// The message names the unsupported feature.
        Unsupported = 1: "A language feature outside this slice. The message names the feature",
        Limit = 2: "An implementation limit from [Resources](#resources)",
        HostBindingMismatch = 3: "An artifact host index has a different binding in the destination registry",
    }
    "lexer and parser" {
        Lexical = 101: "Unexpected character, malformed literal, or unterminated comment or literal",
        Syntax = 102: "Syntax error: expected one thing, found another",
    }
    "names and modules" {
        UnknownName = 201: "Unknown name",
        DuplicateName = 202: "Duplicate function or import name",
        TypeNameAsName = 203: "A predeclared type name used as a name",
        UnresolvedImport = 205: "Unresolved import path, or `import` after another item",
    }
    "types" {
        TypeMismatch = 301: "Type mismatch",
        LiteralOutOfRange = 302: "Scalar literal does not fit its expected type",
        AssignToImmutable = 303: "Assignment to something other than a `var` binding",
        ArgumentCount = 304: "Wrong number of arguments, or calling something that is not a function",
    }
    "control flow" {
        IfWithoutElse = 401: "`if` used as a value without `else`, or a branch without a value",
        MissingReturn = 402: "Missing `return` in a function with a result",
    }
    "bytecode assembly" {
        AsmSyntax = 901: "Malformed `.eits` assembly text",
        AsmMismatch = 902: "`.eits` that contradicts its program: an unknown or duplicate name, a mismatched type, slot, frame size, or encoding, or an unregistered host function",
        AsmRejected = 903: "Bytecode the verifier rejects",
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E{:04}", self.number())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_unique_ordered_and_four_digits() {
        for pair in Code::ALL.windows(2) {
            assert!(pair[0].number() < pair[1].number(), "{pair:?}");
        }
        for &code in Code::ALL {
            assert!(code.number() <= 9999);
            assert_eq!(Code::from_number(code.number()), Some(code));
            // The hundreds digit groups codes by layer.
            assert_eq!(
                Code::from_number(code.number()).unwrap().layer(),
                code.layer()
            );
        }
        assert_eq!(Code::Lexical.to_string(), "E0101");
        assert_eq!(Code::from_number(204), None, "reserved for T022");
    }

    #[test]
    fn layers_own_whole_hundreds() {
        for &a in Code::ALL {
            for &b in Code::ALL {
                if a.number() / 100 == b.number() / 100 {
                    assert_eq!(a.layer(), b.layer(), "{a} and {b}");
                }
            }
        }
    }
}
