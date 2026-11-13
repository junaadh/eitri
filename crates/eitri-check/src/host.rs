//! Host modules: declaration files the host registers, checked once.
//!
//! A host module is an Eitri declaration file of bodiless `pub fn`s (see
//! [Embedding: declaration files](../../../docs/implementation/embedding.md#declaration-files)).
//! Registration parses and checks it with the ordinary frontend and keeps
//! only the signatures, so scripts that import the module never re-parse
//! it. Binding implementations to the declarations is T010.

use eitri_base::{IndexVec, Interner, SourceId, Symbol};
use eitri_diag::{Code, Diagnostic, Diagnostics, Label};
use eitri_syntax::ast::TypeExpr;
use eitri_syntax::{LexScratch, ModuleKind, ParseScratch, Tokens, lex, parse};

use eitri_types::{PortableSignature, TypeStore};

use crate::ty::{Ty, TypeName, type_name};

eitri_base::newtype_index! {
    /// A registered host module.
    pub struct HostModuleId;
    /// A host function, numbered across all modules: the index a host
    /// call instruction carries.
    pub struct HostFnId;
}

#[derive(Clone, Debug)]
pub struct HostFn {
    pub module: HostModuleId,
    pub name: Symbol,
    pub params: Vec<Ty>,
    pub ret: Ty,
}

#[derive(Clone, Debug)]
pub struct HostModule {
    /// The import path scripts use, such as `"test"`.
    pub path: String,
    pub fns: Vec<HostFnId>,
}

/// Every host module registered with a context.
#[derive(Default)]
pub struct Hosts {
    types: TypeStore,
    signatures: IndexVec<HostFnId, PortableSignature>,
    modules: IndexVec<HostModuleId, HostModule>,
    fns: IndexVec<HostFnId, HostFn>,
}

/// Declaration diagnostics refer to the declaration text as this source.
pub const DECLARATION: SourceId = SourceId::from_u32(0);

impl Hosts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse and check the declaration file `text` and register it under
    /// the import path `path`. Its names are committed to `interner`, so
    /// they survive later per-run resets. On error nothing is registered;
    /// diagnostics refer to `text` as [`DECLARATION`].
    pub fn register(
        &mut self,
        path: &str,
        text: &str,
        interner: &mut Interner,
    ) -> Result<HostModuleId, Vec<Diagnostic>> {
        let mut diagnostics = Diagnostics::new(100);
        if self.find(path).is_some() {
            diagnostics.push(
                Diagnostic::error(
                    Code::DuplicateName,
                    format!("host module `{path}` is already registered"),
                )
                .with_label(Label::primary(DECLARATION, Default::default())),
            );
            return Err(diagnostics.into_vec());
        }
        // Drop spellings left by a previous run before committing new ones.
        interner.reset();
        let mut lex_scratch = LexScratch::new();
        let mut parse_scratch = ParseScratch::new();
        let tokens = lex(
            text,
            DECLARATION,
            interner,
            &mut diagnostics,
            &mut lex_scratch,
        );
        let ast = parse(
            text,
            DECLARATION,
            tokens,
            ModuleKind::Host,
            &mut diagnostics,
            &mut parse_scratch,
        );
        let module = self.modules.next_index();
        let mut fns = Vec::new();
        for decl in ast.fns.iter() {
            let name = tokens.symbol(decl.name);
            let span = tokens.span(decl.name);
            if fns.iter().any(|f: &HostFn| f.name == name) {
                diagnostics.push(error(
                    Code::DuplicateName,
                    format!("duplicate function `{}`", interner.resolve(name)),
                    span,
                ));
            }
            let params = ast
                .params(decl.params)
                .iter()
                .map(|p| value_type(p.ty, tokens, interner, &mut diagnostics))
                .collect();
            let ret = match decl.ret {
                TypeExpr::Named(t) if tokens.symbol(t) == eitri_base::sym::VOID => Ty::Void,
                ty => value_type(ty, tokens, interner, &mut diagnostics),
            };
            fns.push(HostFn {
                module,
                name,
                params,
                ret,
            });
        }
        if !ast
            .items
            .iter()
            .all(|item| matches!(item, eitri_syntax::ast::Item::Fn(_)))
            || !ast.imports.is_empty()
        {
            diagnostics.push(error(
                Code::Unsupported,
                "not supported yet: host modules with anything but `pub fn` declarations",
                Default::default(),
            ));
        }
        if diagnostics.has_errors() {
            interner.reset();
            return Err(diagnostics.into_vec());
        }
        let signatures = fns
            .iter()
            .map(|function| {
                let params = function
                    .params
                    .iter()
                    .map(|ty| {
                        self.types
                            .intern(ty.descriptor().expect("checked parameter type"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let result = self
                    .types
                    .intern(function.ret.descriptor().expect("checked return type"))?;
                self.types.signature(&params, result)
            })
            .collect::<Result<Vec<_>, eitri_types::TypeError>>()
            .map_err(|_| {
                vec![error(
                    Code::Limit,
                    "host signature exceeds type encoding limits",
                    Default::default(),
                )]
            })?;
        interner.commit();
        for signature in signatures {
            self.signatures.push(signature);
        }
        let ids = fns.into_iter().map(|f| self.fns.push(f)).collect();
        Ok(self.modules.push(HostModule {
            path: path.to_owned(),
            fns: ids,
        }))
    }

    pub fn find(&self, path: &str) -> Option<HostModuleId> {
        self.modules
            .iter_enumerated()
            .find(|(_, m)| m.path == path)
            .map(|(id, _)| id)
    }

    pub fn module(&self, id: HostModuleId) -> &HostModule {
        &self.modules[id]
    }

    pub fn function(&self, id: HostFnId) -> &HostFn {
        &self.fns[id]
    }

    /// The function `name` in `module`.
    pub fn lookup(&self, module: HostModuleId, name: Symbol) -> Option<HostFnId> {
        self.modules[module]
            .fns
            .iter()
            .copied()
            .find(|&f| self.fns[f].name == name)
    }

    /// Cached structural signature; never compare context-local type IDs.
    pub fn signature(&self, id: HostFnId) -> &PortableSignature {
        &self.signatures[id]
    }

    pub fn function_count(&self) -> usize {
        self.fns.len()
    }
}

fn error(code: Code, message: impl Into<String>, span: eitri_base::Span) -> Diagnostic {
    Diagnostic::error(code, message).with_label(Label::primary(DECLARATION, span))
}

/// A parameter or result type in a declaration: `i64` or `bool`.
fn value_type(
    ty: TypeExpr,
    tokens: &Tokens,
    interner: &Interner,
    diagnostics: &mut Diagnostics,
) -> Ty {
    let TypeExpr::Named(token) = ty else {
        return Ty::Error;
    };
    let span = tokens.span(token);
    let name = interner.resolve(tokens.symbol(token));
    match type_name(tokens.symbol(token)) {
        TypeName::Supported(Ty::Void) => {
            diagnostics.push(error(
                Code::TypeMismatch,
                "`void` can only be a return type",
                span,
            ));
            Ty::Error
        }
        TypeName::Unsupported => {
            diagnostics.push(error(
                Code::Unsupported,
                format!("not supported yet: type `{name}`"),
                span,
            ));
            Ty::Error
        }
        TypeName::Supported(ty) => ty,
        TypeName::Unknown => {
            diagnostics.push(error(
                Code::UnknownName,
                format!("unknown type `{name}`"),
                span,
            ));
            Ty::Error
        }
    }
}
