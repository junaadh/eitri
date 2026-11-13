//! Re-optimizing hot code: passes over a function's recorded logical
//! instructions ([`ir`]), whose result is assembled and verified again.
//!
//! First runs never come here; a host asks for it once code has proved
//! hot. Every pass preserves the language's observable behavior exactly:
//! the same host effects in the same order, the same faults with the same
//! traces, and the same budget debits (no call or backward branch is added
//! or removed). Each rewrite checks its own conditions on arbitrary control
//! flow, since hand-written `.eits` reaches it as well as compiled code.
//!
//! Analysis is liveness over frame slots: bitsets per block, solved
//! backward to a fixed point. Its cost is O(blocks × slots / 64) per
//! iteration; a function whose analysis would exceed [`ANALYSIS_LIMIT`]
//! words is left as it is. A query inside a block scans forward to the
//! slot's next access, at most [`SCAN_LIMIT`] instructions, and answers
//! "live" past that, which only forgoes a rewrite. Every pass is linear in
//! the instructions it rewrites plus the loops' sizes.

use eitri_base::FxHashMap;

use crate::encoding::{MAX_SLOT, Operands};
use crate::function::{Function, SlotRange, ValueType};
use crate::ir::{Body, Branch, Insn, Item};
use crate::spec::{Op, Role};

/// Liveness storage, in 64-bit words, above which a function is not
/// optimized: keeps the optimizer's own cost bounded.
pub const ANALYSIS_LIMIT: usize = 1 << 22;

/// Instructions a liveness query scans within a block before giving up.
pub const SCAN_LIMIT: usize = 4096;

/// What the passes changed, for reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// `MOV`s removed by passing variables' slots as call arguments.
    pub coalesced_moves: u32,
    /// Literal loads moved out of loops.
    pub hoisted_loads: u32,
    /// Functions skipped because their analysis would be too large.
    pub skipped: u32,
}

/// Optimize one function's body. `function` is its current code, for the
/// signature and frame size.
pub fn optimize(body: &Body, function: &Function, report: &mut Report) -> Body {
    let mut body = body.clone();
    // This optimizer has only scalar facts. Retain unknown root lifetimes
    // unchanged until managed map regeneration is available.
    if !function.roots.can_regenerate_scalar() {
        report.skipped += 1;
        return body;
    }
    let frame = function.frame_size as usize;
    let results = function.signature.results.len() as u32;
    // Each pass re-analyzes: rewrites change liveness.
    let Some(live) = Liveness::new(&body, frame, results) else {
        report.skipped += 1;
        return body;
    };
    report.coalesced_moves += coalesce_arguments(&mut body, &live);
    if let Some(live) = Liveness::new(&body, frame, results) {
        report.hoisted_loads += hoist_literals(&mut body, &live, function.frame_size);
    }
    body
}

// ---- slot uses and definitions ----------------------------------------------

/// Calls `visit(slot, is_def)` for each slot an instruction reads (`false`)
/// or writes (`true`). Reads are visited before writes.
fn slots(insn: &Insn, results: u32, mut visit: impl FnMut(u32, bool)) {
    match *insn {
        Insn::Label(_) => {}
        Insn::Load { dst, .. } => visit(dst, true),
        Insn::Plain { op, operands } => {
            let roles = op.info().roles;
            let columns = [operands.a, operands.b, operands.c];
            for (&role, &value) in roles.iter().zip(&columns) {
                if matches!(role, Role::Src | Role::SrcDst) {
                    visit(value as u32, false);
                }
            }
            for (&role, &value) in roles.iter().zip(&columns) {
                if matches!(role, Role::Dst | Role::SrcDst) {
                    visit(value as u32, true);
                }
            }
        }
        Insn::Branch { branch, .. } => match branch {
            Branch::Compare { a, b, .. } => {
                visit(a, false);
                visit(b, false);
            }
            Branch::CompareImm { a, .. } | Branch::Test { a, .. } => visit(a, false),
            Branch::Jump => {}
        },
        Insn::Call(site) => {
            site.args.slots().for_each(|slot| visit(slot, false));
            site.result.slots().for_each(|slot| visit(slot, true));
        }
        Insn::Ret => (0..results).for_each(|slot| visit(slot, false)),
    }
}

