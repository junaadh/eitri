//! Bounded root-map construction from explicitly supplied slot lifetimes.
use crate::{CallRoots, FunctionRoots, RootMap, RootMapId, RootState, SafepointPc, SlotRange};
use eitri_base::FxHashMap;

/// Root positions relative to a defined value's contiguous frame span.
/// All other positions in the span are initialized scalars.
#[derive(Clone, Debug, Default)]
pub struct RootLayout {
    pub ordinary: Vec<u32>,
    pub dynamic: Vec<(u32, u32)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootBuildError {
    Limit,
    OutsideFrame,
    OverlappingLayout,
    PartialDynamicLifetime,
    InvalidCallPhase,
    MissingCallState,
    MissingPc,
    DuplicatePc,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Dead,
    Scalar,
    Reference,
    DynamicType(u32),
    DynamicPayload(u32),
}
/// A branch-local lifetime state. Restore only on a control-flow edge that
/// carries exactly these facts; merging incompatible branches is future work.
#[derive(Clone)]
pub struct RootCheckpoint(Vec<Slot>);
#[derive(Default)]
struct PendingCall {
    pre: Option<RootState>,
    suspended: Option<RootState>,
    post: Option<RootState>,
}
/// At most 65,536 frame slots and an 8 MiB logical metadata budget for
/// snapshots, map payloads and PC expansion (allocator overhead is extra). Snapshot work is O(frame slots + map bytes); map interning uses
/// expected O(map bytes) hashing and O(map bytes) equality per collision.
/// `at_call`, `suspend_call`, `finish_call` capture the three states in order.
/// Snapshots are conservative: every initialized reference is considered live.
/// Control-flow joins and managed lowering must independently establish facts.
pub struct RootBuilder {
    slots: Vec<Slot>,
    maps: Vec<RootMap>,
    interned: FxHashMap<RootMap, RootMapId>,
    calls: Vec<PendingCall>,
    polls: Vec<(u32, RootState)>,
    entry: RootMapId,
    bytes: usize,
}
const LIMIT: usize = 8 * 1024 * 1024;
impl RootBuilder {
    pub fn new(frame_slots: u32) -> Result<Self, RootBuildError> {
        if frame_slots > 65_536 {
            return Err(RootBuildError::Limit);
        }
        let empty = RootMap::default();
        let mut interned = FxHashMap::default();
        interned.insert(empty.clone(), RootMapId(0));
        Ok(Self {
            slots: vec![Slot::Dead; frame_slots as usize],
            maps: vec![empty],
            interned,
            calls: Vec::new(),
            polls: Vec::new(),
            entry: RootMapId(0),
            bytes: 0,
        })
    }
    /// Existing scalar lowering uses this allocation-light path, without a
    /// lifetime pass or a per-slot facts array.
    pub fn scalar(call_sites: usize) -> FunctionRoots {
        FunctionRoots::scalar(call_sites)
    }
    pub fn checkpoint(&self) -> RootCheckpoint {
        RootCheckpoint(self.slots.clone())
    }
    pub fn restore(&mut self, state: &RootCheckpoint) -> Result<(), RootBuildError> {
        if state.0.len() != self.slots.len() {
            return Err(RootBuildError::OutsideFrame);
        }
        self.slots.clone_from(&state.0);
        Ok(())
    }
    fn range(&self, span: SlotRange) -> Result<std::ops::Range<usize>, RootBuildError> {
        let end = span
            .first
            .checked_add(span.count)
            .ok_or(RootBuildError::OutsideFrame)?;
        if end as usize > self.slots.len() {
            return Err(RootBuildError::OutsideFrame);
        }
        Ok(span.first as usize..end as usize)
    }
    fn check_boundary(&self, range: &std::ops::Range<usize>) -> Result<(), RootBuildError> {
        for slot in range.clone() {
            let partner = match self.slots[slot] {
                Slot::DynamicType(p) | Slot::DynamicPayload(p) => p as usize,
                _ => continue,
            };
            if !range.contains(&partner) {
                return Err(RootBuildError::PartialDynamicLifetime);
            }
        }
        Ok(())
    }
    pub fn note_definition(
        &mut self,
        span: SlotRange,
        layout: &RootLayout,
    ) -> Result<(), RootBuildError> {
        let range = self.range(span)?;
        self.check_boundary(&range)?;
        let mut values = vec![Slot::Scalar; range.len()];
        for &relative in &layout.ordinary {
            let slot = values
                .get_mut(relative as usize)
                .ok_or(RootBuildError::OutsideFrame)?;
            if *slot != Slot::Scalar {
                return Err(RootBuildError::OverlappingLayout);
            }
            *slot = Slot::Reference;
        }
        for &(ty, payload) in &layout.dynamic {
            if ty == payload || ty >= span.count || payload >= span.count {
                return Err(RootBuildError::OverlappingLayout);
            }
            if values[ty as usize] != Slot::Scalar || values[payload as usize] != Slot::Scalar {
                return Err(RootBuildError::OverlappingLayout);
            }
            values[ty as usize] = Slot::DynamicType(span.first + payload);
            values[payload as usize] = Slot::DynamicPayload(span.first + ty);
        }
        self.slots[range].copy_from_slice(&values);
        Ok(())
    }
    pub fn end_lifetime(&mut self, span: SlotRange) -> Result<(), RootBuildError> {
        let range = self.range(span)?;
        self.check_boundary(&range)?;
        self.slots[range].fill(Slot::Dead);
        Ok(())
    }
    fn snapshot(&mut self) -> Result<RootState, RootBuildError> {
        let mut map = RootMap::default();
        let mut initialized = Vec::new();
        let mut start = None;
        for (i, slot) in self.slots.iter().enumerate() {
            if *slot != Slot::Dead {
                start.get_or_insert(i as u32);
            }
            if *slot == Slot::Dead
                && let Some(first) = start.take()
            {
                initialized.push(SlotRange {
                    first,
                    count: i as u32 - first,
                });
            }
            match *slot {
                Slot::Reference => {
                    map.ordinary.resize(map.ordinary.len().max(i / 64 + 1), 0);
                    map.ordinary[i / 64] |= 1 << (i % 64);
                }
                Slot::DynamicType(payload) => map.dynamic.push((i as u32, payload)),
                _ => {}
            }
        }
        if let Some(first) = start {
            initialized.push(SlotRange {
                first,
                count: self.slots.len() as u32 - first,
            });
        }
        let new = !self.interned.contains_key(&map);
        let cost = std::mem::size_of::<RootState>()
            + initialized.len() * std::mem::size_of::<SlotRange>()
            + if new {
                2 * (std::mem::size_of::<RootMap>()
                    + map.ordinary.len() * 8
                    + map.dynamic.len() * 8)
            } else {
                0
            };
        let total = self
            .bytes
            .checked_add(cost)
            .filter(|&n| n <= LIMIT)
            .ok_or(RootBuildError::Limit)?;
        let id = if let Some(&id) = self.interned.get(&map) {
            id
        } else {
            let id = RootMapId(u32::try_from(self.maps.len()).map_err(|_| RootBuildError::Limit)?);
            self.interned.insert(map.clone(), id);
            self.maps.push(map);
            id
        };
        self.bytes = total;
        Ok(RootState {
            map: id,
            initialized,
        })
    }
    /// Capture roots of already-initialized parameters at function entry.
    pub fn at_entry(&mut self) -> Result<(), RootBuildError> {
        self.entry = self.snapshot()?.map;
        Ok(())
    }
    /// Capture the pre-call state. `site` is the call-site index.
    pub fn at_call(&mut self, site: u32) -> Result<(), RootBuildError> {
        if site as usize > self.calls.len() || site as usize >= 65_536 {
            return Err(RootBuildError::Limit);
        }
        if self
            .calls
            .get(site as usize)
            .is_some_and(|c| c.pre.is_some())
        {
            return Err(RootBuildError::InvalidCallPhase);
        }
        let state = self.snapshot()?;
        if site as usize == self.calls.len() {
            self.calls.push(PendingCall::default());
        }
        self.calls[site as usize].pre = Some(state);
        Ok(())
    }
    pub fn suspend_call(&mut self, site: u32) -> Result<(), RootBuildError> {
        if !self
            .calls
            .get(site as usize)
            .is_some_and(|c| c.pre.is_some() && c.suspended.is_none())
        {
            return Err(RootBuildError::InvalidCallPhase);
        }
        let state = self.snapshot()?;
        self.calls[site as usize].suspended = Some(state);
        Ok(())
    }
    pub fn finish_call(&mut self, site: u32) -> Result<(), RootBuildError> {
        if !self
            .calls
            .get(site as usize)
            .is_some_and(|c| c.suspended.is_some() && c.post.is_none())
        {
            return Err(RootBuildError::InvalidCallPhase);
        }
        let state = self.snapshot()?;
        self.calls[site as usize].post = Some(state);
        Ok(())
    }
    /// Record a logical instruction identity, resolved by `finish`.
    pub fn at_poll(&mut self, instruction: u32) -> Result<(), RootBuildError> {
        let state = self.snapshot()?;
        self.polls.push((instruction, state));
        Ok(())
    }
    /// Each logical poll maps to one or more final instruction-start PCs;
    /// the caller resolves expanded far branches to their polling instruction.
    /// The mapping must be unique and complete. Managed lowering must capture
    /// parameter roots via `at_entry` before emitting the body.
    pub fn finish(
        self,
        pc_map: &[(u32, Vec<SafepointPc>)],
    ) -> Result<FunctionRoots, RootBuildError> {
        let mut positions = FxHashMap::default();
        let mut budget = self.bytes;
        for (logical, pcs) in pc_map {
            let cost = pcs
                .len()
                .checked_mul(std::mem::size_of::<SafepointPc>())
                .and_then(|n| n.checked_add(std::mem::size_of::<(u32, &Vec<SafepointPc>)>()))
                .ok_or(RootBuildError::Limit)?;
            budget = budget
                .checked_add(cost)
                .filter(|&n| n <= LIMIT)
                .ok_or(RootBuildError::Limit)?;
            if pcs.is_empty() || positions.insert(*logical, pcs).is_some() {
                return Err(RootBuildError::DuplicatePc);
            }
        }
        let mut safepoints = Vec::new();
        for (logical, state) in self.polls {
            let pcs = positions.get(&logical).ok_or(RootBuildError::MissingPc)?;
            for &pc in pcs.iter() {
                let cost = std::mem::size_of::<(SafepointPc, RootState)>()
                    + state.initialized.len() * std::mem::size_of::<SlotRange>();
                budget = budget
                    .checked_add(cost)
                    .filter(|&n| n <= LIMIT)
                    .ok_or(RootBuildError::Limit)?;
                safepoints.push((pc, state.clone()));
            }
        }
        safepoints.sort_unstable_by_key(|(pc, _)| *pc);
        if safepoints.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(RootBuildError::DuplicatePc);
        }
        let calls = self
            .calls
            .into_iter()
            .map(|c| {
                Ok(CallRoots {
                    pre: c.pre.ok_or(RootBuildError::MissingCallState)?,
                    suspended: c.suspended.ok_or(RootBuildError::MissingCallState)?,
                    post: c.post.ok_or(RootBuildError::MissingCallState)?,
                })
            })
            .collect::<Result<_, RootBuildError>>()?;
        Ok(FunctionRoots {
            entry: self.entry,
            maps: self.maps,
            safepoints,
            calls,
        })
    }
}
