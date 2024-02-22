use crate::{
    def::{
        scope::{module_scopes, Scope, ScopeDef, ScopeHirId, ScopeId, Scopes, VariableDef},
        ExprId, Function, ModuleSourceMap,
    },
    source_map,
    typeck::{builtins::builtin_globals, intrinsics::intrinsic_functions},
    Db, Name,
};
use rustc_hash::FxHashMap;
use starpls_common::File;
use starpls_syntax::{TextRange, TextSize};
use std::collections::hash_map::Entry;

/// Resolves things like variables, function definition, etc. For now this is implemented as a simple list
/// of "module" scopes that hold variable declarations, but will need to be updated later to support other
/// features, e.g. type declarations, builtins, etc.
pub(crate) struct Resolver<'a> {
    db: &'a dyn Db,
    file: File,
    scopes: &'a Scopes,
    scope_chain: Vec<ScopeId>,
}

#[derive(Clone, Debug)]
pub(crate) enum Export {
    Variable(ExprId),
    Function(Function),
}

impl<'a> Resolver<'a> {
    pub(crate) fn resolve_export_in_file(
        db: &'a dyn Db,
        file: File,
        name: &Name,
    ) -> Option<Export> {
        Self::new_for_module(db, file).resolve_export(name)
    }

    pub(crate) fn resolve_export(&self, name: &Name) -> Option<Export> {
        self.scopes().find_map(|scope| {
            scope
                .declarations
                .get(name)
                .and_then(|decls| decls.last())
                .and_then(|decl| {
                    Some(match decl {
                        ScopeDef::Variable(VariableDef { expr, .. }) => Export::Variable(*expr),
                        ScopeDef::Function(func) => Export::Function(*func),
                        _ => return None,
                    })
                })
        })
    }

    pub(crate) fn resolve_name(&self, name: &Name) -> Option<Vec<ScopeDef>> {
        // Check module scopes first.
        for scope in self.scopes() {
            if let Some(declarations) = scope.declarations.get(&name) {
                return Some(declarations.clone());
            }
        }

        // Fall back to the builtins scope.
        self.resolve_name_in_builtins(name)
    }

    fn resolve_name_in_builtins(&self, name: &Name) -> Option<Vec<ScopeDef>> {
        intrinsic_functions(self.db)
            .functions(self.db)
            .get(name)
            .copied()
            .map(|func| vec![ScopeDef::IntrinsicFunction(func)])
            .or_else(|| self.resolve_name_in_builtin_globals(name))
    }

    fn resolve_name_in_builtin_globals(&self, name: &Name) -> Option<Vec<ScopeDef>> {
        let globals = builtin_globals(self.db, self.file.dialect(self.db));
        globals
            .functions(self.db)
            .get(name.as_str())
            .copied()
            .map(|func| vec![ScopeDef::BuiltinFunction(func)])
            .or_else(|| {
                globals
                    .variables(self.db)
                    .get(name.as_str())
                    .cloned()
                    .map(|type_ref| vec![ScopeDef::BuiltinVariable(type_ref)])
            })
    }

    pub(crate) fn names(&self) -> FxHashMap<Name, ScopeDef> {
        // Add names from this module.
        let mut names = self.module_names();

        // Add names from Starlark intrinsics.
        for (key, func) in intrinsic_functions(self.db).functions(self.db).iter() {
            names.insert(key.clone(), ScopeDef::IntrinsicFunction(*func));
        }

        // Add global functions from third-party builtins (e.g. Bazel builtins).
        let globals = builtin_globals(self.db, self.file.dialect(self.db));
        for (name, func) in globals.functions(self.db).iter() {
            names.insert(Name::from_str(&name), ScopeDef::BuiltinFunction(*func));
        }

        // Add global variables from third-party builtins (e.g. Bazel builtins).
        for (name, type_ref) in globals.variables(self.db).iter() {
            names.insert(
                Name::from_str(&name),
                ScopeDef::BuiltinVariable(type_ref.clone()),
            );
        }

        names
    }