fn reads(insn: &Insn, results: u32, slot: u32) -> bool {
    let mut found = false;
    slots(insn, results, |s, def| found |= !def && s == slot);
    found
}

fn writes(insn: &Insn, results: u32, slot: u32) -> bool {
    let mut found = false;
    slots(insn, results, |s, def| found |= def && s == slot);
    found
}

// ---- control flow and liveness -----------------------------------------------

struct Block {
    /// Item indices `start..end`, labels included.
    start: usize,
    end: usize,
    succs: [Option<usize>; 2],
}

/// Which slots are live after each item.
struct Liveness {
    words: usize,
    results: u32,
    /// Per block: live on exit.
    live_out: Vec<u64>,
    blocks: Vec<Block>,
    /// The block of each item.
    block_of: Vec<usize>,
}

impl Liveness {
    fn new(body: &Body, frame: usize, results: u32) -> Option<Self> {
        let items = &body.items;
        // Blocks start at labels and after branches and returns.
        let mut starts = vec![0usize];
        for (i, item) in items.iter().enumerate() {
            match item.insn {
                Insn::Label(_) if *starts.last().unwrap() != i => starts.push(i),
                Insn::Branch { .. } | Insn::Ret if i + 1 < items.len() => starts.push(i + 1),
                _ => {}
            }
        }
        starts.dedup();
        let words = frame.div_ceil(64).max(1);
        if starts.len().saturating_mul(words) > ANALYSIS_LIMIT {
            return None;
        }
        let mut block_of = vec![0; items.len()];
        let mut label_block = vec![usize::MAX; body.labels as usize];
        let mut blocks = Vec::with_capacity(starts.len());
        for (b, &start) in starts.iter().enumerate() {
            let end = starts.get(b + 1).copied().unwrap_or(items.len());
            for (i, item) in items.iter().enumerate().take(end).skip(start) {
                block_of[i] = b;
                if let Insn::Label(n) = item.insn {
                    label_block[n as usize] = b;
                }
            }
            blocks.push(Block {
                start,
                end,
                succs: [None, None],
            });
        }
        let count = blocks.len();
        for b in 0..count {
            let last = items[blocks[b].start..blocks[b].end]
                .iter()
                .rev()
                .find(|item| !matches!(item.insn, Insn::Label(_)));
            let next = (b + 1 < count).then_some(b + 1);
            blocks[b].succs = match last.map(|item| item.insn) {
                Some(Insn::Ret) => [None, None],
                Some(Insn::Branch { branch, target }) => {
                    let to = label_block.get(target as usize).copied()?;
                    if to == usize::MAX {
                        return None;
                    }
                    match branch {
                        Branch::Jump => [Some(to), None],
                        _ => [Some(to), next],
                    }
                }
                _ => [next, None],
            };
        }
        // Per block: uses before definitions, and definitions.
        let mut uses = vec![0u64; count * words];
        let mut defs = vec![0u64; count * words];
        for (b, block) in blocks.iter().enumerate() {
            let (u, d) = (&mut uses[b * words..], &mut defs[b * words..]);
            for item in &items[block.start..block.end] {
                slots(&item.insn, results, |slot, def| {
                    let (word, bit) = (slot as usize / 64, 1u64 << (slot % 64));
                    if word >= words {
                        return;
                    }
                    if def {
                        d[word] |= bit;
                    } else if d[word] & bit == 0 {
                        u[word] |= bit;
                    }
                });
            }
        }
        let mut live_in = vec![0u64; count * words];
        let mut live_out = vec![0u64; count * words];
        let mut changed = true;
        while changed {
            changed = false;
            for b in (0..count).rev() {
                for w in 0..words {
                    let mut out = 0;
                    for succ in blocks[b].succs.into_iter().flatten() {
                        out |= live_in[succ * words + w];
                    }
                    let input = uses[b * words + w] | (out & !defs[b * words + w]);
                    live_out[b * words + w] = out;
                    if input != live_in[b * words + w] {
                        live_in[b * words + w] = input;
                        changed = true;
                    }
                }
            }
        }
        Some(Liveness {
            words,
            results,
            live_out,
            blocks,
            block_of,
        })
    }

