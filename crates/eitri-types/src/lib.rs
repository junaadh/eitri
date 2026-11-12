//! Context-local canonical types; owned signatures compare across stores.
use eitri_base::FxHashMap;

mod split;
pub use split::{
    ByteRange, CaseLayout, CopyKind, CopyPlan, DynamicRoot, InitPlan, ScalarTransfer, Transfer,
    UnionLayout,
};
mod layout;
pub use layout::{
    FieldLayout, Layout, LayoutError, LayoutId, LayoutStore, StorageDomain, TraceDesc,
};

/// A local index: meaningful only with the store that issued it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TypeId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ScalarKind {
    I64 = 1,
    I8,
    U8,
    U64,
    F32,
    F64,
    Char,
    Bool,
}

/// Compound construction is reserved to this crate's tests until T017.
/// External consumers can construct only scalar/void descriptions.
/// ```compile_fail
/// use eitri_types::{TypeDesc, TypeStore};
/// let mut store = TypeStore::default();
/// let id = store.intern(TypeDesc::Void).unwrap();
/// let _ = TypeDesc::Optional(id);
/// ```
/// ```compile_fail
/// use eitri_types::{TypeDesc, TypeStore};
/// let mut store = TypeStore::default();
/// let id = store.intern(TypeDesc::Void).unwrap();
/// let _ = TypeDesc::FixedArray { count: 1, element: id };
/// ```
/// ```compile_fail
/// use eitri_types::{TypeDesc, TypeStore};
/// let mut store = TypeStore::default();
/// let id = store.intern(TypeDesc::Void).unwrap();
/// let _ = TypeDesc::Result { error: id, value: id };
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypeDesc {
    Scalar(ScalarKind),
    Void,
    #[non_exhaustive]
    Optional(TypeId),
    #[non_exhaustive]
    FixedArray {
        count: u32,
        element: TypeId,
    },
    #[non_exhaustive]
    Result {
        error: TypeId,
        value: TypeId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeError {
    InvalidId,
    TooLarge,
}

// Limits expanded compound descriptions, not array storage or element counts.
const MAX_TYPE_BYTES: usize = 65_536;
const MAX_SIGNATURE_BYTES: usize = u32::MAX as usize;

#[derive(Default)]
pub struct TypeStore {
    nodes: Vec<(TypeDesc, Box<[u8]>)>,
    ids: FxHashMap<TypeDesc, TypeId>,
}

fn add_size(a: usize, b: usize, limit: usize) -> Result<usize, TypeError> {
    a.checked_add(b)
        .filter(|&n| n <= limit)
        .ok_or(TypeError::TooLarge)
}

impl TypeStore {
    fn node(&self, id: TypeId) -> Result<&(TypeDesc, Box<[u8]>), TypeError> {
        self.nodes.get(id.0 as usize).ok_or(TypeError::InvalidId)
    }

    /// Canonicalize within this store. Child IDs must come from this store.
    /// Children precede parents: cyclic/forward descriptors cannot be built.
    /// Expected O(1) hash lookup plus O(expanded description bytes) for a new
    /// node; worst-case hash collisions cost O(nodes). Each encoding is at
    /// most 65,536 bytes; total retained space is O(sum of node encodings).
    /// All size/index arithmetic is checked before changing the store.
    pub fn intern(&mut self, desc: TypeDesc) -> Result<TypeId, TypeError> {
        if let TypeDesc::Optional(child) = desc
            && matches!(self.node(child)?.0, TypeDesc::Optional(_))
        {
            return Ok(child);
        }
        if let Some(&id) = self.ids.get(&desc) {
            return Ok(id);
        }
        let (prefix, children): (Vec<u8>, &[TypeId]) = match &desc {
            TypeDesc::Void => (vec![0], &[]),
            TypeDesc::Scalar(kind) => (vec![*kind as u8], &[]),
            TypeDesc::Optional(child) => (vec![9], std::slice::from_ref(child)),
            TypeDesc::FixedArray { count, element } => {
                let mut prefix = vec![10];
                prefix.extend_from_slice(&count.to_le_bytes());
                (prefix, std::slice::from_ref(element))
            }
            TypeDesc::Result { error, value } => {
                // The pair is handled below without borrowing a temporary.
                let size = add_size(1, self.node(*error)?.1.len(), MAX_TYPE_BYTES)?;
                add_size(size, self.node(*value)?.1.len(), MAX_TYPE_BYTES)?;
                let mut bytes = vec![11];
                bytes.extend_from_slice(&self.node(*error)?.1);
                bytes.extend_from_slice(&self.node(*value)?.1);
                return self.insert(desc, bytes);
            }
        };
        let mut size = prefix.len();
        for &child in children {
            size = add_size(size, self.node(child)?.1.len(), MAX_TYPE_BYTES)?;
        }
        let mut bytes = prefix;
        bytes.reserve(size - bytes.len());
        for &child in children {
            bytes.extend_from_slice(&self.node(child)?.1);
        }
        self.insert(desc, bytes)
    }

    fn insert(&mut self, desc: TypeDesc, bytes: Vec<u8>) -> Result<TypeId, TypeError> {
        let id = TypeId(u32::try_from(self.nodes.len()).map_err(|_| TypeError::TooLarge)?);
        self.nodes.push((desc, bytes.into_boxed_slice()));
        self.ids.insert(desc, id);
        Ok(id)
    }

    /// Snapshot an ordered signature. Prefix tags have fixed arities and array
    /// counts/count-of-parameters use little-endian u32, so equality is exact
    /// structural equality, independent of local IDs and graph sharing.
    /// Construction is O(B) time/space; B = 4 + sum(parameter encodings) +
    /// result encoding, checked to fit u32 before allocation. No recursion.
    pub fn signature(
        &self,
        params: &[TypeId],
        result: TypeId,
    ) -> Result<PortableSignature, TypeError> {
        let count = u32::try_from(params.len()).map_err(|_| TypeError::TooLarge)?;
        let mut size = add_size(4, self.node(result)?.1.len(), MAX_SIGNATURE_BYTES)?;
        for &id in params {
            size = add_size(size, self.node(id)?.1.len(), MAX_SIGNATURE_BYTES)?;
        }
        let mut bytes = Vec::with_capacity(size);
        bytes.extend_from_slice(&count.to_le_bytes());
        for &id in params {
            bytes.extend_from_slice(&self.node(id)?.1);
        }
        bytes.extend_from_slice(&self.node(result)?.1);
        Ok(PortableSignature {
            encoding: bytes.into_boxed_slice(),
        })
    }
}

/// Owned, store-independent structure; no local IDs, addresses or hashes.
/// Equality takes O(B) worst-case time, O(1) space and no allocations, with
/// B at most u32::MAX bytes per signature. With P scalar parameters B=P+5.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableSignature {
    encoding: Box<[u8]>,
}
impl PortableSignature {
    pub fn heap_bytes(&self) -> usize {
        self.encoding.len()
    }
}

#[cfg(test)]
mod tests;
