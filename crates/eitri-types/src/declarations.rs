//! Caller-owned named-product graph. Constructors remain internal until T017.
use crate::{TypeId, TypeStore};
use eitri_base::FxHashMap;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeclKey {
    pub module: String,
    pub name: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeclId(pub(crate) u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    Type(TypeId),
    Named(DeclId),
    Reference(DeclId),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeclError {
    DuplicateKey(DeclKey),
    SameKeyDifferentShape { key: DeclKey },
    UnknownDecl(DeclId),
    UnknownType(TypeId),
    AlreadyDefined(DeclId),
    DuplicateField(String),
    Undefined(DeclId),
    InlineCycle { keys: Vec<DeclKey> },
    Layout(crate::LayoutError),
    TooMany,
    TooLarge,
}
struct Declaration {
    key: DeclKey,
    fields: Option<Vec<(String, FieldType)>>,
}
#[derive(Default)]
pub struct Declarations {
    declarations: Vec<Declaration>,
    keys: FxHashMap<DeclKey, DeclId>,
}

#[allow(dead_code)] // Internal construction until the language feature lands.
impl Declarations {
    pub(crate) fn declare(&mut self, key: DeclKey) -> Result<DeclId, DeclError> {
        if self.keys.contains_key(&key) {
            return Err(DeclError::DuplicateKey(key));
        }
        let id = DeclId(u32::try_from(self.declarations.len()).map_err(|_| DeclError::TooMany)?);
        self.declarations.push(Declaration {
            key: key.clone(),
            fields: None,
        });
        self.keys.insert(key, id);
        Ok(id)
    }
    pub(crate) fn define(
        &mut self,
        id: DeclId,
        fields: Vec<(String, FieldType)>,
    ) -> Result<(), DeclError> {
        let decl = self
            .declarations
            .get(id.0 as usize)
            .ok_or(DeclError::UnknownDecl(id))?;
        if decl.fields.is_some() {
            return Err(DeclError::AlreadyDefined(id));
        }
        let mut names = eitri_base::FxHashSet::default();
        for (name, ty) in &fields {
            if !names.insert(name) {
                return Err(DeclError::DuplicateField(name.clone()));
            }
            if let FieldType::Named(target) | FieldType::Reference(target) = ty
                && self.declarations.get(target.0 as usize).is_none()
            {
                return Err(DeclError::UnknownDecl(*target));
            }
        }
        self.declarations[id.0 as usize].fields = Some(fields);
        Ok(())
    }
    /// O(declarations + fields) time and space, with an explicit DFS stack.
    pub(crate) fn check(&self) -> Result<(), DeclError> {
        for (n, decl) in self.declarations.iter().enumerate() {
            if decl.fields.is_none() {
                return Err(DeclError::Undefined(DeclId(n as u32)));
            }
        }
        let mut state = vec![0u8; self.declarations.len()];
        let mut path: Vec<usize> = Vec::new();
        let mut stack = Vec::new();
        for root in 0..state.len() {
            if state[root] != 0 {
                continue;
            }
            stack.push((root, 0usize));
            state[root] = 1;
            path.push(root);
            while let Some((id, next)) = stack.last_mut() {
                let fields = self.declarations[*id]
                    .fields
                    .as_ref()
                    .expect("checked above");
                if *next == fields.len() {
                    state[*id] = 2;
                    stack.pop();
                    path.pop();
                    continue;
                }
                let ty = fields[*next].1;
                *next += 1;
                if let FieldType::Named(child) = ty {
                    let child = child.0 as usize;
                    if state[child] == 1 {
                        let start = path.iter().position(|&n| n == child).expect("active path");
                        return Err(DeclError::InlineCycle {
                            keys: path[start..]
                                .iter()
                                .map(|&n| self.declarations[n].key.clone())
                                .collect(),
                        });
                    }
                    if state[child] == 0 {
                        state[child] = 1;
                        stack.push((child, 0));
                        path.push(child);
                    }
                }
            }
        }
        Ok(())
    }
    pub(crate) fn layout(
        &self,
        types: &TypeStore,
        root: DeclId,
        domain: crate::StorageDomain,
    ) -> Result<crate::LayoutStore, DeclError> {
        self.check()?;
        if self.declarations.get(root.0 as usize).is_none() {
            return Err(DeclError::UnknownDecl(root));
        }
        for decl in &self.declarations {
            for (_, ty) in decl.fields.as_ref().expect("checked") {
                if let FieldType::Type(id) = ty
                    && types.nodes.get(id.0 as usize).is_none()
                {
                    return Err(DeclError::UnknownType(*id));
                }
            }
        }
        crate::layout::named_layout(types, self, root, domain).map_err(DeclError::Layout)
    }
    /// Snapshot a signature with named products using the same anonymous type
    /// bytes as `TypeStore::signature`. Named and reference tags precede either
    /// a key and ordered shape or a back-reference distance to an active path
    /// entry. Unique declaration keys make these path encodings canonical under
    /// `same_type`'s coinductive equality, independent of local IDs and sharing.
    ///
    /// Every declaration is expanded at most once per path. With N declarations
    /// and F maximum fields, unfolding can take O((F+1)^N) nodes before the cap;
    /// a wide graph can also exceed it directly. Output is capped at 1 MiB and
    /// returns `TooLarge` before appending beyond it. Time and temporary space
    /// are O(B + N), where B is emitted bytes (up to the cap), with expected
    /// constant-time path lookup; hash collisions can increase lookup time.
    /// Traversal uses an explicit stack, including for deep graphs.
    pub fn signature(
        &self,
        types: &TypeStore,
        params: &[FieldType],
        result: FieldType,
    ) -> Result<crate::PortableSignature, DeclError> {
        self.signature_with_limit(types, params, result, 1_048_576)
    }

    fn signature_with_limit(
        &self,
        types: &TypeStore,
        params: &[FieldType],
        result: FieldType,
        limit: usize,
    ) -> Result<crate::PortableSignature, DeclError> {
        fn append(out: &mut Vec<u8>, bytes: &[u8], limit: usize) -> Result<(), DeclError> {
            if out
                .len()
                .checked_add(bytes.len())
                .filter(|&n| n <= limit)
                .is_none()
            {
                return Err(DeclError::TooLarge);
            }
            out.extend_from_slice(bytes);
            Ok(())
        }
        fn number(out: &mut Vec<u8>, n: usize, limit: usize) -> Result<(), DeclError> {
            append(
                out,
                &u32::try_from(n)
                    .map_err(|_| DeclError::TooLarge)?
                    .to_le_bytes(),
                limit,
            )
        }
        fn string(out: &mut Vec<u8>, s: &str, limit: usize) -> Result<(), DeclError> {
            number(out, s.len(), limit)?;
            append(out, s.as_bytes(), limit)
        }
        // Anonymous tags occupy 0..=11; these tags extend the same prefix grammar.
        const NAMED: u8 = 12;
        const REFERENCE: u8 = 13;
        const BACKREF: u8 = 14;
        const DEFINITION: u8 = 15;
        enum Work {
            Type(FieldType),
            Fields(DeclId, usize),
            Exit(DeclId),
        }
        let mut out = Vec::new();
        number(&mut out, params.len(), limit)?;
        let mut active = FxHashMap::<DeclId, usize>::default();
        let mut depth = 0usize;
        let mut stack = Vec::new();
        for root in params.iter().copied().chain(std::iter::once(result)) {
            stack.push(Work::Type(root));
            while let Some(work) = stack.pop() {
                match work {
                    Work::Type(FieldType::Type(id)) => {
                        let node = types
                            .nodes
                            .get(id.0 as usize)
                            .ok_or(DeclError::UnknownType(id))?;
                        append(&mut out, &node.1, limit)?;
                    }
                    Work::Type(ty @ (FieldType::Named(_) | FieldType::Reference(_))) => {
                        let (id, tag) = match ty {
                            FieldType::Named(id) => (id, NAMED),
                            FieldType::Reference(id) => (id, REFERENCE),
                            FieldType::Type(_) => unreachable!(),
                        };
                        let decl = self
                            .declarations
                            .get(id.0 as usize)
                            .ok_or(DeclError::UnknownDecl(id))?;
                        append(&mut out, &[tag], limit)?;
                        if let Some(&index) = active.get(&id) {
                            append(&mut out, &[BACKREF], limit)?;
                            number(&mut out, depth - 1 - index, limit)?;
                            continue;
                        }
                        let fields = decl.fields.as_ref().ok_or(DeclError::Undefined(id))?;
                        append(&mut out, &[DEFINITION], limit)?;
                        string(&mut out, &decl.key.module, limit)?;
                        string(&mut out, &decl.key.name, limit)?;
                        number(&mut out, fields.len(), limit)?;
                        active.insert(id, depth);
                        depth += 1;
                        stack.push(Work::Exit(id));
                        stack.push(Work::Fields(id, 0));
                    }
                    Work::Fields(id, index) => {
                        let fields = self.declarations[id.0 as usize]
                            .fields
                            .as_ref()
                            .expect("defined above");
                        if let Some((name, ty)) = fields.get(index) {
                            string(&mut out, name, limit)?;
                            stack.push(Work::Fields(id, index + 1));
                            stack.push(Work::Type(*ty));
                        }
                    }
                    Work::Exit(id) => {
                        active.remove(&id);
                        depth -= 1;
                    }
                }
            }
        }
        Ok(crate::PortableSignature {
            encoding: out.into_boxed_slice(),
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.declarations.len()
    }
    pub(crate) fn fields(&self, id: usize) -> &[(String, FieldType)] {
        self.declarations[id].fields.as_deref().expect("checked")
    }
}

/// Compare reachable named products coinductively across independent stores.
/// An explicit stack and set of assumed-equal pairs terminate reference cycles.
/// At most A*B pairs are examined (normally only reachable pairs), for A and B
/// declaration counts. Time is proportional to their fields, names and portable
/// type-encoding bytes, with expected constant-time set operations; hash
/// collisions can increase this. Space is O(A*B + queued field edges).
/// Invalid or undefined reachable IDs return their corresponding errors.
pub fn same_type(
    a: (&Declarations, &TypeStore, DeclId),
    b: (&Declarations, &TypeStore, DeclId),
) -> Result<bool, DeclError> {
    let mut stack = vec![(a.2, b.2, None::<DeclKey>)];
    let mut assumed = eitri_base::FxHashSet::default();
    while let Some((left, right, parent)) = stack.pop() {
        let l =
            a.0.declarations
                .get(left.0 as usize)
                .ok_or(DeclError::UnknownDecl(left))?;
        let r =
            b.0.declarations
                .get(right.0 as usize)
                .ok_or(DeclError::UnknownDecl(right))?;
        if l.key != r.key {
            return match parent {
                Some(key) => Err(DeclError::SameKeyDifferentShape { key }),
                None => Ok(false),
            };
        }
        if !assumed.insert((left, right)) {
            continue;
        }
        let lf = l.fields.as_ref().ok_or(DeclError::Undefined(left))?;
        let rf = r.fields.as_ref().ok_or(DeclError::Undefined(right))?;
        let mismatch = || DeclError::SameKeyDifferentShape { key: l.key.clone() };
        if lf.len() != rf.len() {
            return Err(mismatch());
        }
        for ((ln, lt), (rn, rt)) in lf.iter().zip(rf) {
            if ln != rn {
                return Err(mismatch());
            }
            match (*lt, *rt) {
                (FieldType::Type(x), FieldType::Type(y)) => {
                    let x =
                        a.1.nodes
                            .get(x.0 as usize)
                            .ok_or(DeclError::UnknownType(x))?;
                    let y =
                        b.1.nodes
                            .get(y.0 as usize)
                            .ok_or(DeclError::UnknownType(y))?;
                    if x.1 != y.1 {
                        return Err(mismatch());
                    }
                }
                (FieldType::Named(x), FieldType::Named(y))
                | (FieldType::Reference(x), FieldType::Reference(y)) => {
                    stack.push((x, y, Some(l.key.clone())));
                }
                _ => return Err(mismatch()),
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScalarKind, StorageDomain, TraceDesc, TypeDesc};
    fn key(n: &str) -> DeclKey {
        DeclKey {
            module: "m".into(),
            name: n.into(),
        }
    }
    fn field(n: &str, t: FieldType) -> (String, FieldType) {
        (n.into(), t)
    }
    #[test]
    fn cross_store_identity_and_shape_errors() {
        let mut ta = TypeStore::default();
        let ai = ta.intern(TypeDesc::Scalar(ScalarKind::I64)).unwrap();
        let mut tb = TypeStore::default();
        let bb = tb.intern(TypeDesc::Scalar(ScalarKind::Bool)).unwrap();
        let bi = tb.intern(TypeDesc::Scalar(ScalarKind::I64)).unwrap();
        let mut a = Declarations::default();
        let aa = a.declare(key("A")).unwrap();
        a.define(
            aa,
            vec![
                field("x", FieldType::Type(ai)),
                field("y", FieldType::Type(ai)),
            ],
        )
        .unwrap();
        let mut b = Declarations::default();
        let other = b.declare(key("Other")).unwrap();
        let ba = b.declare(key("A")).unwrap();
        b.define(
            other,
            vec![
                field("x", FieldType::Type(bi)),
                field("y", FieldType::Type(bi)),
            ],
        )
        .unwrap();
        b.define(
            ba,
            vec![
                field("x", FieldType::Type(bi)),
                field("y", FieldType::Type(bi)),
            ],
        )
        .unwrap();
        assert_eq!(same_type((&a, &ta, aa), (&b, &tb, ba)), Ok(true));
        assert_eq!(same_type((&a, &ta, aa), (&b, &tb, other)), Ok(false));
        for fields in [
            vec![
                field("y", FieldType::Type(bi)),
                field("x", FieldType::Type(bi)),
            ],
            vec![
                field("x", FieldType::Type(bb)),
                field("y", FieldType::Type(bi)),
            ],
        ] {
            let mut c = Declarations::default();
            let ca = c.declare(key("A")).unwrap();
            c.define(ca, fields).unwrap();
            assert_eq!(
                same_type((&a, &ta, aa), (&c, &tb, ca)),
                Err(DeclError::SameKeyDifferentShape { key: key("A") })
            );
        }
    }

    #[test]
    fn recursive_identity_and_deep_difference() {
        let types = TypeStore::default();
        let mut a = Declarations::default();
        let aa = a.declare(key("A")).unwrap();
        let ab = a.declare(key("B")).unwrap();
        a.define(
            aa,
            vec![
                field("self", FieldType::Reference(aa)),
                field("b", FieldType::Reference(ab)),
            ],
        )
        .unwrap();
        a.define(ab, vec![field("a", FieldType::Reference(aa))])
            .unwrap();
        let mut b = Declarations::default();
        let bb = b.declare(key("B")).unwrap();
        let ba = b.declare(key("A")).unwrap();
        b.define(
            ba,
            vec![
                field("self", FieldType::Reference(ba)),
                field("b", FieldType::Reference(bb)),
            ],
        )
        .unwrap();
        b.define(bb, vec![field("a", FieldType::Reference(ba))])
            .unwrap();
        assert_eq!(same_type((&a, &types, aa), (&b, &types, ba)), Ok(true));
        let mut c = Declarations::default();
        let ca = c.declare(key("A")).unwrap();
        let cb = c.declare(key("B")).unwrap();
        c.define(
            ca,
            vec![
                field("self", FieldType::Reference(ca)),
                field("b", FieldType::Reference(cb)),
            ],
        )
        .unwrap();
        c.define(cb, vec![field("different", FieldType::Reference(ca))])
            .unwrap();
        assert_eq!(
            same_type((&a, &types, aa), (&c, &types, ca)),
            Err(DeclError::SameKeyDifferentShape { key: key("B") })
        );
    }

    #[test]
    fn compare_thousand_declaration_chain() {
        let types = TypeStore::default();
        let mut a = Declarations::default();
        let mut b = Declarations::default();
        let left: Vec<_> = (0..1000)
            .map(|i| a.declare(key(&i.to_string())).unwrap())
            .collect();
        let mut right: Vec<_> = (0..1000)
            .rev()
            .map(|i| b.declare(key(&i.to_string())).unwrap())
            .collect();
        right.reverse();
        for (l, r) in left.windows(2).zip(right.windows(2)) {
            a.define(l[0], vec![field("next", FieldType::Named(l[1]))])
                .unwrap();
            b.define(r[0], vec![field("next", FieldType::Named(r[1]))])
                .unwrap();
        }
        a.define(left[999], vec![]).unwrap();
        b.define(right[999], vec![]).unwrap();
        assert_eq!(
            same_type((&a, &types, left[0]), (&b, &types, right[0])),
            Ok(true)
        );
    }
    #[test]
    fn portable_signatures_match_identity() {
        let mut ta = TypeStore::default();
        let ai = ta.intern(TypeDesc::Scalar(ScalarKind::I64)).unwrap();
        let ao = ta.intern(TypeDesc::Optional(ai)).unwrap();
        let mut tb = TypeStore::default();
        tb.intern(TypeDesc::Void).unwrap();
        let bi = tb.intern(TypeDesc::Scalar(ScalarKind::I64)).unwrap();
        let bo = tb.intern(TypeDesc::Optional(bi)).unwrap();
        let mut a = Declarations::default();
        let aa = a.declare(key("A")).unwrap();
        let ab = a.declare(key("B")).unwrap();
        a.define(
            aa,
            vec![
                field("self", FieldType::Reference(aa)),
                field("b", FieldType::Named(ab)),
                field("value", FieldType::Type(ao)),
            ],
        )
        .unwrap();
        a.define(ab, vec![field("a", FieldType::Reference(aa))])
            .unwrap();
        let mut b = Declarations::default();
        let bb = b.declare(key("B")).unwrap();
        let ba = b.declare(key("A")).unwrap();
        b.define(
            ba,
            vec![
                field("self", FieldType::Reference(ba)),
                field("b", FieldType::Named(bb)),
                field("value", FieldType::Type(bo)),
            ],
        )
        .unwrap();
        b.define(bb, vec![field("a", FieldType::Reference(ba))])
            .unwrap();
        let sig = |d: &Declarations, t: &TypeStore, id| {
            d.signature(
                t,
                &[FieldType::Named(id), FieldType::Reference(id)],
                FieldType::Named(id),
            )
            .unwrap()
        };
        assert_eq!(same_type((&a, &ta, aa), (&b, &tb, ba)), Ok(true));
        assert_eq!(sig(&a, &ta, aa), sig(&b, &tb, ba));
        assert_eq!(
            a.signature(&ta, &[FieldType::Type(ao)], FieldType::Type(ai))
                .unwrap(),
            ta.signature(&[ao], ai).unwrap()
        );
        assert_ne!(
            a.signature(&ta, &[], FieldType::Named(aa)).unwrap(),
            a.signature(&ta, &[], FieldType::Reference(aa)).unwrap()
        );
        let mut c = Declarations::default();
        let ca = c.declare(key("A")).unwrap();
        let cb = c.declare(key("B")).unwrap();
        c.define(
            ca,
            vec![
                field("self", FieldType::Reference(ca)),
                field("b", FieldType::Named(cb)),
                field("value", FieldType::Type(bo)),
            ],
        )
        .unwrap();
        c.define(cb, vec![field("different", FieldType::Reference(ca))])
            .unwrap();
        assert!(matches!(
            same_type((&a, &ta, aa), (&c, &tb, ca)),
            Err(DeclError::SameKeyDifferentShape { .. })
        ));
        assert_ne!(sig(&a, &ta, aa), sig(&c, &tb, ca));
        assert_eq!(same_type((&a, &ta, aa), (&b, &tb, bb)), Ok(false));
        assert_ne!(sig(&a, &ta, aa), sig(&b, &tb, bb));
    }

    #[test]
    fn portable_signature_size_limits_wide_and_deep_graphs() {
        let mut types = TypeStore::default();
        let value = types.intern(TypeDesc::Void).unwrap();
        let mut wide = Declarations::default();
        let root = wide.declare(key("wide")).unwrap();
        let fields = (0..2000)
            .map(|n| field(&format!("field{n}"), FieldType::Type(value)))
            .collect();
        wide.define(root, fields).unwrap();
        let wide_sig = wide.signature(&types, &[], FieldType::Named(root)).unwrap();
        let size = wide_sig.heap_bytes();
        assert_eq!(
            wide.signature_with_limit(&types, &[], FieldType::Named(root), size)
                .unwrap(),
            wide_sig
        );
        assert_eq!(
            wide.signature_with_limit(&types, &[], FieldType::Named(root), size - 1),
            Err(DeclError::TooLarge)
        );
        let mut deep = Declarations::default();
        let ids: Vec<_> = (0..1000)
            .map(|n| deep.declare(key(&format!("{n}"))).unwrap())
            .collect();
        for pair in ids.windows(2) {
            deep.define(pair[0], vec![field("next", FieldType::Reference(pair[1]))])
                .unwrap();
        }
        deep.define(ids[999], vec![field("cycle", FieldType::Reference(ids[0]))])
            .unwrap();
        let deep_sig = deep
            .signature(&types, &[], FieldType::Named(ids[0]))
            .unwrap();
        let size = deep_sig.heap_bytes();
        assert_eq!(
            deep.signature_with_limit(&types, &[], FieldType::Named(ids[0]), size)
                .unwrap(),
            deep_sig
        );
        assert_eq!(
            deep.signature_with_limit(&types, &[], FieldType::Named(ids[0]), size - 1),
            Err(DeclError::TooLarge)
        );
        let mut huge = Declarations::default();
        let id = huge.declare(key("huge")).unwrap();
        let fields = (0..1000)
            .map(|n| (format!("{n}{}", "x".repeat(1100)), FieldType::Type(value)))
            .collect();
        huge.define(id, fields).unwrap();
        assert_eq!(
            huge.signature(&types, &[], FieldType::Named(id)),
            Err(DeclError::TooLarge)
        );
        let mut long = Declarations::default();
        let ids: Vec<_> = (0..1000)
            .map(|n| {
                long.declare(key(&format!("{n}{}", "x".repeat(1100))))
                    .unwrap()
            })
            .collect();
        for pair in ids.windows(2) {
            long.define(pair[0], vec![field("next", FieldType::Reference(pair[1]))])
                .unwrap();
        }
        long.define(ids[999], vec![]).unwrap();
        assert_eq!(
            long.signature(&types, &[], FieldType::Named(ids[0])),
            Err(DeclError::TooLarge)
        );
    }

    #[test]
    fn portable_signature_rejects_invalid_inputs() {
        let types = TypeStore::default();
        let mut decls = Declarations::default();
        let id = decls.declare(key("undefined")).unwrap();
        assert_eq!(
            decls.signature(&types, &[], FieldType::Named(id)),
            Err(DeclError::Undefined(id))
        );
        assert_eq!(
            decls.signature(&types, &[], FieldType::Reference(DeclId(999))),
            Err(DeclError::UnknownDecl(DeclId(999)))
        );
        assert_eq!(
            decls.signature(&types, &[], FieldType::Type(TypeId(999))),
            Err(DeclError::UnknownType(TypeId(999)))
        );
    }
    #[test]
    fn declaration_errors_and_inline_cycles() {
        let mut g = Declarations::default();
        let a = g.declare(key("A")).unwrap();
        assert_eq!(g.declare(key("A")), Err(DeclError::DuplicateKey(key("A"))));
        assert_eq!(g.check(), Err(DeclError::Undefined(a)));
        assert_eq!(
            g.define(DeclId(99), vec![]),
            Err(DeclError::UnknownDecl(DeclId(99)))
        );
        assert_eq!(
            g.define(
                a,
                vec![
                    field("x", FieldType::Named(a)),
                    field("x", FieldType::Named(a))
                ]
            ),
            Err(DeclError::DuplicateField("x".into()))
        );
        g.define(a, vec![field("x", FieldType::Named(a))]).unwrap();
        assert_eq!(g.define(a, vec![]), Err(DeclError::AlreadyDefined(a)));
        assert_eq!(
            g.check(),
            Err(DeclError::InlineCycle {
                keys: vec![key("A")]
            })
        );
        let mut g = Declarations::default();
        let a = g.declare(key("A")).unwrap();
        let b = g.declare(key("B")).unwrap();
        g.define(a, vec![field("b", FieldType::Named(b))]).unwrap();
        g.define(b, vec![field("a", FieldType::Named(a))]).unwrap();
        assert_eq!(
            g.check(),
            Err(DeclError::InlineCycle {
                keys: vec![key("A"), key("B")]
            })
        );
    }
    #[test]
    fn references_and_inline_layouts() {
        let mut types = TypeStore::default();
        let i = types.intern(TypeDesc::Scalar(ScalarKind::I64)).unwrap();
        let mut g = Declarations::default();
        let a = g.declare(key("A")).unwrap();
        let b = g.declare(key("B")).unwrap();
        let c = g.declare(key("C")).unwrap();
        g.define(
            a,
            vec![
                field("value", FieldType::Type(i)),
                field("next", FieldType::Reference(a)),
                field("b", FieldType::Reference(b)),
            ],
        )
        .unwrap();
        g.define(b, vec![field("a", FieldType::Reference(a))])
            .unwrap();
        g.define(
            c,
            vec![
                field("nested", FieldType::Named(a)),
                field("other", FieldType::Type(i)),
            ],
        )
        .unwrap();
        g.check().unwrap();
        let layout = g.layout(&types, c, StorageDomain::Frame).unwrap();
        let top = layout.get(layout.root()).unwrap();
        let nested = layout.get(top.fields[0].layout).unwrap();
        assert_eq!(
            nested.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
            vec![0, 8, 16]
        );
        assert_eq!(top.fields[1].offset, 24);
        let nodes = [
            crate::layout::Shape::Scalar(ScalarKind::I64),
            crate::layout::Shape::ManagedReference(0),
            crate::layout::Shape::Product(&[0, 1, 1]),
            crate::layout::Shape::Product(&[2, 0]),
        ];
        let anonymous =
            crate::LayoutStore::build(nodes.len(), 3, StorageDomain::Frame, |id| Ok(nodes[id]))
                .unwrap();
        let expected = anonymous.get(anonymous.root()).unwrap();
        assert_eq!(
            (top.size, top.alignment),
            (expected.size, expected.alignment)
        );
        assert_eq!(
            top.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
            expected.fields.iter().map(|f| f.offset).collect::<Vec<_>>()
        );
        assert!(matches!(nested.trace, TraceDesc::Product(_)));
        assert!(matches!(
            layout.get(nested.fields[1].layout).unwrap().trace,
            TraceDesc::Reference
        ));
    }
    #[test]
    fn deep_chain_uses_no_recursion() {
        let mut g = Declarations::default();
        let ids: Vec<_> = (0..10_000)
            .map(|i| g.declare(key(&i.to_string())).unwrap())
            .collect();
        for pair in ids.windows(2) {
            g.define(pair[0], vec![field("next", FieldType::Named(pair[1]))])
                .unwrap();
        }
        g.define(*ids.last().unwrap(), vec![]).unwrap();
        g.check().unwrap();
    }
}