    /// Whether `slot` may be live just after item `at`: its next access in
    /// the block is a read, or there is none and it is live on exit.
    /// Conservative (true) past [`SCAN_LIMIT`].
    fn live_after(&self, items: &[Item], at: usize, slot: u32) -> bool {
        let b = self.block_of[at];
        let w = self.words;
        let (word, bit) = (slot as usize / 64, 1u64 << (slot % 64));
        if word >= w {
            return true;
        }
        let rest = &items[at + 1..self.blocks[b].end];
        for item in rest.iter().take(SCAN_LIMIT) {
            if reads(&item.insn, self.results, slot) {
                return true;
            }
            if writes(&item.insn, self.results, slot) {
                return false;
            }
        }
        rest.len() > SCAN_LIMIT || self.live_out[b * w + word] & bit != 0
    }
}

// ---- passes --------------------------------------------------------------------

fn as_move(insn: &Insn) -> Option<(u32, u32)> {
    match *insn {
        Insn::Plain {
            op: Op::Mov,
            operands,
        } => Some((operands.a as u32, operands.b as u32)),
        _ => None,
    }
}

/// `%t = mov %x` for each argument, straight before a call whose window is
/// exactly those temporaries: pass the variables' slots instead, when they
/// are consecutive, the temporaries are dead after the call, and (for a
/// host call) the new argument window stays disjoint from the result.
fn coalesce_arguments(body: &mut Body, live: &Liveness) -> u32 {
    let items = &body.items;
    let mut remove = vec![false; items.len()];
    let mut retarget: Vec<(usize, u32)> = Vec::new();
    for (q, item) in items.iter().enumerate() {
        let Insn::Call(site) = item.insn else {
            continue;
        };
        let k = site.args.count as usize;
        if k == 0 || q < k {
            continue;
        }
        let group = &items[q - k..q];
        let moves: Option<Vec<(u32, u32)>> = group.iter().map(|i| as_move(&i.insn)).collect();
        let Some(moves) = moves else {
            continue;
        };
        let x0 = moves[0].1;
        let in_order = moves
            .iter()
            .enumerate()
            .all(|(i, &(t, x))| t == site.args.first + i as u32 && x == x0 + i as u32);
        let sources = SlotRange {
            first: x0,
            count: k as u32,
        };
        let disjoint = !sources.overlaps(site.args);
        let host_ok = !matches!(site.target, crate::function::CallTarget::Host(_))
            || !sources.overlaps(site.result);
        // Each temporary is read only by the call.
        let dead = site
            .args
            .slots()
            .all(|t| !live.live_after(items, q, t) || writes(&item.insn, live.results, t));
        if in_order && disjoint && host_ok && dead && live.block_of[q - k] == live.block_of[q] {
            for flag in &mut remove[q - k..q] {
                *flag = true;
            }
            retarget.push((q, x0));
        }
    }
    let count = remove.iter().filter(|&&r| r).count() as u32;
    for (q, x0) in retarget {
        if let Insn::Call(site) = &mut body.items[q].insn {
            site.args.first = x0;
        }
    }
    let mut index = 0;
    body.items.retain(|_| {
        index += 1;
        !remove[index - 1]
    });
    count
}

