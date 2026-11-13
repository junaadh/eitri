//! Base types shared by every Eitri layer: typed indexes and the tables they
//! index, pooled list handles, spans, hashing, and spelling interning.
//!
//! Nothing here is global. Tables, pools, and the [`Interner`] are owned by
//! the caller's context (or the compiler scratch inside it), passed
//! explicitly, and reset between compilations so their allocations are
//! reused. Only immutable tables, such as the predefined symbols, are
//! `static`.

mod fx;
mod idx;
mod list;
mod memory;
mod span;
mod symbol;

pub use fx::{FxBuildHasher, FxHashMap, FxHashSet, FxHasher, fx_hash};
pub use idx::{CapacityError, IdIter, IdRange, Idx, IndexVec, MAX_INDEX};

// Used by `newtype_index!` expansions.
#[doc(hidden)]
pub use idx::{encode, encode_usize as __encode_usize};
pub use list::{List, ListPool};
pub use memory::{MemoryBudget, OutOfMemory};
pub use span::{SourceId, Span};
pub use symbol::{Interner, Symbol, kw, sym};
