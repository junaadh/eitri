//! Test-only managed-slot lowering model. It does not enable managed opcodes.
//! Independent forward definite-shape and backward liveness analyses keep only
//! per-block frame vectors: O(blocks * slots + edges + instructions) space.
//! Fixpoint work is explicitly capped; a limit or incompatible live value is
//! a rejection, never an empty root map.
use eitri_bytecode::{RootFact, RootShape};

const MAX_CELLS: usize = 1 << 20;
const MAX_WORK: usize = 8 << 20;

#[derive(Clone, Debug)]
enum Op {
    Define(usize, Vec<RootShape>),
    End(usize, usize),
    Use(usize, usize),
    Poll(u32),
    Call {
        site: u32,
        args: (usize, usize),
        result: (usize, Vec<RootShape>),
    },
}
#[derive(Clone, Debug)]
struct Block {
    ops: Vec<Op>,
    successors: Vec<usize>,
}
#[derive(Debug, PartialEq, Eq)]
enum Error {
    Limit,
    InvalidGraph,
    InvalidLayout,
    Uninitialized(usize),
}
type Shapes = Vec<Option<RootShape>>;
struct Analysis {
    shapes: Vec<Option<Shapes>>,
    live_out: Vec<Vec<bool>>,
}

fn range(first: usize, count: usize, frame: usize) -> Result<std::ops::Range<usize>, Error> {
    let end = first
        .checked_add(count)
        .filter(|&end| end <= frame)
        .ok_or(Error::InvalidLayout)?;
    Ok(first..end)
}
fn check_pairs(slots: &[Option<RootShape>]) -> Result<(), Error> {
    for (i, shape) in slots.iter().enumerate() {
        match shape {
            Some(RootShape::DynamicType { payload })
                if slots.get(*payload as usize)
                    != Some(&Some(RootShape::DynamicPayload {
                        type_slot: i as u32,
                    })) =>
            {
                return Err(Error::InvalidLayout);
            }
            Some(RootShape::DynamicPayload { type_slot })
                if slots.get(*type_slot as usize)
                    != Some(&Some(RootShape::DynamicType { payload: i as u32 })) =>
            {
                return Err(Error::InvalidLayout);
            }
            _ => {}
        }
    }
    Ok(())
}
fn update(slots: &mut Shapes, first: usize, shape: &[RootShape]) -> Result<(), Error> {
    let span = range(first, shape.len(), slots.len())?;
    // Validate a replacement transactionally, including existing pairs that
    // would otherwise be torn apart by an overlapping definition.
    let mut next = slots.clone();
    for (slot, ty) in span.zip(shape) {
        next[slot] = Some(*ty);
    }
    check_pairs(&next)?;
    *slots = next;
    Ok(())
}
fn end(slots: &mut Shapes, first: usize, count: usize) -> Result<(), Error> {
    let span = range(first, count, slots.len())?;
    let mut next = slots.clone();
    next[span].fill(None);
    check_pairs(&next)?;
    *slots = next;
    Ok(())
}
fn forward(slots: &mut Shapes, op: &Op, check_reads: bool) -> Result<(), Error> {
    let read = |slots: &Shapes, first, count| -> Result<(), Error> {
        for index in range(first, count, slots.len())? {
            if check_reads && slots[index].is_none() {
                return Err(Error::Uninitialized(index));
            }
        }
        Ok(())
    };
    match op {
        Op::Define(first, shapes) => update(slots, *first, shapes)?,
        Op::End(first, count) => end(slots, *first, *count)?,
        Op::Use(first, count) => read(slots, *first, *count)?,
        Op::Poll(_) => {}
        Op::Call {
            args: (first, count),
            result: (dst, shapes),
            ..
        } => {
            read(slots, *first, *count)?;
            update(slots, *dst, shapes)?;
        }
    }
    Ok(())
}
fn backward(live: &mut [bool], op: &Op) -> Result<(), Error> {
    let frame = live.len();
    match op {
        Op::Define(first, shapes) => live[range(*first, shapes.len(), frame)?].fill(false),
        Op::End(first, count) => live[range(*first, *count, frame)?].fill(false),
        Op::Use(first, count) => live[range(*first, *count, frame)?].fill(true),
        Op::Poll(_) => {}
        Op::Call {
            args: (first, count),
            result: (dst, shapes),
            ..
        } => {
            live[range(*dst, shapes.len(), frame)?].fill(false);
            live[range(*first, *count, frame)?].fill(true);
        }
    }
    Ok(())
}