/// In a loop entered only at its top, a literal loaded into a slot that
/// only the next instruction reads moves to a fresh slot, loaded once
/// before the loop.
fn hoist_literals(body: &mut Body, live: &Liveness, frame: u32) -> u32 {
    let items = &body.items;
    // Where each label is defined, and the first and last branch to it.
    let labels = body.labels as usize;
    let mut label_at = vec![usize::MAX; labels];
    let mut sources = vec![(usize::MAX, 0usize); labels];
    for (i, item) in items.iter().enumerate() {
        match item.insn {
            Insn::Label(n) => label_at[n as usize] = i,
            Insn::Branch { target, .. } => {
                let range = &mut sources[target as usize];
                *range = (range.0.min(i), range.1.max(i));
            }
            _ => {}
        }
    }
    let mut next_slot = frame;
    // (insert before item, slot, value, type, span)
    let mut preheader: Vec<(usize, u32, u64, ValueType, eitri_base::Span)> = Vec::new();
    let mut rewrite: Vec<(usize, u32, u32)> = Vec::new();
    let mut remove = vec![false; items.len()];
    for (j, item) in items.iter().enumerate() {
        let Insn::Branch {
            branch: Branch::Jump,
            target,
        } = item.insn
        else {
            continue;
        };
        let header = label_at[target as usize];
        if header >= j {
            continue;
        }
        // Single entry: every branch to a label in the region comes from
        // inside it, so the region is entered only by falling into its top.
        let inside = |at: usize| (header..=j).contains(&at);
        let entered_from_outside = items[header..=j].iter().any(|other| {
            matches!(other.insn, Insn::Label(n)
                if sources[n as usize].0 != usize::MAX
                    && !(inside(sources[n as usize].0) && inside(sources[n as usize].1)))
        });
        if entered_from_outside {
            continue;
        }
        // This loop's hoisted literals, by value and type.
        let mut hoisted: FxHashMap<(u64, ValueType), u32> = FxHashMap::default();
        for p in header..j {
            let Insn::Load { dst, value, ty } = items[p].insn else {
                continue;
            };
            let user = &items[p + 1];
            if remove[p] || !rewritable(&user.insn, dst) {
                continue;
            }
            let used_once = reads(&user.insn, live.results, dst)
                && (writes(&user.insn, live.results, dst) || !live.live_after(items, p + 1, dst));
            if !used_once {
                continue;
            }
            let slot = match hoisted.get(&(value, ty)) {
                Some(&slot) => slot,
                // No encodable slot left above the frame: keep the load.
                None if next_slot as i64 > MAX_SLOT => continue,
                None => {
                    let slot = next_slot;
                    next_slot += 1;
                    hoisted.insert((value, ty), slot);
                    preheader.push((header, slot, value, ty, items[p].span));
                    slot
                }
            };
            remove[p] = true;
            rewrite.push((p + 1, dst, slot));
        }
    }
    let count = remove.iter().filter(|&&r| r).count() as u32;
    for (at, from, to) in rewrite {
        replace_reads(&mut body.items[at].insn, from, to);
    }
    // One merge: preheader loads in insertion order, each before its loop.
    preheader.sort_by_key(|h| h.0);
    let mut pending = preheader.iter().peekable();
    let mut out = Vec::with_capacity(body.items.len() + preheader.len());
    for (i, item) in std::mem::take(&mut body.items).into_iter().enumerate() {
        while let Some(&(_, slot, value, ty, span)) = pending.next_if(|h| h.0 == i) {
            out.push(Item::new(
                Insn::Load {
                    dst: slot,
                    value,
                    ty,
                },
                span,
            ));
        }
        if !remove[i] {
            out.push(item);
        }
    }
    body.items = out;
    count
}

/// Whether every read of `slot` in `insn` is one [`replace_reads`] can
/// redirect: a plain instruction's source operand or a branch operand.
fn rewritable(insn: &Insn, slot: u32) -> bool {
    match *insn {
        Insn::Plain { op, operands } => {
            let columns = [operands.a, operands.b, operands.c];
            let roles = op.info().roles;
            let read_in_place = roles
                .iter()
                .zip(columns)
                .any(|(&role, value)| role == Role::SrcDst && value == slot as i64);
            !read_in_place
        }
        Insn::Branch { .. } => true,
        _ => false,
    }
}

/// Replace reads of slot `from` with `to`, leaving writes alone.
fn replace_reads(insn: &mut Insn, from: u32, to: u32) {
    let swap = |slot: &mut u32| {
        if *slot == from {
            *slot = to;
        }
    };
    match insn {
        Insn::Plain { op, operands } => {
            let roles = op.info().roles;
            let Operands { a, b, c, .. } = operands;
            for (role, value) in roles.iter().zip([a, b, c]) {
                if matches!(role, Role::Src) && *value == from as i64 {
                    *value = to as i64;
                }
            }
        }
        Insn::Branch { branch, .. } => match branch {
            Branch::Compare { a, b, .. } => {
                swap(a);
                swap(b);
            }
            Branch::CompareImm { a, .. } | Branch::Test { a, .. } => swap(a),
            Branch::Jump => {}
        },
        // A literal feeding a call argument is not hoisted: the window is
        // fixed and must stay contiguous.
        _ => {}
    }
}
