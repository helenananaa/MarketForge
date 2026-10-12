use super::*;
use pine_syntax::{MethodDecl, UserTypeDecl};

fn name(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        ExprKind::QualifiedName(parts) => Some(parts.join(".")),
        ExprKind::Group(inner) => name(inner),
        _ => None,
    }
}

fn fresh_array(expr: &Expr, element: &str) -> bool {
    matches!(&expr.kind, ExprKind::Call { callee, args }
        if name(callee).is_some_and(|n| n == format!("array.new<{element}>") || n == format!("array.new_{element}"))
        && args.is_empty())
}

fn fresh_object(expr: &Expr, ty: &UserTypeDecl) -> bool {
    let ExprKind::Call { callee, args } = &expr.kind else {
        return false;
    };
    if name(callee).as_deref() != Some(&format!("{}.new", ty.name)) {
        return false;
    }
    let mut bound = vec![None; ty.fields.len()];
    let mut named = false;
    for (position, arg) in args.iter().enumerate() {
        let index = if let Some(arg_name) = &arg.name {
            named = true;
            let Some(index) = ty.fields.iter().position(|field| &field.name == arg_name) else {
                return false;
            };
            index
        } else {
            if named {
                return false;
            }
            position
        };
        let Some(slot) = bound.get_mut(index) else {
            return false;
        };
        if slot.is_some() {
            return false;
        }
        *slot = Some(&arg.value);
    }
    ty.fields.iter().zip(bound).all(|(field, value)| {
        let Some(element) = field
            .type_name
            .strip_prefix("array<")
            .and_then(|s| s.strip_suffix('>'))
        else {
            return matches!(
                field.type_name.as_str(),
                "int" | "float" | "bool" | "string" | "color"
            ) && value.is_none_or(scalar_constructor_argument);
        };
        value.is_some_and(|value| fresh_array(value, element))
    })
}

// Unknown helpers could retain a reference or mutate previously stored objects.
// Only scalar syntax and these read-only builtin families can seed scalar fields.
fn scalar_constructor_argument(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Identifier(_) | ExprKind::QualifiedName(_) => true,
        ExprKind::Group(inner) | ExprKind::Unary { expr: inner, .. } => {
            scalar_constructor_argument(inner)
        }
        ExprKind::Binary { left, right, .. } => {
            scalar_constructor_argument(left) && scalar_constructor_argument(right)
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            scalar_constructor_argument(condition)
                && scalar_constructor_argument(then_expr)
                && scalar_constructor_argument(else_expr)
        }
        ExprKind::Call { callee, args } => {
            name(callee).is_some_and(|n| {
                (matches!(n.as_str(), "time" | "time_close")
                    || n.starts_with("math.")
                    || n.starts_with("str."))
                    && pine_builtins::get_phase_1_builtin(&n).is_some()
            }) && args
                .iter()
                .all(|arg| scalar_constructor_argument(&arg.value))
        }
        _ => false,
    }
}

