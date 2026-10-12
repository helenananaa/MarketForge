use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pine_ir::{PineType, Qualifier, ValueKind};
use pine_syntax::{
    BinaryOp, Diagnostic, ExportItem, Expr, ExprKind, FunctionBody, Program, Span, Stmt, StmtKind,
    SwitchArmResult, UnaryOp, UserTypeField, parse_source,
};

use crate::analyzer::context::{FunctionInfo, MethodInfo, MethodParamInfo};
use crate::analyzer::functions::{
    function_default_values, function_param_names, record_default_shadowing,
};
use crate::legacy::SourcePolicy;
use crate::source_graph::{AnalysisInput, SourceContextId, SourceId};
use crate::types::array_kind_from_element_type_name;

mod function_parameters;
use function_parameters::{
    imported_function_overloads, imported_function_param_types, module_function_param_types,
    same_parameter_signature,
};

mod alias_access;
mod imports;
mod model;
#[path = "modules_rewrite.rs"]
mod modules_rewrite;
mod side_effects;
#[cfg(test)]
#[path = "modules/source_context_tests.rs"]
mod source_context_tests;
mod version_policy;

use imports::{
    detect_import_cycles, imports_in_program, validate_library_imports, validate_root_imports,
};
use model::{
    ExportInfo, ModuleInfo, ModuleMethodInfo, ModuleMethodParamInfo, ModuleUserTypeFieldInfo,
    ModuleUserTypeIdentity, ModuleUserTypeInfo, imported_user_type_field_type,
    imported_user_type_scalar_field_type, module_user_type_fields_match,
};
pub(crate) use model::{
    ImportedUserTypeFieldInfo, ImportedUserTypeIdentity, ImportedUserTypeInfo, ModuleValidation,
};
use modules_rewrite::{RewriteContext, rewrite_expr, rewrite_function_body, rewrite_program};
use side_effects::{first_statement_span, function_body_has_side_effect, visit_statement_exprs};
pub(crate) fn visit_expression(expr: &Expr, visitor: &mut impl FnMut(&Expr)) {
    side_effects::visit_expr(expr, visitor);
}
use version_policy::validate_language_versions;

pub(crate) fn validate_modules(input: &AnalysisInput) -> ModuleValidation {
    validate_modules_inner(input, crate::PineDialect::V1, false)
}

#[cfg(test)]
pub(crate) fn validate_modules_with_implicit(
    input: &AnalysisInput,
    implicit_dialect: crate::PineDialect,
) -> ModuleValidation {
    validate_modules_inner(input, implicit_dialect, true)
}

fn validate_modules_inner(
    input: &AnalysisInput,
    implicit_dialect: crate::PineDialect,
    inherit_root_for_implicit_libraries: bool,
) -> ModuleValidation {
    let graph = input.source_graph();
    let mut diagnostics = Vec::new();
    let mut modules = Vec::with_capacity(graph.libraries().len() + 1);
    let root_parse = parse_source(graph.root().source());
    diagnostics.extend(root_parse.diagnostics.clone());
    let root_program = root_parse.program;
    modules.push(ModuleInfo {
        id: graph.root().id(),
        source: Arc::new(graph.root().source().clone()),
        key: None,
        program: root_program.clone(),
        exports: HashMap::new(),
        private_symbols: HashSet::new(),
        user_types: HashMap::new(),
        methods: HashMap::new(),
        functions: HashMap::new(),
        constants: HashMap::new(),
    });

    let mut library_index = HashMap::new();
    for library in graph.libraries() {
        let diagnostic_start = diagnostics.len();
        let parsed = parse_source(library.source());
        diagnostics.extend(parsed.diagnostics.clone());
        let mut module = ModuleInfo {
            id: library.id(),
            source: Arc::new(library.source().clone()),
            key: library.import_key().map(str::to_owned),
            program: parsed.program,
            exports: HashMap::new(),
            private_symbols: HashSet::new(),
            user_types: HashMap::new(),
            methods: HashMap::new(),
            functions: HashMap::new(),
            constants: HashMap::new(),
        };
        collect_library_declarations(&mut module, &mut diagnostics);
        module.attach_diagnostics(&mut diagnostics[diagnostic_start..]);
        if let Some(key) = &module.key {
            library_index.insert(key.clone(), modules.len());
        }
        modules.push(module);
    }

    validate_root_imports(&modules, &library_index, &mut diagnostics);
    validate_library_imports(&modules, &library_index, &mut diagnostics);
    detect_import_cycles(&modules, &library_index, &mut diagnostics);

    let root_policy = SourcePolicy::from_program_with_implicit(&root_program, implicit_dialect);
    validate_language_versions(
        &modules,
        &root_policy,
        implicit_dialect,
        inherit_root_for_implicit_libraries,
        &mut diagnostics,
    );
    let halt_before_analysis = diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "E_LEX_VERSION" || diagnostic.code.starts_with("E_LANGUAGE_VERSION_")
    });

    let import_plan = build_import_plan(&modules, &library_index, &mut diagnostics);
    let root_program = rewrite_program(&root_program, &import_plan.root_rewrites);

    ModuleValidation {
        source_texts: modules
            .iter()
            .map(|module| (module.id, Arc::clone(&module.source)))
            .collect(),
        source_context_origins: import_plan.source_context_origins,
        diagnostics,
        root_program,
        root_policy,
        halt_before_analysis,
        imported_functions: import_plan.imported_functions,
        imported_methods: import_plan.imported_methods,
        imported_user_types: import_plan.imported_user_types,
    }
}