fn analyze(blocks: &[Block], initial: Shapes) -> Result<Analysis, Error> {
    let frame = initial.len();
    if blocks.is_empty()
        || frame > 65_536
        || blocks
            .len()
            .checked_mul(frame.max(1))
            .is_none_or(|n| n > MAX_CELLS)
    {
        return Err(Error::Limit);
    }
    check_pairs(&initial)?;
    if blocks
        .iter()
        .flat_map(|b| &b.successors)
        .any(|&s| s >= blocks.len())
    {
        return Err(Error::InvalidGraph);
    }
    let mut budget = 0usize;
    let mut charge = |block: &Block| -> Result<(), Error> {
        let cost = frame
            .max(1)
            .checked_mul(block.ops.len() + block.successors.len() + 1)
            .ok_or(Error::Limit)?;
        budget = budget
            .checked_add(cost)
            .filter(|&n| n <= MAX_WORK)
            .ok_or(Error::Limit)?;
        Ok(())
    };
    let mut incoming = vec![None; blocks.len()];
    incoming[0] = Some(initial);
    let mut work = vec![0];
    let mut queued = vec![false; blocks.len()];
    queued[0] = true;
    while let Some(b) = work.pop() {
        queued[b] = false;
        charge(&blocks[b])?;
        let mut state = incoming[b].clone().expect("queued reachable block");
        for op in &blocks[b].ops {
            forward(&mut state, op, false)?;
        }
        for &next in &blocks[b].successors {
            let changed = match &mut incoming[next] {
                None => {
                    incoming[next] = Some(state.clone());
                    true
                }
                Some(old) => {
                    let mut changed = false;
                    for (old, &new) in old.iter_mut().zip(&state) {
                        if *old != new && old.is_some() {
                            *old = None;
                            changed = true;
                        }
                    }
                    // An incongruent dynamic half cannot remain initialized.
                    let previous = old.clone();
                    for (i, shape) in previous.iter().enumerate() {
                        let partner = match shape {
                            Some(RootShape::DynamicType { payload }) => *payload as usize,
                            Some(RootShape::DynamicPayload { type_slot }) => *type_slot as usize,
                            _ => continue,
                        };
                        if previous.get(partner).is_none_or(Option::is_none) {
                            old[i] = None;
                            changed = true;
                        }
                    }
                    changed
                }
            };
            if changed && !queued[next] {
                work.push(next);
                queued[next] = true;
            }
        }
    }
    // Reject reads after all joins settle, not transient fixpoint states.
    for (b, state) in incoming.iter().enumerate() {
        if let Some(state) = state {
            charge(&blocks[b])?;
            let mut state = state.clone();
            for op in &blocks[b].ops {
                forward(&mut state, op, true)?;
            }
        }
    }
    let mut live_in = vec![vec![false; frame]; blocks.len()];
    let mut live_out = live_in.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..blocks.len()).rev() {
            if incoming[b].is_none() {
                continue;
            }
            charge(&blocks[b])?;
            let mut out = vec![false; frame];
            for &next in &blocks[b].successors {
                for (dst, &src) in out.iter_mut().zip(&live_in[next]) {
                    *dst |= src;
                }
            }
            let mut input = out.clone();
            for op in blocks[b].ops.iter().rev() {
                backward(&mut input, op)?;
            }
            changed |= out != live_out[b] || input != live_in[b];
            live_out[b] = out;
            live_in[b] = input;
        }
    }
    Ok(Analysis {
        shapes: incoming,
        live_out,
    })
}