// This proof is admission-only: it does not implement collection-bearing UDTs.
// All receiver initializations and replacements must allocate fresh fields.
pub(super) fn local_field_mutation_spans(body: &FunctionBody, declarations: &[Stmt]) -> Vec<Span> {
    let FunctionBody::Block(statements) = body else {
        return Vec::new();
    };
    let types: Vec<_> = declarations
        .iter()
        .filter_map(|stmt| match &stmt.kind {
            StmtKind::UserType(ty) => Some(ty),
            StmtKind::Export(export) => match &export.item {
                ExportItem::UserType { decl, .. } => Some(decl),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let methods: Vec<_> = declarations
        .iter()
        .filter_map(|stmt| match &stmt.kind {
            StmtKind::Method(method) => Some(method),
            _ => None,
        })
        .collect();
    let mut allowed = Vec::new();
    if declarations.iter().any(|stmt| match &stmt.kind {
        StmtKind::Import(import) => import.alias.as_ref().is_some_and(|alias| matches!(alias.name.as_str(), "math" | "str" | "array")),
        StmtKind::Function { name, .. } => matches!(name.as_str(), "time" | "time_close"),
        StmtKind::Export(export) => matches!(&export.item, ExportItem::Function { name, .. } if matches!(name.as_str(), "time" | "time_close")),
        _ => false,
    }) { return allowed; }
    // Method names can override array syntax; syntactic push/clear alone is
    // not evidence of a builtin operation in that case.
    if methods
        .iter()
        .any(|method| matches!(method.name.as_str(), "push" | "clear"))
    {
        return allowed;
    }
    for statement in statements {
        let StmtKind::Decl {
            name: binding,
            value,
            ..
        } = &statement.kind
        else {
            continue;
        };
        let Some(ty) = types.iter().find(|ty| fresh_object(value, ty)) else {
            continue;
        };
        let containers: HashSet<_> = statements
            .iter()
            .filter_map(|stmt| {
                if let StmtKind::Decl { name, value, .. } = &stmt.kind
                    && fresh_array(value, &ty.name)
                {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();
        if !statements
            .iter()
            .all(|stmt| stable_statement(stmt, binding, ty, &containers, &methods))
        {
            continue;
        }
        for stmt in statements {
            if let StmtKind::Expr(Expr {
                kind: ExprKind::Call { callee, .. },
                ..
            }) = &stmt.kind
                && let Some(path) = name(callee)
                && field_mutation(&path, binding, ty)
            {
                allowed.push(callee.span);
            }
        }
    }
    allowed
}

fn field_mutation(path: &str, binding: &str, ty: &UserTypeDecl) -> bool {
    ty.fields.iter().any(|field| {
        field.type_name.starts_with("array<")
            && (path == format!("{binding}.{}.push", field.name)
                || path == format!("{binding}.{}.clear", field.name))
    })
}

fn reference_value(
    expr: &Expr,
    binding: &str,
    ty: &UserTypeDecl,
    containers: &HashSet<String>,
) -> bool {
    if name(expr).is_some_and(|n| {
        n == binding
            || containers.contains(&n)
            || ty
                .fields
                .iter()
                .any(|f| f.type_name.starts_with("array<") && n == format!("{binding}.{}", f.name))
    }) {
        return true;
    }
    match &expr.kind {
        ExprKind::Group(inner) => reference_value(inner, binding, ty, containers),
        ExprKind::Ternary {
            then_expr,
            else_expr,
            ..
        } => {
            reference_value(then_expr, binding, ty, containers)
                || reference_value(else_expr, binding, ty, containers)
        }
        ExprKind::History { expr, .. } => reference_value(expr, binding, ty, containers),
        ExprKind::Call { callee, .. } => name(callee).is_some_and(|n| {
            containers.iter().any(|container| {
                n == format!("{container}.get")
                    || n == format!("{container}.last")
                    || n == format!("{container}.shift")
            })
        }),
        _ => false,
    }
}

fn stable_statement(
    stmt: &Stmt,
    binding: &str,
    ty: &UserTypeDecl,
    containers: &HashSet<String>,
    methods: &[&MethodDecl],
) -> bool {
    if let StmtKind::Reassign { name, .. } = &stmt.kind
        && name != binding
        && !containers.contains(name)
    {
        // A free reassignment may target a captured/global reference. This
        // admission proof only models fresh replacement of its owned roots.
        return false;
    }
    if let StmtKind::Decl {
        name: target,
        declared_type,
        value,
        ..
    } = &stmt.kind
        && target != binding
        && !containers.contains(target)
    {
        let mut borrows = false;
        super::visit_expr(value, &mut |expr| {
            borrows |= reference_value(expr, binding, ty, containers);
        });
        let scalar = matches!(declared_type, Some(pine_syntax::DeclaredType::Named(n)) if matches!(n.as_str(), "int"|"float"|"bool"|"string"|"color"));
        if borrows && !scalar {
            return false;
        }
    }
    match &stmt.kind {
        StmtKind::Decl {
            name: target,
            value,
            ..
        }
        | StmtKind::Reassign {
            name: target,
            value,
        } => {
            if target == binding {
                return fresh_object(value, ty);
            }
            if containers.contains(target) {
                return fresh_array(value, &ty.name);
            }
            if reference_value(value, binding, ty, containers) {
                return false;
            }
            safe_expression(value, binding, ty, containers, methods)
        }
        StmtKind::Expr(expr) => safe_expression(expr, binding, ty, containers, methods),
        StmtKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            safe_expression(condition, binding, ty, containers, methods)
                && then_branch
                    .iter()
                    .chain(else_branch)
                    .all(|stmt| stable_statement(stmt, binding, ty, containers, methods))
        }
        _ => false,
    }
}

fn safe_expression(
    expr: &Expr,
    binding: &str,
    ty: &UserTypeDecl,
    containers: &HashSet<String>,
    methods: &[&MethodDecl],
) -> bool {
    let mut safe = true;
    super::visit_expr(expr, &mut |node| {
        if matches!(
            node.kind,
            ExprKind::If { .. }
                | ExprKind::Switch { .. }
                | ExprKind::For { .. }
                | ExprKind::ForIn { .. }
                | ExprKind::While { .. }
        ) {
            safe = false;
        }
        if let ExprKind::Call { callee, args } = &node.kind {
            let Some(path) = name(callee) else {
                safe = false;
                return;
            };
            let borrows = args
                .iter()
                .any(|arg| reference_value(&arg.value, binding, ty, containers));
            let on_object = path.starts_with(&format!("{binding}."));
            if on_object && !field_mutation(&path, binding, ty) {
                safe = false;
            }
            let on_container = containers
                .iter()
                .any(|container| path.starts_with(&format!("{container}.")));
            if borrows || on_container {
                let (receiver, method_name) = path
                    .split_once('.')
                    .map_or((None, path.as_str()), |(r, m)| (Some(r), m));
                if receiver.is_some_and(|r| !containers.contains(r)) {
                    safe = false;
                    return;
                }
                let candidates: Vec<_> = methods
                    .iter()
                    .filter(|m| {
                        m.name == method_name
                            && m.params
                                .first()
                                .is_some_and(|p| p.type_name == format!("array<{}>", ty.name))
                    })
                    .collect();
                if candidates.len() != 1 || !preserves_fields(candidates[0], ty, methods) {
                    safe = false;
                }
            }
        }
    });
    safe
}

fn preserves_fields(method: &MethodDecl, ty: &UserTypeDecl, methods: &[&MethodDecl]) -> bool {
    let FunctionBody::Block(body) = &method.body else {
        return false;
    };
    let receiver = &method.params[0].name;
    let mut read_receivers = HashSet::from([receiver.clone()]);
    for statement in body {
        if let StmtKind::ForIn {
            value: binding,
            iterable,
            ..
        } = &statement.kind
            && name(iterable).as_deref() == Some(receiver)
        {
            for field in &ty.fields {
                if field.type_name.starts_with("array<") {
                    read_receivers.insert(format!("{binding}.{}", field.name));
                }
            }
        }
    }
    let scalar_locals: HashSet<_> = body
        .iter()
        .filter_map(|stmt| match &stmt.kind {
            StmtKind::Decl {
                name,
                declared_type: Some(pine_syntax::DeclaredType::Named(ty)),
                ..
            } if matches!(ty.as_str(), "int" | "float" | "bool" | "string" | "color") => {
                Some(name.clone())
            }
            _ => None,
        })
        .collect();
    fn statement_ok(stmt: &Stmt, method: &MethodDecl, scalar_locals: &HashSet<String>) -> bool {
        match &stmt.kind {
            StmtKind::Decl {
                name,
                declared_type,
                ..
            } => {
                !method.params.iter().any(|param| &param.name == name)
                    && matches!(declared_type, Some(pine_syntax::DeclaredType::Named(ty)) if matches!(ty.as_str(), "int"|"float"|"bool"|"string"|"color"))
            }
            StmtKind::Reassign { name, .. } => scalar_locals.contains(name),
            StmtKind::Expr(_) => true,
            StmtKind::If {
                then_branch,
                else_branch,
                ..
            } => then_branch
                .iter()
                .chain(else_branch)
                .all(|stmt| statement_ok(stmt, method, scalar_locals)),
            StmtKind::ForIn { body, .. } => body
                .iter()
                .all(|stmt| statement_ok(stmt, method, scalar_locals)),
            _ => false,
        }
    }
    if !body
        .iter()
        .all(|stmt| statement_ok(stmt, method, &scalar_locals))
    {
        return false;
    }
    let mut safe = true;
    for stmt in body {
        super::visit_statement_exprs(stmt, &mut |expr| {
            if matches!(
                expr.kind,
                ExprKind::If { .. }
                    | ExprKind::Switch { .. }
                    | ExprKind::For { .. }
                    | ExprKind::ForIn { .. }
                    | ExprKind::While { .. }
            ) {
                safe = false;
            }
            if let ExprKind::Call { callee, args } = &expr.kind {
                let Some(path) = name(callee) else {
                    safe = false;
                    return;
                };
                let Some((base, operation)) = path.rsplit_once('.') else {
                    safe = false;
                    return;
                };
                if methods.iter().any(|m| m.name == operation) {
                    safe = false;
                }
                match operation {
                    "push" | "shift" | "clear" => {
                        if base != receiver {
                            safe = false;
                        }
                    }
                    "size" | "get" | "last" | "binary_search_leftmost" => {
                        if !read_receivers.contains(base)
                            || args.iter().any(|arg| {
                                name(&arg.value).is_some_and(|n| {
                                    method.params.iter().any(|p| {
                                        p.name == n
                                            && (p.type_name.contains(&ty.name)
                                                || p.type_name.starts_with("array<"))
                                    })
                                })
                            })
                        {
                            safe = false;
                        }
                    }
                    _ => safe = false,
                }
            }
        });
    }
    safe
}