fn collect_library_declarations(module: &mut ModuleInfo, diagnostics: &mut Vec<Diagnostic>) {
    let mut library_declarations = 0;
    let mut default_shadowed_names = HashSet::new();
    // Temporarily move the statements out so we can iterate by reference
    // without cloning the entire AST; the loop only mutates other fields of
    // `module`, never `module.program.statements`.
    let statements = std::mem::take(&mut module.program.statements);
    for statement in &statements {
        match &statement.kind {
            StmtKind::Library(_) => library_declarations += 1,
            StmtKind::Export(export) => match &export.item {
                ExportItem::Function {
                    name,
                    params,
                    body,
                    span,
                } => {
                    let param_types =
                        module_function_param_types(module, params, None, diagnostics);
                    let distinct_overload =
                        module.functions.get(name).is_some_and(|previous| {
                            std::iter::once(previous)
                                .chain(previous.overloads.iter())
                                .all(|candidate| {
                                    !same_parameter_signature(&candidate.param_types, &param_types)
                                })
                        }) && matches!(module.exports.get(name), Some(ExportInfo::Function { .. }));
                    if !distinct_overload {
                        register_export(
                            module,
                            name,
                            ExportInfo::Function { span: *span },
                            diagnostics,
                        );
                    }
                    if function_body_has_side_effect(body, &statements) {
                        diagnostics.push(Diagnostic::error(
                            "E_IMPORT_FUNCTION_SIDE_EFFECT",
                            format!("exported function `{name}` contains unsupported side effects"),
                            *span,
                        ));
                    }
                    let function = FunctionInfo {
                        overloads: Vec::new(),
                        display_name: name.clone(),
                        source_id: module.id,
                        // Library declarations are re-contextualized for each root import
                        // instance in `build_import_plan` before semantic analysis.
                        source_context_id: SourceContextId::root(),
                        params: function_param_names(params),
                        default_values: function_default_values(
                            params,
                            module.program.version.map_or(1, |version| version.version),
                            &default_shadowed_names,
                            diagnostics,
                        ),
                        param_types,
                        body: body.clone(),
                        span: *span,
                    };
                    if distinct_overload {
                        module
                            .functions
                            .get_mut(name)
                            .unwrap()
                            .overloads
                            .push(function);
                    } else {
                        module.functions.insert(name.clone(), function);
                    }
                }
                ExportItem::Const { name, value, span } => {
                    register_export(
                        module,
                        name,
                        ExportInfo::Const {
                            value: value.clone(),
                            span: *span,
                        },
                        diagnostics,
                    );
                    if !is_const_import_expr(value) {
                        diagnostics.push(Diagnostic::error(
                            "E_IMPORT_CONST_VALUE",
                            format!("exported constant `{name}` must be a const expression"),
                            value.span,
                        ));
                    }
                    module.constants.insert(name.clone(), value.clone());
                }
                ExportItem::UserType { decl, span } => {
                    let user_type = module_user_type_info(
                        module.id,
                        &decl.name,
                        &decl.fields,
                        *span,
                        diagnostics,
                    );
                    register_export(
                        module,
                        &decl.name,
                        ExportInfo::UserType {
                            identity: user_type.identity.clone(),
                            fields: user_type.fields.clone(),
                            span: *span,
                        },
                        diagnostics,
                    );
                    module.user_types.insert(decl.name.clone(), user_type);
                }
                ExportItem::Unknown { .. } => {}
            },
            StmtKind::Function { name, params, body } => {
                module.private_symbols.insert(name.clone());
                module.functions.insert(
                    name.clone(),
                    FunctionInfo {
                        overloads: Vec::new(),
                        display_name: name.clone(),
                        source_id: module.id,
                        // Library declarations are re-contextualized for each root import
                        // instance in `build_import_plan` before semantic analysis.
                        source_context_id: SourceContextId::root(),
                        params: function_param_names(params),
                        default_values: function_default_values(
                            params,
                            module.program.version.map_or(1, |version| version.version),
                            &default_shadowed_names,
                            diagnostics,
                        ),
                        param_types: module_function_param_types(module, params, None, diagnostics),
                        body: body.clone(),
                        span: statement.span,
                    },
                );
            }
            StmtKind::Decl { name, value, .. } => {
                module.private_symbols.insert(name.clone());
                if is_const_import_expr(value) {
                    module.constants.insert(name.clone(), value.clone());
                }
            }
            StmtKind::UserType(user_type) => {
                module.private_symbols.insert(user_type.name.clone());
                module.user_types.insert(
                    user_type.name.clone(),
                    module_user_type_info(
                        module.id,
                        &user_type.name,
                        &user_type.fields,
                        statement.span,
                        diagnostics,
                    ),
                );
            }
            StmtKind::Method(_) => {}
            _ => {}
        }
        record_default_shadowing(statement, &mut default_shadowed_names);
    }
    for statement in &statements {
        let StmtKind::Method(method) = &statement.kind else {
            continue;
        };
        let receiver_type_name = method
            .params
            .first()
            .map(|receiver| receiver.type_name.clone());
        let receiver_identity = receiver_type_name
            .as_deref()
            .and_then(|type_name| module.user_types.get(type_name))
            .map(|user_type| user_type.identity.clone());
        module.methods.insert(
            (
                receiver_type_name.clone().unwrap_or_default(),
                method.name.clone(),
            ),
            ModuleMethodInfo {
                receiver_type_name,
                receiver_identity,
                receiver_name: method
                    .params
                    .first()
                    .map(|receiver| receiver.name.clone())
                    .unwrap_or_default(),
                params: method
                    .params
                    .iter()
                    .skip(1)
                    .map(|param| ModuleMethodParamInfo {
                        name: param.name.clone(),
                        type_name: param.type_name.clone(),
                    })
                    .collect(),
                param_names: method
                    .params
                    .iter()
                    .map(|param| param.name.clone())
                    .collect(),
                body: method.body.clone(),
                span: statement.span,
            },
        );
    }
    // Restore the statements that were moved out above.
    module.program.statements = statements;

    if module.key.is_some() && library_declarations != 1 {
        diagnostics.push(Diagnostic::error(
            "E_IMPORT_INVALID_LIBRARY",
            "library source must contain exactly one library declaration",
            first_statement_span(&module.program).unwrap_or_else(|| Span::new(0, 0)),
        ));
    }
}

