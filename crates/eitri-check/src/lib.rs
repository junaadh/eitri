//! The Eitri frontend's semantic layer: name resolution and type checking
//! of the first slice (T008), and the host module registry it checks
//! imports against.

pub(crate) mod check;
pub mod dump;
mod host;
mod ty;

pub use check::{
    Binding, BindingId, CheckScratch, Checked, Input, Res, Signature, check, int_value,
};
pub use host::{DECLARATION, HostFn, HostFnId, HostModule, HostModuleId, Hosts};
pub use ty::{FixedScalar, Ty};

#[cfg(test)]
mod tests;