impl Analysis {
    /// Reconstruct facts immediately before an operation, or at block end.
    /// Uses O(frame) scratch and O(block instructions * frame) work; callers
    /// request publication sites only. Dynamic partners are live together.
    fn at(&self, block: usize, index: usize, blocks: &[Block]) -> Result<Vec<RootFact>, Error> {
        let ops = &blocks.get(block).ok_or(Error::InvalidGraph)?.ops;
        if index > ops.len() {
            return Err(Error::InvalidGraph);
        }
        let mut shapes = self
            .shapes
            .get(block)
            .and_then(Clone::clone)
            .ok_or(Error::InvalidGraph)?;
        for op in &ops[..index] {
            forward(&mut shapes, op, true)?;
        }
        let mut live = self.live_out[block].clone();
        for op in ops[index..].iter().rev() {
            backward(&mut live, op)?;
        }
        for (i, shape) in shapes.iter().enumerate() {
            if let Some(RootShape::DynamicType { payload }) = shape
                && (live[i] || live[*payload as usize])
            {
                live[i] = true;
                live[*payload as usize] = true;
            }
        }
        Ok(shapes
            .into_iter()
            .zip(live)
            .map(|(shape, live)| RootFact {
                shape: shape.unwrap_or(RootShape::Scalar),
                initialized: shape.is_some(),
                live,
            })
            .collect())
    }
}