    pub(crate) fn module_names(&self) -> FxHashMap<Name, ScopeDef> {
        let mut names = FxHashMap::default();
        for scope in self.scopes() {
            for (name, decl) in scope.declarations.iter() {
                if let Entry::Vacant(entry) = names.entry(name.clone()) {
                    if let Some(decl) = decl.first().cloned() {
                        entry.insert(decl);
                    }
                }
            }
        }
        names
    }

    fn scopes(&self) -> impl Iterator<Item = &Scope> {
        self.scope_chain
            .iter()
            .rev()
            .map(|scope| &self.scopes.scopes[*scope])
    }

    pub(crate) fn new_for_module(db: &'a dyn Db, file: File) -> Self {
        let scopes = module_scopes(db, file).scopes(db);
        let scope = scopes.scope_for_hir_id(ScopeHirId::Module);
        Self::from_parts(db, file, scopes, scope)
    }

    pub(crate) fn new_for_expr(db: &'a dyn Db, file: File, expr: ExprId) -> Self {
        let scopes = module_scopes(db, file).scopes(db);
        let scope = scopes.scope_for_hir_id(expr);
        Self::from_parts(db, file, scopes, scope)
    }

    pub(crate) fn new_for_offset(db: &'a dyn Db, file: File, offset: TextSize) -> Self {
        let scopes = module_scopes(db, file).scopes(db);
        let source_map = source_map(db, file);
        let scope = scopes
            .scopes_by_hir_id
            .iter()
            .map(|(hir, scope)| {
                let ptr = match *hir {
                    ScopeHirId::Module => source_map.root.syntax_node_ptr(),
                    ScopeHirId::Expr(expr) => source_map
                        .expr_map_back
                        .get(&expr)
                        .unwrap()
                        .syntax_node_ptr(),
                    ScopeHirId::Stmt(stmt) => source_map
                        .stmt_map_back
                        .get(&stmt)
                        .unwrap()
                        .syntax_node_ptr(),
                };
                (ptr.text_range(), *scope)
            })
            .filter(|(range, _)| range.start() <= offset && offset <= range.end())
            .min_by_key(|(range, _)| range.len())
            .map(|(hir_range, scope)| {
                find_nearest_predecessor(&scopes, &source_map, hir_range, offset).unwrap_or(scope)
            });
        Self::from_parts(db, file, scopes, scope)
    }

    fn from_parts(db: &'a dyn Db, file: File, scopes: &'a Scopes, scope: Option<ScopeId>) -> Self {
        let mut scope_chain = scopes.scope_chain(scope).collect::<Vec<_>>();
        scope_chain.reverse();
        Self {
            db,
            file,
            scopes,
            scope_chain,
        }
    }
}

fn find_nearest_predecessor(
    scopes: &Scopes,
    source_map: &ModuleSourceMap,
    hir_range: TextRange,
    offset: TextSize,
) -> Option<ScopeId> {
    scopes
        .scopes_by_hir_id
        .iter()
        .map(|(hir, scope)| {
            let ptr = match *hir {
                ScopeHirId::Module => source_map.root.syntax_node_ptr(),
                ScopeHirId::Expr(expr) => source_map
                    .expr_map_back
                    .get(&expr)
                    .unwrap()
                    .syntax_node_ptr(),
                ScopeHirId::Stmt(stmt) => source_map
                    .stmt_map_back
                    .get(&stmt)
                    .unwrap()
                    .syntax_node_ptr(),
            };
            (ptr.text_range(), *scope)
        })
        .filter(|(range, _)| {
            range.start() <= offset && hir_range.contains_range(*range) && hir_range != *range
        })
        .max_by(|(lhs, _), (rhs, _)| {
            if lhs.contains_range(*rhs) {
                std::cmp::Ordering::Greater
            } else if rhs.contains_range(*lhs) {
                std::cmp::Ordering::Less
            } else {
                lhs.start().cmp(&rhs.start())
            }
        })
        .map(|(_, scope)| scope)
}
