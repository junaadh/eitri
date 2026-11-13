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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootMap {
    /// Bit i describes ordinary frame slot i. Trailing zero words are allowed.
    pub ordinary: Vec<u64>,
    /// Type and payload slot for each dynamic value.
    pub dynamic: Vec<(u32, u32)>,
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
