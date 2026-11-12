//! Anonymous split-union geometry and compact transfer plans; no execution.
use crate::layout::{Shape, aligned};
use crate::{FieldLayout, Layout, LayoutError, LayoutId, StorageDomain, TraceDesc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub offset: usize,
    pub bytes: usize,
}
/// Eight-byte slot indexes relative to the synthetic value, composed with
/// enclosing byte offsets. Creation remains internal pending dynamic support.
/// ```compile_fail
/// let _ = eitri_types::DynamicRoot { type_slot: 0, payload_slot: 1 };
/// ```
/// No inactive dynamic type/payload encoding is selected here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DynamicRoot {
    pub type_slot: u32,
    pub payload_slot: u32,
}
#[derive(Debug)]
pub struct ScalarTransfer {
    pub source: ByteRange,
    pub destination: usize,
}
#[derive(Debug)]
pub struct Transfer {
    pub source: usize,
    pub references: usize,
    pub scalars: usize,
    pub layout: LayoutId,
}
#[derive(Debug)]
pub enum CopyKind {
    /// No split-copy rule is supplied for dynamic type/payload storage yet.
    Dynamic,
    Scalar,
    Reference,
    Product(Vec<Transfer>),
    Repeat {
        count: u32,
        source_stride: usize,
        reference_stride: usize,
        scalar_stride: usize,
        element: LayoutId,
    },
    Split {
        reference_source: usize,
        scalars: [ScalarTransfer; 2],
    },
}
/// For has_dynamic=false, describes packing source values into reference/scalar
/// destinations. Consumers must provide non-overlapping source/destination
/// storage. Active values and reference positions must be initialized. Scalar
/// spans may include padding/inactive bytes: an eventual executor must copy
/// opaque, provenance-preserving (possibly uninitialized) storage, never read
/// those bytes as typed values. Repeats/child links keep size O(graph + fields),
/// never O(array count). Padding has no roots and need not carry a value.
#[derive(Debug)]
pub struct CopyPlan {
    pub reference_bytes: usize,
    pub scalar_bytes: usize,
    pub scalar_alignment: usize,
    /// If true, split-copy execution is unsupported: dynamic payloads must not
    /// be treated as ordinary scalars or references using these byte counts.
    /// Trace recipes remain available independently of a split-copy decision.
    pub has_dynamic: bool,
    pub kind: CopyKind,
}
/// Clear the entire reference partition to initialized null BEFORE transferring
/// the active payload, then write the tag. Inactive positions remain null.
/// These are plan data, not a publication/GC protocol or runtime constructor.
#[derive(Debug)]
pub struct InitPlan {
    pub null_references: ByteRange,
    pub tag: u8,
}
#[derive(Debug)]
pub struct CaseLayout {
    pub payload: Option<LayoutId>,
    pub transfer: Option<Transfer>,
    pub init: InitPlan,
}
#[derive(Debug)]
pub struct UnionLayout {
    pub tag: ByteRange,
    pub references: ByteRange,
    pub scalars: ByteRange,
    /// Optional: null/some. Result: error/success. Tags are internal, not ABI.
    pub cases: [CaseLayout; 2],
}
fn add(a: usize, b: usize) -> Result<usize, LayoutError> {
    a.checked_add(b)
        .filter(|&n| n <= isize::MAX as usize)
        .ok_or(LayoutError::Overflow)
}
fn mul(a: usize, b: usize) -> Result<usize, LayoutError> {
    a.checked_mul(b)
        .filter(|&n| n <= isize::MAX as usize)
        .ok_or(LayoutError::Overflow)
}