/// Test-only integration: derive snapshots from the dataflow results, feed the
/// builder using logical identities, then independently verify every map. The
/// scalar executable verifier cannot yet install managed maps.
fn build_maps(
    blocks: &[Block],
    analysis: &Analysis,
    pc_map: &[(u32, Vec<eitri_bytecode::SafepointPc>)],
) -> Result<eitri_bytecode::FunctionRoots, eitri_diag::Diagnostic> {
    use eitri_bytecode::{RootBuilder, RootLayout, SlotRange, verify_root_state};
    let source = eitri_base::SourceId::from_u32(0);
    let diagnostic = |e| crate::root_build_diagnostic(e, source, eitri_base::Span::default());
    let invalid = |e| eitri_diag::Diagnostic::bug(format!("synthetic root facts: {e:?}"));
    let entry = analysis.at(0, 0, blocks).map_err(invalid)?;
    let mut budget = 0usize;
    for block in blocks {
        let sites = block
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Poll(_) | Op::Call { .. }))
            .count();
        let cost = sites
            .checked_mul(block.ops.len() + 1)
            .and_then(|n| n.checked_mul(entry.len().max(1)))
            .and_then(|n| n.checked_mul(4)) // construction and verification replays
            .ok_or_else(|| diagnostic(eitri_bytecode::RootBuildError::Limit))?;
        budget = budget
            .checked_add(cost)
            .filter(|&n| n <= MAX_WORK)
            .ok_or_else(|| diagnostic(eitri_bytecode::RootBuildError::Limit))?;
    }
    fn sync(
        builder: &mut RootBuilder,
        facts: &[RootFact],
    ) -> Result<(), eitri_bytecode::RootBuildError> {
        builder.end_lifetime(SlotRange {
            first: 0,
            count: facts.len() as u32,
        })?;
        let mut i = 0;
        while i < facts.len() {
            if !facts[i].initialized {
                i += 1;
                continue;
            }
            let start = i;
            while i < facts.len() && facts[i].initialized {
                i += 1;
            }
            let mut layout = RootLayout::default();
            for (offset, fact) in facts[start..i].iter().enumerate() {
                match fact.shape {
                    RootShape::Reference if fact.live => layout.ordinary.push(offset as u32),
                    RootShape::DynamicType { payload } if fact.live => {
                        layout.dynamic.push((offset as u32, payload - start as u32))
                    }
                    _ => {}
                }
            }
            builder.note_definition(
                SlotRange {
                    first: start as u32,
                    count: (i - start) as u32,
                },
                &layout,
            )?;
        }
        Ok(())
    }
    let mut builder = RootBuilder::new(entry.len() as u32).map_err(diagnostic)?;
    sync(&mut builder, &entry).map_err(diagnostic)?;
    builder.at_entry().map_err(diagnostic)?;
    for (b, block) in blocks.iter().enumerate() {
        if analysis.shapes[b].is_none() {
            continue;
        }
        for (i, op) in block.ops.iter().enumerate() {
            match op {
                Op::Poll(id) => {
                    sync(&mut builder, &analysis.at(b, i, blocks).map_err(invalid)?)
                        .map_err(diagnostic)?;
                    builder.at_poll(*id).map_err(diagnostic)?;
                }
                Op::Call { site, .. } => {
                    sync(&mut builder, &analysis.at(b, i, blocks).map_err(invalid)?)
                        .map_err(diagnostic)?;
                    builder.at_call(*site).map_err(diagnostic)?;
                    builder.suspend_call(*site).map_err(diagnostic)?;
                    sync(
                        &mut builder,
                        &analysis.at(b, i + 1, blocks).map_err(invalid)?,
                    )
                    .map_err(diagnostic)?;
                    builder.finish_call(*site).map_err(diagnostic)?;
                }
                _ => {}
            }
        }
    }
    let maps = builder.finish(pc_map).map_err(diagnostic)?;
    let verify = |facts: &[RootFact], id: eitri_bytecode::RootMapId| {
        verify_root_state(facts, &maps.maps[id.0 as usize])
            .map_err(|e| eitri_diag::Diagnostic::bug(format!("invalid synthetic map: {e:?}")))
    };
    verify(&entry, maps.entry)?;
    for (b, block) in blocks.iter().enumerate() {
        if analysis.shapes[b].is_none() {
            continue;
        }
        for (i, op) in block.ops.iter().enumerate() {
            match op {
                Op::Poll(id) => {
                    let facts = analysis.at(b, i, blocks).map_err(invalid)?;
                    let pcs = pc_map
                        .iter()
                        .find(|(key, _)| key == id)
                        .ok_or_else(|| diagnostic(eitri_bytecode::RootBuildError::MissingPc))?;
                    for pc in &pcs.1 {
                        let index = maps
                            .safepoints
                            .binary_search_by_key(pc, |(pc, _)| *pc)
                            .map_err(|_| diagnostic(eitri_bytecode::RootBuildError::MissingPc))?;
                        verify(&facts, maps.safepoints[index].1.map)?;
                    }
                }
                Op::Call { site, .. } => {
                    let call = &maps.calls[*site as usize];
                    let before = analysis.at(b, i, blocks).map_err(invalid)?;
                    verify(&before, call.pre.map)?;
                    verify(&before, call.suspended.map)?;
                    verify(
                        &analysis.at(b, i + 1, blocks).map_err(invalid)?,
                        call.post.map,
                    )?;
                }
                _ => {}
            }
        }
    }
    Ok(maps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eitri_bytecode::{SafepointPc, verify_root_state};

    fn block(ops: Vec<Op>, successors: &[usize]) -> Block {
        Block {
            ops,
            successors: successors.to_vec(),
        }
    }
    fn cfg() -> Vec<Block> {
        use RootShape::*;
        vec![
            block(vec![Op::Poll(1)], &[1, 2]),
            block(vec![Op::Define(1, vec![Reference]), Op::Poll(2)], &[3]),
            block(vec![Op::Define(1, vec![Reference]), Op::Poll(3)], &[3]),
            block(
                vec![
                    Op::Poll(4),
                    Op::Call {
                        site: 0,
                        args: (0, 2),
                        result: (1, vec![Scalar]),
                    },
                    Op::Use(1, 1),
                    Op::End(0, 1),
                ],
                &[],
            ),
        ]
    }
    fn roots(facts: &[RootFact]) -> Vec<usize> {
        facts
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                (f.initialized && f.live && f.shape == RootShape::Reference).then_some(i)
            })
            .collect()
    }

    #[test]
    fn joins_and_call_result_reuse_derive_facts_and_maps() {
        let blocks = cfg();
        let analysis = analyze(&blocks, vec![Some(RootShape::Reference), None, None]).unwrap();
        let entry = analysis.at(0, 0, &blocks).unwrap();
        let left = analysis.at(1, 1, &blocks).unwrap();
        let right = analysis.at(2, 1, &blocks).unwrap();
        let join = analysis.at(3, 0, &blocks).unwrap();
        let before = analysis.at(3, 1, &blocks).unwrap();
        let after = analysis.at(3, 2, &blocks).unwrap();
        assert_eq!(roots(&entry), vec![0]);
        assert_eq!(roots(&left), vec![0, 1]);
        assert_eq!(roots(&right), vec![0, 1]);
        assert_eq!(roots(&join), vec![0, 1]);
        assert_eq!(roots(&before), vec![0, 1]);
        assert_eq!(roots(&after), Vec::<usize>::new());
        assert_eq!(after[1].shape, RootShape::Scalar);
        assert!(after[1].initialized);

        let maps = build_maps(
            &blocks,
            &analysis,
            &[
                (1, vec![SafepointPc(10)]),
                (2, vec![SafepointPc(20)]),
                (3, vec![SafepointPc(30)]),
                (4, vec![SafepointPc(40)]),
            ],
        )
        .unwrap();
        assert_eq!(
            verify_root_state(&entry, &maps.maps[maps.entry.0 as usize]),
            Ok(())
        );
        for (index, facts) in [&entry, &left, &right, &join].iter().enumerate() {
            let state = &maps.safepoints[index].1;
            assert_eq!(
                verify_root_state(facts, &maps.maps[state.map.0 as usize]),
                Ok(())
            );
        }
        let call = &maps.calls[0];
        for state in [&call.pre, &call.suspended] {
            assert_eq!(
                verify_root_state(&before, &maps.maps[state.map.0 as usize]),
                Ok(())
            );
        }
        assert_eq!(
            verify_root_state(&after, &maps.maps[call.post.map.0 as usize]),
            Ok(())
        );
    }

    #[test]
    fn join_rejects_missing_or_incompatible_initialization() {
        let mut blocks = cfg();
        blocks[2].ops.remove(0);
        assert!(matches!(
            analyze(&blocks, vec![Some(RootShape::Reference), None, None]),
            Err(Error::Uninitialized(1))
        ));
        blocks[2]
            .ops
            .insert(0, Op::Define(1, vec![RootShape::Scalar]));
        assert!(matches!(
            analyze(&blocks, vec![Some(RootShape::Reference), None, None]),
            Err(Error::Uninitialized(1))
        ));
    }

    #[test]
    fn dynamic_pair_liveness_and_backedge_fixpoint() {
        use RootShape::*;
        let blocks = vec![
            block(
                vec![Op::Define(
                    1,
                    vec![DynamicType { payload: 2 }, DynamicPayload { type_slot: 1 }],
                )],
                &[1],
            ),
            block(vec![Op::Poll(9), Op::Use(2, 1)], &[1, 2]),
            block(vec![Op::End(1, 2)], &[]),
        ];
        let analysis = analyze(&blocks, vec![None, None, None]).unwrap();
        let at = analysis.at(1, 0, &blocks).unwrap();
        assert!(at[1].live && at[2].live);
        let maps = build_maps(&blocks, &analysis, &[(9, vec![SafepointPc(9)])]).unwrap();
        let map = &maps.maps[maps.safepoints[0].1.map.0 as usize];
        assert_eq!(map.dynamic, vec![(1, 2)]);
        assert_eq!(verify_root_state(&at, map), Ok(()));
    }

    #[test]
    fn missing_logical_pc_produces_limit_diagnostic() {
        let blocks = cfg();
        let analysis = analyze(&blocks, vec![Some(RootShape::Reference), None, None]).unwrap();
        let error = match build_maps(&blocks, &analysis, &[]) {
            Err(error) => error,
            Ok(_) => panic!("missing PCs accepted"),
        };
        assert_eq!(error.code, Some(eitri_diag::Code::Limit));
    }

    #[test]
    fn rejects_partial_dynamic_overwrite_and_work_limit() {
        use RootShape::*;
        let blocks = vec![block(
            vec![
                Op::Define(
                    0,
                    vec![DynamicType { payload: 1 }, DynamicPayload { type_slot: 0 }],
                ),
                Op::Define(1, vec![Scalar]),
            ],
            &[],
        )];
        assert!(matches!(
            analyze(&blocks, vec![None, None]),
            Err(Error::InvalidLayout)
        ));
        let blocks = vec![block(vec![], &[]); 1025];
        assert!(matches!(
            analyze(&blocks, vec![None; 1024]),
            Err(Error::Limit)
        ));
    }
}
