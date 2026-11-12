//! Layout arithmetic only: no allocation of script objects or pointer tracing.
use super::{ScalarKind, TypeDesc, TypeId, TypeStore};
use crate::{CopyKind, CopyPlan, DynamicRoot, UnionLayout};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageDomain {
    Frame,
    Heap,
    ExternC,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutId(pub(crate) u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    InvalidId,
    InlineCycle,
    Overflow,
    Unsupported,
    InvalidAlignment,
}

#[derive(Debug)]
pub struct FieldLayout {
    pub offset: usize,
    pub layout: LayoutId,
}
/// Reference locations are byte offsets. Product entries compose child maps;
/// Repeat never expands per element, even for nested arrays. Reference and
/// ReferencePartition describe ordinary managed positions; Dynamic is a
/// conditional type/payload recipe. Extern pointers/host identities are untraced.
#[derive(Debug)]
pub enum TraceDesc {
    Dynamic(DynamicRoot),
    ReferencePartition {
        offset: usize,
        bytes: usize,
        stride: usize,
    },
    None,
    Reference,
    Product(Vec<FieldLayout>),
    Repeat {
        count: u32,
        stride: usize,
        element: LayoutId,
    },
}
#[derive(Debug)]
pub struct Layout {
    pub union: Option<UnionLayout>,
    pub copy: CopyPlan,
    /// Array element spacing, including padding; None for other shapes.
    pub array_stride: Option<usize>,
    pub size: usize,
    pub alignment: usize,
    pub fields: Vec<FieldLayout>,
    pub trace: TraceDesc,
}
impl Layout {
    /// Slots covering this byte extent; use Frame layouts for VM field offsets.
    pub fn frame_slots(&self) -> usize {
        self.size.div_ceil(8)
    }
}

pub struct LayoutStore {
    layouts: Vec<Option<Layout>>,
    root: LayoutId,
}

// Borrowed, anonymous layout inputs. Only tests construct products and leaves;
// there is deliberately no declaration identity or public graph builder.
#[derive(Clone, Copy)]
#[allow(dead_code)] // Synthetic leaves are exercised before language support.
pub(crate) enum Shape<'a> {
    Optional(usize),
    Result(usize, usize),
    Dynamic,
    Scalar(ScalarKind),
    Void,
    Product(&'a [usize]),
    Array(u32, usize),
    ManagedReference(usize),
    ExternPointer,
    // Geometry supplied by the future handle implementation; not a frozen ABI.
    HostIdentity { size: usize, alignment: usize },
}

pub(crate) fn aligned(size: usize, alignment: usize) -> Result<usize, LayoutError> {
    if !alignment.is_power_of_two() {
        return Err(LayoutError::InvalidAlignment);
    }
    size.checked_add(alignment - 1)
        .map(|n| n & !(alignment - 1))
        .filter(|&n| n <= isize::MAX as usize)
        .ok_or(LayoutError::Overflow)
}

impl LayoutStore {
    /// Compute reachable layouts without recursion. N is the source store's
    /// node count; E is reachable inline edges/fields. Time/space O(N + E),
    /// independent of array counts, with explicit DFS state and shared children.
    /// IDs belong to the returned store; no cache mixes different TypeStores.
    /// Sizes/offsets are checked and limited to isize::MAX bytes. Frame uses
    /// eight-byte scalar slots; heap keeps compact native scalar geometry.
    /// ExternC follows target C field order/alignment and rejects managed leaves.
    /// Anonymous Optional/Result use split unions; ExternC rejects them.
    pub fn for_type(
        types: &TypeStore,
        root: TypeId,
        domain: StorageDomain,
    ) -> Result<Self, LayoutError> {
        Self::build(types.nodes.len(), root.0 as usize, domain, |id| {
            Ok(match types.nodes.get(id).ok_or(LayoutError::InvalidId)?.0 {
                TypeDesc::Scalar(kind) => Shape::Scalar(kind),
                TypeDesc::Void => Shape::Void,
                TypeDesc::FixedArray { count, element } => Shape::Array(count, element.0 as usize),
                TypeDesc::Optional(child) => Shape::Optional(child.0 as usize),
                TypeDesc::Result { error, value } => {
                    Shape::Result(error.0 as usize, value.0 as usize)
                }
            })
        })
    }
    pub fn root(&self) -> LayoutId {
        self.root
    }
    pub fn get(&self, id: LayoutId) -> Option<&Layout> {
        self.layouts.get(id.0 as usize)?.as_ref()
    }

    fn build<'a>(
        count: usize,
        root: usize,
        domain: StorageDomain,
        shape: impl Fn(usize) -> Result<Shape<'a>, LayoutError>,
    ) -> Result<Self, LayoutError> {
        if root >= count {
            return Err(LayoutError::InvalidId);
        }
        u32::try_from(count).map_err(|_| LayoutError::Overflow)?;
        let mut store = Self {
            layouts: (0..count).map(|_| None).collect(),
            root: LayoutId(root as u32),
        };
        let mut active = vec![false; count];
        let mut stack = vec![(root, false)];
        while let Some((id, finish)) = stack.pop() {
            if id >= count {
                return Err(LayoutError::InvalidId);
            }
            if store.layouts[id].is_some() {
                continue;
            }
            let node = shape(id)?;
            if !finish {
                if active[id] {
                    return Err(LayoutError::InlineCycle);
                }
                active[id] = true;
                stack.push((id, true));
                match node {
                    Shape::Product(fields) => {
                        stack.extend(fields.iter().rev().map(|&child| (child, false)))
                    }
                    Shape::Array(_, child) | Shape::Optional(child) => stack.push((child, false)),
                    Shape::Result(error, value) => stack.extend([(value, false), (error, false)]),
                    Shape::ManagedReference(target) if target >= count => {
                        return Err(LayoutError::InvalidId);
                    }
                    _ => {}
                }
                continue;
            }
            let child = |id: usize| {
                store
                    .layouts
                    .get(id)
                    .and_then(Option::as_ref)
                    .ok_or(LayoutError::InvalidId)
            };
            let cases = match node {
                Shape::Optional(child) => Some([None, Some(child)]),
                Shape::Result(error, value) => Some([Some(error), Some(value)]),
                _ => None,
            };
            if let Some(cases) = cases {
                store.layouts[id] = Some(crate::split::union(domain, cases, child)?);
                active[id] = false;
                continue;
            }
            let mut fields = Vec::new();
            let mut array_stride = None;
            let (size, alignment, trace) = match node {
                Shape::Optional(_) | Shape::Result(_, _) => unreachable!("handled above"),
                Shape::Dynamic => {
                    if domain == StorageDomain::ExternC {
                        return Err(LayoutError::Unsupported);
                    }
                    (
                        16,
                        8,
                        TraceDesc::Dynamic(DynamicRoot {
                            type_slot: 0,
                            payload_slot: 1,
                        }),
                    )
                }
                Shape::Void => (0, 1, TraceDesc::None),
                Shape::Scalar(kind) => {
                    let (size, align) = match kind {
                        ScalarKind::I8 => (size_of::<i8>(), align_of::<i8>()),
                        ScalarKind::U8 => (size_of::<u8>(), align_of::<u8>()),
                        ScalarKind::Bool => (size_of::<bool>(), align_of::<bool>()),
                        ScalarKind::F32 => (size_of::<f32>(), align_of::<f32>()),
                        ScalarKind::Char if domain == StorageDomain::ExternC => {
                            return Err(LayoutError::Unsupported);
                        }
                        ScalarKind::Char => (size_of::<char>(), align_of::<char>()),
                        ScalarKind::I64 => (size_of::<i64>(), align_of::<i64>()),
                        ScalarKind::U64 => (size_of::<u64>(), align_of::<u64>()),
                        ScalarKind::F64 => (size_of::<f64>(), align_of::<f64>()),
                    };
                    (size, align, TraceDesc::None)
                }
                Shape::ExternPointer => (
                    size_of::<*const ()>(),
                    align_of::<*const ()>(),
                    TraceDesc::None,
                ),
                Shape::HostIdentity { size, alignment } => {
                    if domain == StorageDomain::ExternC {
                        return Err(LayoutError::Unsupported);
                    }
                    (aligned(size, alignment)?, alignment, TraceDesc::None)
                }
                Shape::ManagedReference(_) => {
                    if domain == StorageDomain::ExternC {
                        return Err(LayoutError::Unsupported);
                    }
                    (
                        size_of::<*const ()>(),
                        align_of::<*const ()>(),
                        TraceDesc::Reference,
                    )
                }
                Shape::Array(count, element) => {
                    let layout = child(element)?;
                    let stride = aligned(layout.size, layout.alignment)?;
                    array_stride = Some(stride);
                    let size = stride
                        .checked_mul(count as usize)
                        .ok_or(LayoutError::Overflow)?;
                    let trace = if count == 0 || matches!(layout.trace, TraceDesc::None) {
                        TraceDesc::None
                    } else {
                        TraceDesc::Repeat {
                            count,
                            stride,
                            element: LayoutId(element as u32),
                        }
                    };
                    (size, layout.alignment, trace)
                }
                Shape::Product(children) => {
                    let (mut end, mut alignment) = (0usize, 1usize);
                    let mut traces = Vec::new();
                    for &id in children {
                        let layout = child(id)?;
                        let offset = aligned(end, layout.alignment)?;
                        end = offset
                            .checked_add(layout.size)
                            .ok_or(LayoutError::Overflow)?;
                        alignment = alignment.max(layout.alignment);
                        fields.push(FieldLayout {
                            offset,
                            layout: LayoutId(id as u32),
                        });
                        if !matches!(layout.trace, TraceDesc::None) {
                            traces.push(FieldLayout {
                                offset,
                                layout: LayoutId(id as u32),
                            });
                        }
                    }
                    (
                        aligned(end, alignment)?,
                        alignment,
                        if traces.is_empty() {
                            TraceDesc::None
                        } else {
                            TraceDesc::Product(traces)
                        },
                    )
                }
            };
            let (size, alignment) = if domain == StorageDomain::Frame {
                (aligned(size, 8)?, alignment.max(8))
            } else {
                (aligned(size, alignment)?, alignment)
            };
            let mut layout = Layout {
                union: None,
                copy: CopyPlan {
                    reference_bytes: 0,
                    scalar_bytes: 0,
                    scalar_alignment: 1,
                    has_dynamic: false,
                    kind: CopyKind::Scalar,
                },
                array_stride,
                size,
                alignment,
                fields,
                trace,
            };
            layout.copy = crate::split::plan(node, &layout, child)?;
            store.layouts[id] = Some(layout);
            active[id] = false;
        }
        Ok(store)
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "union_tests.rs"]
mod union_tests;