pub(crate) fn plan<'a>(
    node: Shape<'_>,
    layout: &Layout,
    child: impl Fn(usize) -> Result<&'a Layout, LayoutError>,
) -> Result<CopyPlan, LayoutError> {
    let mut result = CopyPlan {
        reference_bytes: 0,
        scalar_bytes: layout.size,
        scalar_alignment: layout.alignment,
        has_dynamic: false,
        kind: CopyKind::Scalar,
    };
    match node {
        Shape::ManagedReference(_) => {
            result.reference_bytes = layout.size;
            result.scalar_bytes = 0;
            result.scalar_alignment = 1;
            result.kind = CopyKind::Reference;
        }
        Shape::Dynamic => {
            result.has_dynamic = true;
            result.kind = CopyKind::Dynamic;
        }
        Shape::Product(_) => {
            let (mut refs, mut scalars, mut alignment) = (0, 0, 1);
            let mut transfers = Vec::new();
            for field in &layout.fields {
                let copy = &child(field.layout.0 as usize)?.copy;
                scalars = aligned(scalars, copy.scalar_alignment)?;
                transfers.push(Transfer {
                    source: field.offset,
                    references: refs,
                    scalars,
                    layout: field.layout,
                });
                refs = add(refs, copy.reference_bytes)?;
                scalars = add(scalars, copy.scalar_bytes)?;
                alignment = alignment.max(copy.scalar_alignment);
                result.has_dynamic |= copy.has_dynamic;
            }
            result.reference_bytes = refs;
            result.scalar_bytes = aligned(scalars, alignment)?;
            result.scalar_alignment = alignment;
            result.kind = CopyKind::Product(transfers);
        }
        Shape::Array(count, element) => {
            let copy = &child(element)?.copy;
            let scalar_stride = aligned(copy.scalar_bytes, copy.scalar_alignment)?;
            result.reference_bytes = mul(count as usize, copy.reference_bytes)?;
            result.scalar_bytes = mul(count as usize, scalar_stride)?;
            result.scalar_alignment = copy.scalar_alignment;
            result.has_dynamic = copy.has_dynamic;
            result.kind = CopyKind::Repeat {
                count,
                source_stride: layout.array_stride.expect("array layout"),
                reference_stride: copy.reference_bytes,
                scalar_stride,
                element: LayoutId(element as u32),
            };
        }
        _ => {}
    }
    Ok(result)
}

pub(crate) fn union<'a>(
    domain: StorageDomain,
    cases: [Option<usize>; 2],
    child: impl Fn(usize) -> Result<&'a Layout, LayoutError>,
) -> Result<Layout, LayoutError> {
    if domain == StorageDomain::ExternC {
        return Err(LayoutError::Unsupported);
    }
    let (mut refs, mut scalars, mut scalar_align) = (0, 0, 1);
    for id in cases.into_iter().flatten() {
        let copy = &child(id)?.copy;
        // Inactive dynamic payloads need a representation decision, not a guess.
        if copy.has_dynamic {
            return Err(LayoutError::Unsupported);
        }
        refs = refs.max(copy.reference_bytes);
        scalars = scalars.max(copy.scalar_bytes);
        scalar_align = scalar_align.max(copy.scalar_alignment);
    }
    let reference_size = if domain == StorageDomain::Frame {
        8
    } else {
        size_of::<*const ()>()
    };
    let ref_align = if refs == 0 {
        1
    } else if domain == StorageDomain::Frame {
        8
    } else {
        align_of::<*const ()>()
    };
    let tag = ByteRange {
        offset: 0,
        bytes: if domain == StorageDomain::Frame { 8 } else { 1 },
    };
    let references = ByteRange {
        offset: aligned(tag.bytes, ref_align)?,
        bytes: refs,
    };
    let scalar_offset = aligned(add(references.offset, refs)?, scalar_align)?;
    let scalars = ByteRange {
        offset: scalar_offset,
        bytes: aligned(scalars, scalar_align)?,
    };
    let alignment = ref_align
        .max(scalar_align)
        .max(if domain == StorageDomain::Frame { 8 } else { 1 });
    let size = aligned(add(scalar_offset, scalars.bytes)?, alignment)?;
    let scalar_payload = aligned(tag.bytes, scalar_align)?;
    let copy = CopyPlan {
        reference_bytes: refs,
        scalar_bytes: add(scalar_payload, scalars.bytes)?,
        scalar_alignment: scalar_align.max(if domain == StorageDomain::Frame { 8 } else { 1 }),
        has_dynamic: false,
        kind: CopyKind::Split {
            reference_source: references.offset,
            scalars: [
                ScalarTransfer {
                    source: tag,
                    destination: 0,
                },
                ScalarTransfer {
                    source: scalars,
                    destination: scalar_payload,
                },
            ],
        },
    };
    let make_case = |tag: u8, id: Option<usize>| CaseLayout {
        payload: id.map(|id| LayoutId(id as u32)),
        transfer: id.map(|id| Transfer {
            source: 0,
            references: references.offset,
            scalars: scalars.offset,
            layout: LayoutId(id as u32),
        }),
        init: InitPlan {
            null_references: references,
            tag,
        },
    };
    let cases = [make_case(0, cases[0]), make_case(1, cases[1])];
    Ok(Layout {
        array_stride: None,
        size,
        alignment,
        fields: Vec::<FieldLayout>::new(),
        copy,
        trace: if refs == 0 {
            TraceDesc::None
        } else {
            TraceDesc::ReferencePartition {
                offset: references.offset,
                bytes: refs,
                stride: reference_size,
            }
        },
        union: Some(UnionLayout {
            tag,
            references,
            scalars,
            cases,
        }),
    })
}