fn module_user_type_info(
    source_id: SourceId,
    name: &str,
    fields: &[UserTypeField],
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> ModuleUserTypeInfo {
    for field in fields {
        if let Some(value) = &field.default_value {
            let default_type = crate::analyzer::user_types::field_default_type(value);
            let target_type = imported_user_type_field_type(&field.type_name);
            let valid = default_type.is_some_and(|actual| {
                target_type.map_or(actual.kind == ValueKind::Na, |target| {
                    crate::types::can_assign(PineType::new(Qualifier::Series, target.kind), actual)
                })
            });
            if !valid {
                diagnostics.push(Diagnostic::error(
                    "E_UDT_FIELD_DEFAULT",
                    "UDT field defaults must be compatible literals or built-in variables",
                    value.span,
                ));
            }
        }
    }
    let identity = ModuleUserTypeIdentity {
        source_id,
        name: name.to_owned(),
    };
    let fields = fields
        .iter()
        .map(|field| ModuleUserTypeFieldInfo {
            default_value: field.default_value.clone(),
            varip: field.varip,
            name: field.name.clone(),
            type_name: field.type_name.clone(),
            pine_type: imported_user_type_field_type(&field.type_name),
            span: field.span,
        })
        .collect();
    ModuleUserTypeInfo {
        identity,
        fields,
        span,
    }
}

fn register_export(
    module: &mut ModuleInfo,
    name: &str,
    export: ExportInfo,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Some(existing) = module.exports.insert(name.to_owned(), export.clone()) {
        diagnostics.push(Diagnostic::error(
            "E_IMPORT_DUPLICATE_EXPORT",
            format!("duplicate export `{name}`"),
            existing.span().merge(export.span()),
        ));
    }
}

#[derive(Default)]
struct ImportPlan {
    source_context_origins: HashMap<SourceContextId, (SourceId, Option<String>)>,
    root_rewrites: RewriteContext,
    imported_functions: HashMap<String, FunctionInfo>,
    imported_methods: HashMap<(String, String), MethodInfo>,
    imported_user_types: HashMap<String, ImportedUserTypeInfo>,
}

fn build_import_plan(
    modules: &[ModuleInfo],
    library_index: &HashMap<String, usize>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ImportPlan {
    let mut plan = ImportPlan::default();
    plan.source_context_origins
        .insert(SourceContextId::root(), (SourceId::root(), None));
    let root_imports = imports_in_program(&modules[0].program);
    let mut contexts = root_imports
        .iter()
        .enumerate()
        .filter_map(|(index, import)| {
            Some((
                import.alias.as_ref()?.0.clone(),
                *library_index.get(&import.key)?,
                SourceContextId::import_instance(index),
                true,
            ))
        })
        .collect::<Vec<_>>();
    // One canonical context per dependency source, not one per path through
    // the graph. Diamonds and cycles cannot cause recursive body expansion.
    let dependency_indices: HashSet<_> = modules
        .iter()
        .skip(1)
        .flat_map(|module| imports_in_program(&module.program))
        .filter_map(|import| library_index.get(&import.key).copied())
        .collect();
    contexts.extend(
        modules
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(index, _)| dependency_indices.contains(index))
            .map(|(index, module)| {
                (
                    dependency_namespace(module),
                    index,
                    SourceContextId::import_instance(root_imports.len() + index - 1),
                    false,
                )
            }),
    );
    for (alias, module_index, source_context_id, is_root_import) in contexts {
        let module = &modules[module_index];
        let diagnostic_start = diagnostics.len();
        plan.source_context_origins
            .insert(source_context_id, (module.id, module.key.clone()));
        let mut module_context = rewrite_context_for_module(&alias, module);
        for import in imports_in_program(&module.program) {
            let (Some((dependency_alias, _)), Some(index)) =
                (import.alias, library_index.get(&import.key))
            else {
                continue;
            };
            add_dependency_bindings(&mut module_context, &dependency_alias, &modules[*index]);
        }

        for (name, export) in &module.exports {
            match export {
                ExportInfo::Const { value, .. } => {
                    if is_root_import {
                        plan.root_rewrites.constants.insert(
                            format!("{alias}.{name}"),
                            rewrite_expr(value, &module_context),
                        );
                    }
                }
                ExportInfo::Function { .. } => {
                    if is_root_import {
                        plan.root_rewrites.function_targets.insert(
                            format!("{alias}.{name}"),
                            module_function_key(&alias, module, name),
                        );
                    }
                }
                ExportInfo::UserType {
                    identity,
                    fields,
                    span,
                } => {
                    let Some(user_type) = module.user_types.get(name) else {
                        diagnostics.push(Diagnostic::error(
                            "E_IMPORT_UNKNOWN_EXPORT",
                            format!("unknown imported user type `{name}`"),
                            *span,
                        ));
                        continue;
                    };
                    debug_assert!(module_user_type_fields_match(fields, &user_type.fields));
                    debug_assert!(module.user_types.get(name).is_some_and(|user_type| {
                        module_user_type_fields_match(fields, &user_type.fields)
                    }));
                    debug_assert_eq!(identity, &user_type.identity);
                    insert_imported_user_type_metadata(
                        &mut plan.imported_user_types,
                        &alias,
                        module,
                        name,
                        user_type,
                        &mut HashSet::new(),
                    );
                }
            }
        }

        for (name, function) in &module.functions {
            let key = module_function_key(&alias, module, name);
            let body = rewrite_function_body(&function.body, &function.params, &module_context);
            if name_is_exported_function(module, name) || module.private_symbols.contains(name) {
                plan.imported_functions.insert(
                    key,
                    FunctionInfo {
                        overloads: imported_function_overloads(
                            function,
                            &alias,
                            name,
                            module,
                            source_context_id,
                            &module_context,
                        ),
                        display_name: if is_root_import {
                            if name_is_exported_function(module, name) {
                                format!("{alias}.{name}")
                            } else {
                                format!("__import_{alias}_{name}")
                            }
                        } else {
                            format!("{}::{name}", module.key.as_deref().unwrap_or("library"))
                        },
                        source_id: function.source_id,
                        source_context_id,
                        params: function.params.clone(),
                        default_values: function.default_values.clone(),
                        param_types: imported_function_param_types(
                            &alias,
                            module,
                            &function.param_types,
                        ),
                        body,
                        span: function.span,
                    },
                );
            } else {
                diagnostics.push(Diagnostic::error(
                    "E_IMPORT_UNKNOWN_EXPORT",
                    format!("unknown imported function `{name}`"),
                    function.span,
                ));
            }
        }

        for ((_, name), method) in &module.methods {
            let Some(method_info) =
                imported_method_info(&alias, module, method, source_context_id, &module_context)
            else {
                continue;
            };
            plan.imported_methods.insert(
                (method_info.receiver_type.clone(), name.clone()),
                method_info,
            );
        }
        module.attach_diagnostics(&mut diagnostics[diagnostic_start..]);
    }
    debug_assert!(plan.imported_functions.values().all(|function| {
        function.source_id != SourceId::root()
            && function.source_context_id != SourceContextId::root()
    }));
    debug_assert!(plan.imported_methods.values().all(|method| {
        method.source_id != SourceId::root() && method.source_context_id != SourceContextId::root()
    }));
    plan
}

fn imported_method_info(
    alias: &str,
    module: &ModuleInfo,
    method: &ModuleMethodInfo,
    source_context_id: SourceContextId,
    module_context: &RewriteContext,
) -> Option<MethodInfo> {
    let identity = method.receiver_identity.as_ref()?;
    if !exported_user_type(module, &identity.name) {
        return None;
    }

    let mut params = Vec::with_capacity(method.params.len());
    for param in &method.params {
        params.push(imported_method_param_info(alias, module, param)?);
    }

    Some(MethodInfo {
        source_id: identity.source_id,
        source_context_id,
        receiver_type: format!("{alias}.{}", identity.name),
        receiver_name: method.receiver_name.clone(),
        params,
        body: rewrite_function_body(&method.body, &method.param_names, module_context),
        span: method.span,
    })
}

fn imported_method_param_info(
    alias: &str,
    module: &ModuleInfo,
    param: &ModuleMethodParamInfo,
) -> Option<MethodParamInfo> {
    if param.type_name.starts_with("array<") && param.type_name.ends_with('>') {
        let element_type = &param.type_name["array<".len()..param.type_name.len() - 1];
        if let Some(kind) = array_kind_from_element_type_name(element_type) {
            return Some(MethodParamInfo {
                name: param.name.clone(),
                pine_type: PineType::new(Qualifier::Series, kind),
                user_type_name: None,
            });
        }
        if exported_scalar_tree_user_type(module, element_type) {
            return Some(MethodParamInfo {
                name: param.name.clone(),
                pine_type: PineType::new(Qualifier::Series, ValueKind::UserTypeArray),
                user_type_name: Some(format!("{alias}.{element_type}")),
            });
        }
        return None;
    }
    if let Some(pine_type) = imported_user_type_scalar_field_type(&param.type_name) {
        return Some(MethodParamInfo {
            name: param.name.clone(),
            pine_type,
            user_type_name: None,
        });
    }
    if !exported_user_type(module, &param.type_name) {
        return None;
    }
    Some(MethodParamInfo {
        name: param.name.clone(),
        pine_type: PineType::new(Qualifier::Series, ValueKind::UserType),
        user_type_name: Some(format!("{alias}.{}", param.type_name)),
    })
}

fn exported_scalar_tree_user_type(module: &ModuleInfo, type_name: &str) -> bool {
    let Some(ExportInfo::UserType { fields, .. }) = module.exports.get(type_name) else {
        return false;
    };
    debug_assert!(
        module
            .user_types
            .get(type_name)
            .is_some_and(|user_type| module_user_type_fields_match(fields, &user_type.fields))
    );
    module_user_type_fields_are_scalar_tree(module, fields, &mut HashSet::new())
}

fn exported_user_type(module: &ModuleInfo, type_name: &str) -> bool {
    let Some(ExportInfo::UserType { fields, .. }) = module.exports.get(type_name) else {
        return false;
    };
    debug_assert!(
        module
            .user_types
            .get(type_name)
            .is_some_and(|user_type| module_user_type_fields_match(fields, &user_type.fields))
    );
    true
}

fn insert_imported_user_type_metadata(
    imported_user_types: &mut HashMap<String, ImportedUserTypeInfo>,
    alias: &str,
    module: &ModuleInfo,
    name: &str,
    user_type: &ModuleUserTypeInfo,
    seen: &mut HashSet<String>,
) {
    let imported_name = format!("{alias}.{name}");
    if imported_user_types.contains_key(&imported_name) {
        return;
    }
    if !seen.insert(name.to_owned()) {
        return;
    }
    imported_user_types.insert(
        imported_name,
        ImportedUserTypeInfo {
            identity: ImportedUserTypeIdentity {
                source_id: user_type.identity.source_id,
                name: user_type.identity.name.clone(),
            },
            fields: user_type
                .fields
                .iter()
                .map(|field| ImportedUserTypeFieldInfo {
                    default_value: field.default_value.clone(),
                    varip: field.varip,
                    name: field.name.clone(),
                    type_name: field.type_name.clone(),
                    pine_type: field.pine_type,
                    span: field.span,
                })
                .collect(),
            span: user_type.span,
        },
    );
    for field in &user_type.fields {
        if field.pine_type.is_some() {
            continue;
        }
        if let Some(nested) = module.user_types.get(&field.type_name) {
            insert_imported_user_type_metadata(
                imported_user_types,
                alias,
                module,
                &field.type_name,
                nested,
                seen,
            );
        }
    }
    seen.remove(name);
}

fn module_user_type_fields_are_scalar_tree(
    module: &ModuleInfo,
    fields: &[ModuleUserTypeFieldInfo],
    seen: &mut HashSet<String>,
) -> bool {
    fields.iter().all(|field| {
        if let Some(pine_type) = field.pine_type {
            return is_scalar_user_type_field_kind(pine_type.kind);
        }
        if !seen.insert(field.type_name.clone()) {
            return false;
        }
        let supported = module
            .user_types
            .get(&field.type_name)
            .is_some_and(|user_type| {
                module_user_type_fields_are_scalar_tree(module, &user_type.fields, seen)
            });
        seen.remove(&field.type_name);
        supported
    })
}

fn is_scalar_user_type_field_kind(kind: ValueKind) -> bool {
    matches!(
        kind,
        ValueKind::Int | ValueKind::Float | ValueKind::Bool | ValueKind::String | ValueKind::Color
    )
}

fn rewrite_context_for_module(alias: &str, module: &ModuleInfo) -> RewriteContext {
    let mut context = RewriteContext::default();
    for (name, value) in &module.constants {
        context.constants.insert(name.clone(), value.clone());
    }
    for name in module.functions.keys() {
        context
            .function_targets
            .insert(name.clone(), module_function_key(alias, module, name));
    }
    for name in module
        .exports
        .iter()
        .filter_map(|(name, export)| matches!(export, ExportInfo::UserType { .. }).then_some(name))
    {
        context
            .type_targets
            .insert(name.clone(), format!("{alias}.{name}"));
    }
    context
}

fn module_function_key(alias: &str, _module: &ModuleInfo, name: &str) -> String {
    // Not spellable by Pine source, so public aliases cannot capture builtin
    // calls inside a library or collide with generated private function keys.
    format!("@import:{alias}.{name}")
}

fn dependency_namespace(module: &ModuleInfo) -> String {
    format!("@source{}", module.id.get())
}

fn add_dependency_bindings(context: &mut RewriteContext, alias: &str, module: &ModuleInfo) {
    let namespace = dependency_namespace(module);
    for (name, export) in &module.exports {
        let source_name = format!("{alias}.{name}");
        match export {
            ExportInfo::Function { .. } => {
                context
                    .function_targets
                    .insert(source_name, module_function_key(&namespace, module, name));
            }
            ExportInfo::Const { value, .. } => {
                context.constants.insert(
                    source_name,
                    rewrite_expr(value, &rewrite_context_for_module(&namespace, module)),
                );
            }
            ExportInfo::UserType { .. } => {
                context
                    .type_targets
                    .insert(source_name, format!("{namespace}.{name}"));
            }
        }
    }
}

fn name_is_exported_function(module: &ModuleInfo, name: &str) -> bool {
    matches!(module.exports.get(name), Some(ExportInfo::Function { .. }))
}

fn is_const_import_expr(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Member { .. } => false,
        ExprKind::Literal(_) => true,
        ExprKind::QualifiedName(parts) => const_qualified_type(&parts.join(".")).is_some(),
        ExprKind::Unary { op, expr } => {
            matches!(op, UnaryOp::Plus | UnaryOp::Minus | UnaryOp::Not)
                && is_const_import_expr(expr)
        }
        ExprKind::Binary { op, left, right } => {
            matches!(
                op,
                BinaryOp::Add
                    | BinaryOp::Sub
                    | BinaryOp::Mul
                    | BinaryOp::Div
                    | BinaryOp::Mod
                    | BinaryOp::Eq
                    | BinaryOp::NotEq
                    | BinaryOp::Gt
                    | BinaryOp::Gte
                    | BinaryOp::Lt
                    | BinaryOp::Lte
                    | BinaryOp::And
                    | BinaryOp::Or
            ) && is_const_import_expr(left)
                && is_const_import_expr(right)
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            is_const_import_expr(condition)
                && is_const_import_expr(then_expr)
                && is_const_import_expr(else_expr)
        }
        ExprKind::Call { .. }
        | ExprKind::If { .. }
        | ExprKind::For { .. }
        | ExprKind::ForIn { .. }
        | ExprKind::While { .. }
        | ExprKind::Switch { .. }
        | ExprKind::Tuple(_)
        | ExprKind::History { .. }
        | ExprKind::Group(_)
        | ExprKind::Identifier(_) => false,
    }
}

fn const_qualified_type(name: &str) -> Option<PineType> {
    if pine_builtins::named_color(name).is_some() {
        return Some(PineType::new(Qualifier::Const, ValueKind::Color));
    }
    if pine_builtins::named_float_constant(name).is_some() {
        return Some(PineType::new(Qualifier::Const, ValueKind::Float));
    }
    if pine_builtins::named_int_constant(name).is_some() {
        return Some(PineType::new(Qualifier::Const, ValueKind::Int));
    }
    if pine_builtins::named_string_constant(name).is_some() {
        return Some(PineType::new(Qualifier::Const, ValueKind::String));
    }
    None
}

#[cfg(test)]
#[path = "modules/tests.rs"]
mod tests;
