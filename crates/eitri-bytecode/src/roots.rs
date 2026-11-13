//! Root publication metadata. Coverage against slot facts is verified separately.
use crate::SlotRange;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RootMapId(pub u32);
/// Instruction-start word PC used for root publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SafepointPc(pub u32);
/// Word PC used for reporting a fault, not for selecting a root map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FaultPc(pub u32);

/// One dynamic root guarded by every enclosing `(tag slot, case)` pair,
/// outermost first. A flat path encodes nested guards without recursive
/// allocation or recursive traversal. Inactive payload bytes are never read.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GuardedRoot {
    pub guards: Vec<(u32, u32)>,
    pub type_slot: u32,
    pub payload_slot: u32,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct RootMap {
    /// Bit i describes ordinary frame slot i. Trailing zero words are allowed.
    pub ordinary: Vec<u64>,
    /// Type and payload slot for each dynamic value.
    pub dynamic: Vec<(u32, u32)>,
    /// Dynamic pairs that may be read only under their complete case path.
    pub guarded: Vec<GuardedRoot>,
}
/// Independently derived slot shape, including the partner of a dynamic slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootShape {
    Scalar,
    Reference,
    DynamicType { payload: u32 },
    DynamicPayload { type_slot: u32 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RootFact {
    pub shape: RootShape,
    pub initialized: bool,
    pub live: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootState {
    pub map: RootMapId,
    pub initialized: Vec<SlotRange>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRoots {
    pub pre: RootState,
    pub suspended: RootState,
    pub post: RootState,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionRoots {
    pub entry: RootMapId,
    pub maps: Vec<RootMap>,
    pub safepoints: Vec<(SafepointPc, RootState)>,
    pub calls: Vec<CallRoots>,
}
impl FunctionRoots {
    /// Compare referenced states by map contents, independent of local IDs and
    /// unused maps. PCs and initialized spans remain part of the comparison.
    pub fn equivalent(&self, other: &Self) -> bool {
        let map_eq = |a: RootMapId, b: RootMapId| match (
            self.maps.get(a.0 as usize),
            other.maps.get(b.0 as usize),
        ) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        let state_eq =
            |a: &RootState, b: &RootState| a.initialized == b.initialized && map_eq(a.map, b.map);
        map_eq(self.entry, other.entry)
            && self.safepoints.len() == other.safepoints.len()
            && self
                .safepoints
                .iter()
                .zip(&other.safepoints)
                .all(|((pa, a), (pb, b))| pa == pb && state_eq(a, b))
            && self.calls.len() == other.calls.len()
            && self.calls.iter().zip(&other.calls).all(|(a, b)| {
                state_eq(&a.pre, &b.pre)
                    && state_eq(&a.suspended, &b.suspended)
                    && state_eq(&a.post, &b.post)
            })
    }
    /// Only this metadata can currently be regenerated after rewriting scalar
    /// code. Other states must be preserved until managed lowering exists.
    pub fn can_regenerate_scalar(&self) -> bool {
        self.maps.iter().all(|m| {
            m.dynamic.is_empty() && m.guarded.is_empty() && m.ordinary.iter().all(|&w| w == 0)
        }) && self
            .safepoints
            .iter()
            .all(|(_, s)| s.initialized.is_empty())
            && self.calls.iter().all(|c| {
                c.pre.initialized.is_empty()
                    && c.suspended.initialized.is_empty()
                    && c.post.initialized.is_empty()
            })
    }
    /// Heap storage retained by this metadata, excluding the inline struct.
    pub fn heap_bytes(&self) -> usize {
        use std::mem::size_of;
        let spans = |state: &RootState| state.initialized.capacity() * size_of::<SlotRange>();
        self.maps.capacity() * size_of::<RootMap>()
            + self
                .maps
                .iter()
                .map(|map| {
                    map.ordinary.capacity() * size_of::<u64>()
                        + map.dynamic.capacity() * size_of::<(u32, u32)>()
                        + map.guarded.capacity() * size_of::<GuardedRoot>()
                        + map
                            .guarded
                            .iter()
                            .map(|root| root.guards.capacity() * size_of::<(u32, u32)>())
                            .sum::<usize>()
                })
                .sum::<usize>()
            + self.safepoints.capacity() * size_of::<(SafepointPc, RootState)>()
            + self
                .safepoints
                .iter()
                .map(|(_, state)| spans(state))
                .sum::<usize>()
            + self.calls.capacity() * size_of::<CallRoots>()
            + self
                .calls
                .iter()
                .map(|call| spans(&call.pre) + spans(&call.suspended) + spans(&call.post))
                .sum::<usize>()
    }
    /// Scalar code has no managed roots; all call states share one empty map.
    pub fn scalar(call_sites: usize) -> Self {
        let state = RootState {
            map: RootMapId(0),
            initialized: Vec::new(),
        };
        Self {
            entry: RootMapId(0),
            maps: vec![RootMap::default()],
            safepoints: Vec::new(),
            calls: vec![
                CallRoots {
                    pre: state.clone(),
                    suspended: state.clone(),
                    post: state
                };
                call_sites
            ],
        }
    }
}
