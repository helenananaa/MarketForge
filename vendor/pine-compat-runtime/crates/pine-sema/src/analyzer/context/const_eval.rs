use super::*;
use crate::analyzer::functions::resolve_udf_arg_indices;
use crate::constant_values::{ConstValue, eval_pure_const_call, exact_i64_from_numeric};
use crate::legacy::PineDialect;
mod tuple_projection;

const MAX_STRING_VALUE_DOMAIN_DEPTH: u32 = 64;
const MAX_STRING_VALUE_DOMAIN_VALUES: usize = 64;

#[derive(Default)]
struct StringValueDomainEnv {
    symbol_visiting: Vec<SymbolId>,
    function_visiting: Vec<String>,
}

impl Analyzer {
    pub(crate) fn record_symbol_const_switch_key(
        &mut self,
        symbol: SymbolInfo,
        key: &ConstSwitchKey,
    ) {
        match key {
            ConstSwitchKey::Bool(value) => {
                self.const_bool_symbols.insert(symbol.id, *value);
            }
            ConstSwitchKey::Numeric(value) => {
                self.const_numeric_symbols.insert(symbol.id, *value);
            }
            ConstSwitchKey::String(value) => {
                self.const_string_symbols.insert(symbol.id, value.clone());
            }
            ConstSwitchKey::Color(value) => {
                self.const_color_symbols.insert(symbol.id, *value);
            }
        }
    }

    fn function_param_const_switch_key(&self, name: &str) -> Option<&ConstSwitchKey> {
        self.function_param_const_switch_keys
            .iter()
            .rev()
            .find_map(|keys| keys.get(name))
    }

    pub(super) fn const_lookup_symbol(&self, name: &str, span: Span) -> Option<SymbolInfo> {
        self.bound_symbol(name, span)
            .or_else(|| self.scope.resolve(name))
    }

    fn known_const_call_value(
        &self,
        callee: &pine_syntax::Expr,
        args: &[pine_syntax::CallArg],
    ) -> Option<ConstValue> {
        let callee = const_call_name(callee)?;
        let args = args
            .iter()
            .map(|arg| self.known_const_value(&arg.value))
            .collect::<Option<Vec<_>>>()?;
        eval_pure_const_call(&callee, &args)
    }

    fn known_const_value(&self, expr: &pine_syntax::Expr) -> Option<ConstValue> {
        self.known_const_int_value(expr)
            .map(ConstValue::Int)
            .or_else(|| {
                self.known_const_numeric_value(expr)
                    .filter(|value| value.is_finite())
                    .map(ConstValue::Float)
            })
            .or_else(|| self.known_const_bool_value(expr).map(ConstValue::Bool))
    }

    pub(crate) fn known_const_int_value(&self, expr: &pine_syntax::Expr) -> Option<i64> {
        let expr = expr.without_groups();
        self.legacy
            .canonical_value_name(self.current_source_context_id(), expr.span)
            .and_then(pine_builtins::named_int_constant)
            .or_else(|| const_int_value(expr))
            .or_else(|| self.known_const_int_value_from_symbols(expr))
    }

    /// Returns a constant integer for range validation, while preserving the
    /// distinction between an unknown expression and a statically-int numeric
    /// result that cannot be represented by `i64` (for example
    /// `math.abs(i64::MIN)`, whose runtime fallback is a float).
    pub(crate) fn known_const_int_for_validation(
        &self,
        expr: &pine_syntax::Expr,
    ) -> Option<Result<i64, ()>> {
        if let Some(value) = self.known_const_int_value(expr) {
            return Some(Ok(value));
        }

        let pine_type = self.type_of_expr_with_params(expr, &HashMap::new())?;
        if pine_type.kind != pine_ir::ValueKind::Int {
            return None;
        }
        let value = self.known_const_numeric_value(expr)?;
        Some(exact_i64_from_numeric(value).ok_or(()))
    }

    /// Validates arguments that the runtime consumes as an actual
    /// `PineValue::Int`. A statically-int expression whose known runtime value
    /// has promoted to float (notably wrappers around `math.abs(i64::MIN)`) is
    /// invalid even when that float is mathematically integral.
    pub(crate) fn known_strict_const_int_for_validation(
        &self,
        expr: &pine_syntax::Expr,
    ) -> Option<Result<i64, ()>> {
        if let Some(value) = self.known_const_int_value(expr) {
            return Some(Ok(value));
        }

        let pine_type = self.type_of_expr_with_params(expr, &HashMap::new())?;
        (pine_type.kind == pine_ir::ValueKind::Int
            && self.known_const_numeric_value(expr).is_some())
        .then_some(Err(()))
    }

    pub(crate) fn known_history_offset_int_value(&self, expr: &pine_syntax::Expr) -> Option<i64> {
        let mut env = HistoryOffsetIntEnv::default();
        self.known_history_offset_int_value_inner(expr, &mut env)
            .or_else(|| {
                (self.type_of_expr_with_params(expr, &HashMap::new())?.kind
                    == pine_ir::ValueKind::Int)
                    .then(|| self.known_history_offset_numeric_value_inner(expr, &mut env))
                    .flatten()
                    .and_then(exact_i64_from_numeric)
            })
    }

    pub(super) fn known_history_offset_call_value(
        &self,
        callee: &pine_syntax::Expr,
        args: &[pine_syntax::CallArg],
        env: &mut HistoryOffsetIntEnv,
    ) -> Option<ConstValue> {
        let callee = const_call_name(callee)?;
        let args = args
            .iter()
            .map(|arg| self.known_history_offset_value_inner(&arg.value, env))
            .collect::<Option<Vec<_>>>()?;
        eval_pure_const_call(&callee, &args)
    }

    fn known_history_offset_value_inner(
        &self,
        expr: &pine_syntax::Expr,
        env: &mut HistoryOffsetIntEnv,
    ) -> Option<ConstValue> {
        self.known_history_offset_int_value_inner(expr, env)
            .map(ConstValue::Int)
            .or_else(|| {
                self.known_history_offset_numeric_value_inner(expr, env)
                    .filter(|value| value.is_finite())
                    .map(ConstValue::Float)
            })
            .or_else(|| {
                self.known_history_offset_bool_value_inner(expr, env)
                    .map(ConstValue::Bool)
            })
    }

    pub(super) fn known_history_offset_int_value_inner(
        &self,
        expr: &pine_syntax::Expr,
        env: &mut HistoryOffsetIntEnv,
    ) -> Option<i64> {
        const_int_value(expr)
            .or_else(|| self.known_history_offset_int_value_from_symbols(expr, env))
    }

    fn known_history_offset_int_value_from_symbols(
        &self,
        expr: &pine_syntax::Expr,
        env: &mut HistoryOffsetIntEnv,
    ) -> Option<i64> {
        let expr = expr.without_groups();
        match &expr.kind {
            pine_syntax::ExprKind::Identifier(name) => {
                if let Some(local) = env.locals.get(name).cloned() {
                    if env.local_visiting.contains(name) {
                        return None;
                    }
                    env.local_visiting.push(name.clone());
                    let result = self.known_history_offset_int_value_inner(&local, env);
                    env.local_visiting.pop();
                    return result;
                }
                if env.shadowed_locals.contains(name) {
                    return None;
                }

                let symbol = self.const_lookup_symbol(name, expr.span)?;
                if let Some(value) = self.const_int_symbols.get(&symbol.id) {
                    return Some(*value);
                }
                if env.symbol_visiting.contains(&symbol.id) {
                    return None;
                }
                env.symbol_visiting.push(symbol.id);
                let result = self.with_symbol_initializer(symbol.id, |analyzer, init_expr| {
                    analyzer.known_history_offset_int_value_inner(init_expr, env)
                });
                env.symbol_visiting.pop();
                result
            }
            pine_syntax::ExprKind::Call { callee, args } => self
                .known_history_offset_call_value(callee, args, env)
                .and_then(ConstValue::as_int),
            pine_syntax::ExprKind::Unary {
                op: pine_syntax::UnaryOp::Plus,
                expr,
            } => self.known_history_offset_int_value_inner(expr, env),
            pine_syntax::ExprKind::Unary {
                op: pine_syntax::UnaryOp::Minus,
                expr,
            } => self
                .known_history_offset_int_value_inner(expr, env)
                .and_then(i64::checked_neg),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Add,
                left,
                right,
            } => self
                .known_history_offset_int_value_inner(left, env)?
                .checked_add(self.known_history_offset_int_value_inner(right, env)?),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Sub,
                left,
                right,
            } => self
                .known_history_offset_int_value_inner(left, env)?
                .checked_sub(self.known_history_offset_int_value_inner(right, env)?),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Mul,
                left,
                right,
            } => self
                .known_history_offset_int_value_inner(left, env)?
                .checked_mul(self.known_history_offset_int_value_inner(right, env)?),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Div,
                left,
                right,
            } if self.legacy.dialect() <= PineDialect::V5 => self
                .known_history_offset_int_value_inner(left, env)?
                .checked_div(self.known_history_offset_int_value_inner(right, env)?),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Mod,
                left,
                right,
            } => self
                .known_history_offset_int_value_inner(left, env)?
                .checked_rem(self.known_history_offset_int_value_inner(right, env)?),
            pine_syntax::ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => match self.known_history_offset_bool_value_inner(condition, env) {
                Some(true) => self.known_history_offset_int_value_inner(then_expr, env),
                Some(false) => self.known_history_offset_int_value_inner(else_expr, env),
                None => {
                    let then_value = self.known_history_offset_int_value_inner(then_expr, env)?;
                    let else_value = self.known_history_offset_int_value_inner(else_expr, env)?;
                    (then_value == else_value).then_some(then_value)
                }
            },
            pine_syntax::ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => match self.known_history_offset_bool_value_inner(condition, env) {
                Some(true) => self.known_history_offset_int_branch_result(then_branch, env),
                Some(false) => self.known_history_offset_int_branch_result(else_branch, env),
                None => {
                    let then_value =
                        self.known_history_offset_int_branch_result(then_branch, env)?;
                    let else_value =
                        self.known_history_offset_int_branch_result(else_branch, env)?;
                    (then_value == else_value).then_some(then_value)
                }
            },
            pine_syntax::ExprKind::Switch { selector, arms } => {
                self.known_history_offset_int_switch_result(selector.as_deref(), arms, env)
            }
            pine_syntax::ExprKind::For {
                from,
                to,
                step,
                body,
                ..
            } => {
                self.known_history_offset_int_value_inner(from, env)?;
                self.known_history_offset_int_value_inner(to, env)?;
                if let Some(step) = step
                    && self.known_history_offset_int_value_inner(step, env)? == 0
                {
                    return None;
                }
                self.known_history_offset_int_branch_result(body, env)
            }
            pine_syntax::ExprKind::ForIn {
                index,
                value,
                iterable,
                body,
            } => self.known_history_offset_for_in_branch_result(
                index,
                value,
                iterable,
                body,
                env,
                Self::known_history_offset_int_branch_result,
            ),
            _ => None,
        }
    }
}

impl Analyzer {
    fn known_history_offset_int_branch_result(
        &self,
        statements: &[pine_syntax::Stmt],
        env: &mut HistoryOffsetIntEnv,
    ) -> Option<i64> {
        let saved_locals = env.locals.clone();
        let result = self.known_history_offset_int_branch_result_inner(statements, env);
        env.locals = saved_locals;
        result
    }

    fn known_history_offset_int_branch_result_inner(
        &self,
        statements: &[pine_syntax::Stmt],
        env: &mut HistoryOffsetIntEnv,
    ) -> Option<i64> {
        let (last, prefix) = statements.split_last()?;
        for statement in prefix {
            match &statement.kind {
                pine_syntax::StmtKind::Expr(_) => {}
                pine_syntax::StmtKind::Decl {
                    mode: pine_syntax::DeclMode::Normal,
                    name,
                    value,
                    ..
                } => {
                    env.locals.insert(name.clone(), value.clone());
                }
                pine_syntax::StmtKind::TupleDecl { names, value } => {
                    let pine_syntax::ExprKind::Tuple(values) = &value.kind else {
                        return None;
                    };
                    if names.len() != values.len() {
                        return None;
                    }
                    for (name, value) in names.iter().zip(values) {
                        env.locals.insert(name.clone(), value.clone());
                    }
                }
                pine_syntax::StmtKind::Reassign { name, .. } => {
                    env.locals.remove(name);
                }
                _ => return None,
            }
        }

        match &last.kind {
            pine_syntax::StmtKind::Expr(expr) => {
                self.known_history_offset_int_value_inner(expr, env)
            }
            _ => None,
        }
    }

    fn known_history_offset_int_switch_result(
        &self,
        selector: Option<&pine_syntax::Expr>,
        arms: &[pine_syntax::SwitchArm],
        env: &mut HistoryOffsetIntEnv,
    ) -> Option<i64> {
        if let Some(selector) = selector {
            let Some(selector_key) = self.known_history_offset_switch_key(selector, env) else {
                return self.known_history_offset_int_all_switch_results_with_default(arms, env);
            };
            for (index, arm) in arms.iter().enumerate() {
                match &arm.condition {
                    Some(condition) => match self.known_history_offset_switch_key(condition, env) {
                        Some(condition_key) => {
                            if condition_key == selector_key {
                                return self
                                    .known_history_offset_int_switch_arm_result(&arm.result, env);
                            }
                        }
                        None => {
                            return self.known_history_offset_int_all_switch_results_with_default(
                                &arms[index..],
                                env,
                            );
                        }
                    },
                    None => {
                        return self.known_history_offset_int_switch_arm_result(&arm.result, env);
                    }
                }
            }
            return None;
        }

        for (index, arm) in arms.iter().enumerate() {
            match &arm.condition {
                Some(condition) => match self.known_history_offset_bool_value_inner(condition, env)
                {
                    Some(true) => {
                        return self.known_history_offset_int_switch_arm_result(&arm.result, env);
                    }
                    Some(false) => {}
                    None => {
                        return self.known_history_offset_int_all_switch_results_with_default(
                            &arms[index..],
                            env,
                        );
                    }
                },
                None => {
                    return self.known_history_offset_int_switch_arm_result(&arm.result, env);
                }
            }
        }
        None
    }

    fn known_history_offset_int_all_switch_results_with_default(
        &self,
        arms: &[pine_syntax::SwitchArm],
        env: &mut HistoryOffsetIntEnv,
    ) -> Option<i64> {
        if !arms.iter().any(|arm| arm.condition.is_none()) {
            return None;
        }

        let mut expected = None;
        for arm in arms {
            let value = self.known_history_offset_int_switch_arm_result(&arm.result, env)?;
            match expected {
                Some(expected) if expected != value => return None,
                Some(_) => {}
                None => expected = Some(value),
            }
        }
        expected
    }

    fn known_history_offset_int_switch_arm_result(
        &self,
        result: &pine_syntax::SwitchArmResult,
        env: &mut HistoryOffsetIntEnv,
    ) -> Option<i64> {
        match result {
            pine_syntax::SwitchArmResult::Expr(expr) => {
                self.known_history_offset_int_value_inner(expr, env)
            }
            pine_syntax::SwitchArmResult::Block(statements) => {
                self.known_history_offset_int_branch_result(statements, env)
            }
        }
    }

    fn known_const_int_value_from_symbols(&self, expr: &pine_syntax::Expr) -> Option<i64> {
        match &expr.kind {
            pine_syntax::ExprKind::Identifier(name) => {
                let symbol = self.const_lookup_symbol(name, expr.span)?;
                self.const_int_symbols.get(&symbol.id).copied()
            }
            pine_syntax::ExprKind::Call { callee, args } => self
                .known_const_call_value(callee, args)
                .and_then(ConstValue::as_int),
            pine_syntax::ExprKind::Unary {
                op: pine_syntax::UnaryOp::Plus,
                expr,
            } => self.known_const_int_value(expr),
            pine_syntax::ExprKind::Unary {
                op: pine_syntax::UnaryOp::Minus,
                expr,
            } => self.known_const_int_value(expr).and_then(i64::checked_neg),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Add,
                left,
                right,
            } => self
                .known_const_int_value(left)?
                .checked_add(self.known_const_int_value(right)?),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Sub,
                left,
                right,
            } => self
                .known_const_int_value(left)?
                .checked_sub(self.known_const_int_value(right)?),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Mul,
                left,
                right,
            } => self
                .known_const_int_value(left)?
                .checked_mul(self.known_const_int_value(right)?),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Div,
                left,
                right,
            } if self.legacy.dialect() <= PineDialect::V5 => self
                .known_const_int_value(left)?
                .checked_div(self.known_const_int_value(right)?),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Mod,
                left,
                right,
            } => self
                .known_const_int_value(left)?
                .checked_rem(self.known_const_int_value(right)?),
            pine_syntax::ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => match self.known_const_bool_value(condition) {
                Some(true) => self.known_const_int_value(then_expr),
                Some(false) => self.known_const_int_value(else_expr),
                None => {
                    let then_value = self.known_const_int_value(then_expr)?;
                    let else_value = self.known_const_int_value(else_expr)?;
                    (then_value == else_value).then_some(then_value)
                }
            },
            pine_syntax::ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => match self.known_const_bool_value(condition) {
                Some(true) => self.known_const_int_branch_result(then_branch),
                Some(false) => self.known_const_int_branch_result(else_branch),
                None => {
                    let then_value = self.known_const_int_branch_result(then_branch)?;
                    let else_value = self.known_const_int_branch_result(else_branch)?;
                    (then_value == else_value).then_some(then_value)
                }
            },
            pine_syntax::ExprKind::Switch { selector, arms } => {
                self.known_const_int_switch_result(selector.as_deref(), arms)
            }
            _ => None,
        }
    }

    fn known_const_int_branch_result(&self, statements: &[pine_syntax::Stmt]) -> Option<i64> {
        match &statements.last()?.kind {
            pine_syntax::StmtKind::Expr(expr) => self.known_const_int_value(expr),
            _ => None,
        }
    }

    fn known_const_int_switch_result(
        &self,
        selector: Option<&pine_syntax::Expr>,
        arms: &[pine_syntax::SwitchArm],
    ) -> Option<i64> {
        if let Some(selector) = selector {
            let Some(selector_key) = self.known_const_switch_key(selector) else {
                return self.known_const_int_all_switch_results_with_default(arms);
            };
            for (index, arm) in arms.iter().enumerate() {
                match &arm.condition {
                    Some(condition) => match self.known_const_switch_key(condition) {
                        Some(condition_key) => {
                            if condition_key == selector_key {
                                return self.known_const_int_switch_arm_result(&arm.result);
                            }
                        }
                        None => {
                            return self
                                .known_const_int_all_switch_results_with_default(&arms[index..]);
                        }
                    },
                    None => return self.known_const_int_switch_arm_result(&arm.result),
                }
            }
            return None;
        }

        for (index, arm) in arms.iter().enumerate() {
            match &arm.condition {
                Some(condition) => match self.known_const_bool_value(condition) {
                    Some(true) => return self.known_const_int_switch_arm_result(&arm.result),
                    Some(false) => {}
                    None => {
                        return self
                            .known_const_int_all_switch_results_with_default(&arms[index..]);
                    }
                },
                None => return self.known_const_int_switch_arm_result(&arm.result),
            }
        }
        None
    }

    fn known_const_int_all_switch_results_with_default(
        &self,
        arms: &[pine_syntax::SwitchArm],
    ) -> Option<i64> {
        if !arms.iter().any(|arm| arm.condition.is_none()) {
            return None;
        }

        let mut expected = None;
        for arm in arms {
            let value = self.known_const_int_switch_arm_result(&arm.result)?;
            match expected {
                Some(expected) if expected != value => return None,
                Some(_) => {}
                None => expected = Some(value),
            }
        }
        expected
    }

    fn known_const_int_switch_arm_result(
        &self,
        result: &pine_syntax::SwitchArmResult,
    ) -> Option<i64> {
        match result {
            pine_syntax::SwitchArmResult::Expr(expr) => self.known_const_int_value(expr),
            pine_syntax::SwitchArmResult::Block(statements) => {
                self.known_const_int_branch_result(statements)
            }
        }
    }

    pub(crate) fn known_const_string_value(&self, expr: &pine_syntax::Expr) -> Option<String> {
        let expr = expr.without_groups();
        if let pine_syntax::ExprKind::Binary { op, left, right } = &expr.kind
            && matches!(op, pine_syntax::BinaryOp::Add | pine_syntax::BinaryOp::Sub)
            && self
                .type_of_expr_with_params(expr, &HashMap::new())
                .is_some_and(|ty| ty.kind == pine_ir::ValueKind::PlotDisplay)
        {
            return pine_builtins::combine_display_values(
                &self.known_const_string_value(left)?,
                &self.known_const_string_value(right)?,
                *op == pine_syntax::BinaryOp::Sub,
            );
        }

        self.legacy
            .canonical_string_value(self.current_source_context_id(), expr.span)
            .map(str::to_owned)
            .or_else(|| {
                self.legacy
                    .canonical_value_name(self.current_source_context_id(), expr.span)
                    .and_then(pine_builtins::named_string_constant)
                    .map(str::to_owned)
            })
            .or_else(|| const_string_value(expr))
            .or_else(|| self.known_const_string_value_from_symbols(expr))
    }

    /// Returns every string that a drawing-enum expression can produce when
    /// its domain is statically provable and bounded. Unlike constant folding,
    /// this deliberately follows immutable initializers and joins dynamic
    /// branches. String inputs need explicit options; user functions must have
    /// bounded return branches independent of their parameters and locals.
    pub(crate) fn known_string_value_domain(
        &self,
        expr: &pine_syntax::Expr,
    ) -> Option<Vec<String>> {
        self.known_string_value_domain_inner(expr, &mut StringValueDomainEnv::default(), 0)
    }

    fn known_string_value_domain_inner(
        &self,
        expr: &pine_syntax::Expr,
        env: &mut StringValueDomainEnv,
        depth: u32,
    ) -> Option<Vec<String>> {
        if depth > MAX_STRING_VALUE_DOMAIN_DEPTH {
            return None;
        }
        let expr = expr.without_groups();
        // A function body is inspected outside its invocation's lexical scope.
        // Do not reuse caller constants or bindings from another invocation to
        // narrow its result. Join every return branch instead.
        let constant = if env.function_visiting.is_empty() {
            self.known_const_string_value(expr)
        } else {
            const_string_value(expr)
        };
        if let Some(value) = constant {
            return Some(vec![value]);
        }

        match &expr.kind {
            pine_syntax::ExprKind::Identifier(name) => {
                if !env.function_visiting.is_empty() {
                    return None;
                }
                let symbol = self.const_lookup_symbol(name, expr.span)?;
                if env.symbol_visiting.contains(&symbol.id)
                    || self.request_reassigned_names.contains(name)
                {
                    return None;
                }
                env.symbol_visiting.push(symbol.id);
                let domain = self
                    .with_symbol_initializer(symbol.id, |analyzer, initializer| {
                        analyzer.known_string_value_domain_inner(initializer, env, depth + 1)
                    })
                    .or_else(|| {
                        let (source, index) = self.symbol_tuple_value_sources.get(&symbol.id)?;
                        let projected = tuple_projection::project(&source.expr, *index, 0)?;
                        self.with_source_context_ref(source.source_context_id, |analyzer| {
                            analyzer.known_string_value_domain_inner(&projected, env, depth + 1)
                        })
                    });
                env.symbol_visiting.pop();
                domain
            }
            pine_syntax::ExprKind::Ternary {
                then_expr,
                else_expr,
                ..
            } => merge_string_value_domains(
                self.known_string_value_domain_inner(then_expr, env, depth + 1)?,
                self.known_string_value_domain_inner(else_expr, env, depth + 1)?,
            ),
            pine_syntax::ExprKind::If {
                then_branch,
                else_branch,
                ..
            } => merge_string_value_domains(
                self.known_string_value_domain_branch(then_branch, env, depth + 1)?,
                self.known_string_value_domain_branch(else_branch, env, depth + 1)?,
            ),
            pine_syntax::ExprKind::Switch { selector, arms } => {
                self.known_string_value_domain_switch(selector.as_deref(), arms, env, depth + 1)
            }
            pine_syntax::ExprKind::Call { callee, args } => self
                .known_string_input_value_domain(callee, args, env, depth + 1)
                .or_else(|| self.known_function_string_value_domain(callee, args, env, depth + 1)),
            _ => None,
        }
    }

    fn known_function_string_value_domain(
        &self,
        callee: &pine_syntax::Expr,
        args: &[pine_syntax::CallArg],
        env: &mut StringValueDomainEnv,
        depth: u32,
    ) -> Option<Vec<String>> {
        let name = const_call_name(callee)?;
        if env.function_visiting.contains(&name) {
            return None;
        }
        let function = self.functions.get(&name)?;
        // Without invocation-specific type information, an overload cannot be
        // selected safely. Leave it unsupported rather than prove the wrong body.
        if !function.overloads.is_empty() {
            return None;
        }
        let completed_args = function.complete_args(args, callee.span).ok()?;
        resolve_udf_arg_indices(&function.params, &completed_args).ok()?;
        env.function_visiting.push(name);
        let domain = self.with_source_context_ref(function.source_context_id, |analyzer| {
            match &function.body {
                pine_syntax::FunctionBody::Expr(expr) => {
                    analyzer.known_string_value_domain_inner(expr, env, depth)
                }
                pine_syntax::FunctionBody::Block(statements) => {
                    analyzer.known_string_value_domain_branch(statements, env, depth)
                }
            }
        });
        env.function_visiting.pop();
        domain
    }

    fn known_string_value_domain_branch(
        &self,
        statements: &[pine_syntax::Stmt],
        env: &mut StringValueDomainEnv,
        depth: u32,
    ) -> Option<Vec<String>> {
        if depth > MAX_STRING_VALUE_DOMAIN_DEPTH {
            return None;
        }
        match &statements.last()?.kind {
            pine_syntax::StmtKind::Expr(expr) => {
                self.known_string_value_domain_inner(expr, env, depth)
            }
            pine_syntax::StmtKind::If {
                then_branch,
                else_branch,
                ..
            } => merge_string_value_domains(
                self.known_string_value_domain_branch(then_branch, env, depth + 1)?,
                self.known_string_value_domain_branch(else_branch, env, depth + 1)?,
            ),
            _ => None,
        }
    }

    fn known_string_value_domain_switch(
        &self,
        selector: Option<&pine_syntax::Expr>,
        arms: &[pine_syntax::SwitchArm],
        env: &mut StringValueDomainEnv,
        depth: u32,
    ) -> Option<Vec<String>> {
        let mut domain = Vec::new();
        let mut has_default = false;
        for arm in arms {
            has_default |= arm.condition.is_none();
            let arm_domain = match &arm.result {
                pine_syntax::SwitchArmResult::Expr(expr) => {
                    self.known_string_value_domain_inner(expr, env, depth)?
                }
                pine_syntax::SwitchArmResult::Block(statements) => {
                    self.known_string_value_domain_branch(statements, env, depth)?
                }
            };
            domain = merge_string_value_domains(domain, arm_domain)?;
        }
        let exhaustive = has_default
            || selector
                .and_then(|selector| {
                    let values = self.known_string_value_domain_inner(selector, env, depth)?;
                    let keys = arms
                        .iter()
                        .map(|arm| self.known_const_string_value(arm.condition.as_ref()?))
                        .collect::<Option<Vec<_>>>()?;
                    Some(values.iter().all(|value| keys.contains(value)))
                })
                .unwrap_or(false);
        (exhaustive && !domain.is_empty()).then_some(domain)
    }

    fn known_string_input_value_domain(
        &self,
        callee: &pine_syntax::Expr,
        args: &[pine_syntax::CallArg],
        env: &mut StringValueDomainEnv,
        depth: u32,
    ) -> Option<Vec<String>> {
        let callee_name = const_call_name(callee)?;
        if callee_name != "input" && callee_name != "input.string" {
            return None;
        }
        let positional_options_index =
            if callee_name == "input" && self.legacy.dialect().version() <= 4 {
                4
            } else {
                2
            };
        let options = args.iter().enumerate().find_map(|(index, arg)| {
            (arg.name.as_deref() == Some("options")
                || (arg.name.is_none() && index == positional_options_index))
                .then_some(&arg.value)
        })?;
        let pine_syntax::ExprKind::Tuple(values) = &options.kind else {
            return None;
        };
        let mut domain = Vec::new();
        for value in values {
            domain = merge_string_value_domains(
                domain,
                self.known_string_value_domain_inner(value, env, depth)?,
            )?;
        }
        (!domain.is_empty()).then_some(domain)
    }

    fn known_const_string_value_from_symbols(&self, expr: &pine_syntax::Expr) -> Option<String> {
        match &expr.kind {
            pine_syntax::ExprKind::Identifier(name) => {
                if let Some(ConstSwitchKey::String(value)) =
                    self.function_param_const_switch_key(name)
                {
                    return Some(value.clone());
                }
                let symbol = self.const_lookup_symbol(name, expr.span)?;
                self.const_string_symbols.get(&symbol.id).cloned()
            }
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Add,
                left,
                right,
            } => {
                let mut value = self.known_const_string_value(left)?;
                value.push_str(&self.known_const_string_value(right)?);
                Some(value)
            }
            pine_syntax::ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                if self.known_const_bool_value(condition)? {
                    self.known_const_string_value(then_expr)
                } else {
                    self.known_const_string_value(else_expr)
                }
            }
            pine_syntax::ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                if self.known_const_bool_value(condition)? {
                    self.known_const_string_branch_result(then_branch)
                } else {
                    self.known_const_string_branch_result(else_branch)
                }
            }
            pine_syntax::ExprKind::Switch { selector, arms } => {
                self.known_const_string_switch_result(selector.as_deref(), arms)
            }
            _ => None,
        }
    }

    fn known_const_string_branch_result(&self, statements: &[pine_syntax::Stmt]) -> Option<String> {
        match &statements.last()?.kind {
            pine_syntax::StmtKind::Expr(expr) => self.known_const_string_value(expr),
            _ => None,
        }
    }

    fn known_const_string_switch_result(
        &self,
        selector: Option<&pine_syntax::Expr>,
        arms: &[pine_syntax::SwitchArm],
    ) -> Option<String> {
        if let Some(selector) = selector {
            let selector_key = self.known_const_switch_key(selector)?;
            for arm in arms {
                match &arm.condition {
                    Some(condition) => {
                        if self.known_const_switch_key(condition)? == selector_key {
                            return self.known_const_string_switch_arm_result(&arm.result);
                        }
                    }
                    None => return self.known_const_string_switch_arm_result(&arm.result),
                }
            }
            return None;
        }

        for arm in arms {
            match &arm.condition {
                Some(condition) => {
                    if self.known_const_bool_value(condition)? {
                        return self.known_const_string_switch_arm_result(&arm.result);
                    }
                }
                None => return self.known_const_string_switch_arm_result(&arm.result),
            }
        }
        None
    }

    fn known_const_string_switch_arm_result(
        &self,
        result: &pine_syntax::SwitchArmResult,
    ) -> Option<String> {
        match result {
            pine_syntax::SwitchArmResult::Expr(expr) => self.known_const_string_value(expr),
            pine_syntax::SwitchArmResult::Block(statements) => {
                self.known_const_string_branch_result(statements)
            }
        }
    }

    pub(crate) fn known_const_color_value(&self, expr: &pine_syntax::Expr) -> Option<u32> {
        let expr = expr.without_groups();
        self.legacy
            .canonical_value_name(self.current_source_context_id(), expr.span)
            .and_then(pine_builtins::named_color)
            .or_else(|| const_color_value(expr))
            .or_else(|| self.known_const_color_value_from_symbols(expr))
    }

    fn known_const_color_value_from_symbols(&self, expr: &pine_syntax::Expr) -> Option<u32> {
        match &expr.kind {
            pine_syntax::ExprKind::Identifier(name) => {
                if let Some(ConstSwitchKey::Color(value)) =
                    self.function_param_const_switch_key(name)
                {
                    return Some(*value);
                }
                let symbol = self.const_lookup_symbol(name, expr.span)?;
                self.const_color_symbols.get(&symbol.id).copied()
            }
            pine_syntax::ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                if self.known_const_bool_value(condition)? {
                    self.known_const_color_value(then_expr)
                } else {
                    self.known_const_color_value(else_expr)
                }
            }
            pine_syntax::ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                if self.known_const_bool_value(condition)? {
                    self.known_const_color_branch_result(then_branch)
                } else {
                    self.known_const_color_branch_result(else_branch)
                }
            }
            pine_syntax::ExprKind::Switch { selector, arms } => {
                self.known_const_color_switch_result(selector.as_deref(), arms)
            }
            _ => None,
        }
    }

    fn known_const_color_branch_result(&self, statements: &[pine_syntax::Stmt]) -> Option<u32> {
        match &statements.last()?.kind {
            pine_syntax::StmtKind::Expr(expr) => self.known_const_color_value(expr),
            _ => None,
        }
    }

    fn known_const_color_switch_result(
        &self,
        selector: Option<&pine_syntax::Expr>,
        arms: &[pine_syntax::SwitchArm],
    ) -> Option<u32> {
        if let Some(selector) = selector {
            let selector_key = self.known_const_switch_key(selector)?;
            for arm in arms {
                match &arm.condition {
                    Some(condition) => {
                        if self.known_const_switch_key(condition)? == selector_key {
                            return self.known_const_color_switch_arm_result(&arm.result);
                        }
                    }
                    None => return self.known_const_color_switch_arm_result(&arm.result),
                }
            }
            return None;
        }

        for arm in arms {
            match &arm.condition {
                Some(condition) => {
                    if self.known_const_bool_value(condition)? {
                        return self.known_const_color_switch_arm_result(&arm.result);
                    }
                }
                None => return self.known_const_color_switch_arm_result(&arm.result),
            }
        }
        None
    }

    fn known_const_color_switch_arm_result(
        &self,
        result: &pine_syntax::SwitchArmResult,
    ) -> Option<u32> {
        match result {
            pine_syntax::SwitchArmResult::Expr(expr) => self.known_const_color_value(expr),
            pine_syntax::SwitchArmResult::Block(statements) => {
                self.known_const_color_branch_result(statements)
            }
        }
    }

    pub(crate) fn known_const_numeric_value(&self, expr: &pine_syntax::Expr) -> Option<f64> {
        let expr = expr.without_groups();
        self.legacy
            .canonical_value_name(self.current_source_context_id(), expr.span)
            .and_then(|name| {
                if pine_builtins::builtin_series_value_type(name)
                    .is_some_and(|ty| ty.qualifier != Qualifier::Const)
                {
                    return None;
                }
                pine_builtins::named_float_constant(name)
                    .or_else(|| pine_builtins::named_int_constant(name).map(|value| value as f64))
            })
            .or_else(|| const_numeric_value(expr))
            .or_else(|| self.known_const_numeric_value_from_symbols(expr))
    }

    fn known_const_numeric_value_from_symbols(&self, expr: &pine_syntax::Expr) -> Option<f64> {
        match &expr.kind {
            pine_syntax::ExprKind::Identifier(name) => {
                if let Some(ConstSwitchKey::Numeric(value)) =
                    self.function_param_const_switch_key(name)
                {
                    return Some(*value);
                }
                let symbol = self.const_lookup_symbol(name, expr.span)?;
                self.const_numeric_symbols.get(&symbol.id).copied()
            }
            pine_syntax::ExprKind::Call { callee, args } => self
                .known_const_call_value(callee, args)
                .and_then(ConstValue::as_numeric),
            pine_syntax::ExprKind::Unary {
                op: pine_syntax::UnaryOp::Plus,
                expr,
            } => self.known_const_numeric_value(expr),
            pine_syntax::ExprKind::Unary {
                op: pine_syntax::UnaryOp::Minus,
                expr,
            } => self.known_const_numeric_value(expr).map(|value| -value),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Add,
                left,
                right,
            } => Some(
                self.known_const_numeric_value(left)? + self.known_const_numeric_value(right)?,
            ),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Sub,
                left,
                right,
            } => Some(
                self.known_const_numeric_value(left)? - self.known_const_numeric_value(right)?,
            ),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Mul,
                left,
                right,
            } => Some(
                self.known_const_numeric_value(left)? * self.known_const_numeric_value(right)?,
            ),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Div,
                left,
                right,
            } => {
                let value = self.known_const_numeric_value(left)?
                    / self.known_const_numeric_value(right)?;
                value.is_finite().then_some(value)
            }
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Mod,
                left,
                right,
            } => {
                let value = self.known_const_numeric_value(left)?
                    % self.known_const_numeric_value(right)?;
                value.is_finite().then_some(value)
            }
            pine_syntax::ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                if self.known_const_bool_value(condition)? {
                    self.known_const_numeric_value(then_expr)
                } else {
                    self.known_const_numeric_value(else_expr)
                }
            }
            pine_syntax::ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                if self.known_const_bool_value(condition)? {
                    self.known_const_numeric_branch_result(then_branch)
                } else {
                    self.known_const_numeric_branch_result(else_branch)
                }
            }
            pine_syntax::ExprKind::Switch { selector, arms } => {
                self.known_const_numeric_switch_result(selector.as_deref(), arms)
            }
            _ => None,
        }
    }

    fn known_const_numeric_branch_result(&self, statements: &[pine_syntax::Stmt]) -> Option<f64> {
        match &statements.last()?.kind {
            pine_syntax::StmtKind::Expr(expr) => self.known_const_numeric_value(expr),
            _ => None,
        }
    }

    fn known_const_numeric_switch_result(
        &self,
        selector: Option<&pine_syntax::Expr>,
        arms: &[pine_syntax::SwitchArm],
    ) -> Option<f64> {
        if let Some(selector) = selector {
            let selector_key = self.known_const_switch_key(selector)?;
            for arm in arms {
                match &arm.condition {
                    Some(condition) => {
                        if self.known_const_switch_key(condition)? == selector_key {
                            return self.known_const_numeric_switch_arm_result(&arm.result);
                        }
                    }
                    None => return self.known_const_numeric_switch_arm_result(&arm.result),
                }
            }
            return None;
        }

        for arm in arms {
            match &arm.condition {
                Some(condition) => {
                    if self.known_const_bool_value(condition)? {
                        return self.known_const_numeric_switch_arm_result(&arm.result);
                    }
                }
                None => return self.known_const_numeric_switch_arm_result(&arm.result),
            }
        }
        None
    }

    fn known_const_numeric_switch_arm_result(
        &self,
        result: &pine_syntax::SwitchArmResult,
    ) -> Option<f64> {
        match result {
            pine_syntax::SwitchArmResult::Expr(expr) => self.known_const_numeric_value(expr),
            pine_syntax::SwitchArmResult::Block(statements) => {
                self.known_const_numeric_branch_result(statements)
            }
        }
    }

    pub(crate) fn known_const_bool_value(&self, expr: &pine_syntax::Expr) -> Option<bool> {
        let expr = expr.without_groups();
        match &expr.kind {
            pine_syntax::ExprKind::Literal(pine_syntax::Literal::Bool(value)) => Some(*value),
            pine_syntax::ExprKind::Identifier(name) => {
                if let Some(ConstSwitchKey::Bool(value)) =
                    self.function_param_const_switch_key(name)
                {
                    return Some(*value);
                }
                let symbol = self.const_lookup_symbol(name, expr.span)?;
                self.const_bool_symbols.get(&symbol.id).copied()
            }
            pine_syntax::ExprKind::Unary {
                op: pine_syntax::UnaryOp::Not,
                expr,
            } => self.known_const_bool_value(expr).map(|value| !value),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::And,
                left,
                right,
            } => Some(self.known_const_bool_value(left)? && self.known_const_bool_value(right)?),
            pine_syntax::ExprKind::Binary {
                op: pine_syntax::BinaryOp::Or,
                left,
                right,
            } => Some(self.known_const_bool_value(left)? || self.known_const_bool_value(right)?),
            pine_syntax::ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                if self.known_const_bool_value(condition)? {
                    self.known_const_bool_value(then_expr)
                } else {
                    self.known_const_bool_value(else_expr)
                }
            }
            pine_syntax::ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                if self.known_const_bool_value(condition)? {
                    self.known_const_bool_branch_result(then_branch)
                } else {
                    self.known_const_bool_branch_result(else_branch)
                }
            }
            pine_syntax::ExprKind::Switch { selector, arms } => {
                self.known_const_bool_switch_result(selector.as_deref(), arms)
            }
            pine_syntax::ExprKind::Binary {
                op:
                    op @ (pine_syntax::BinaryOp::Eq
                    | pine_syntax::BinaryOp::NotEq
                    | pine_syntax::BinaryOp::Gt
                    | pine_syntax::BinaryOp::Gte
                    | pine_syntax::BinaryOp::Lt
                    | pine_syntax::BinaryOp::Lte),
                left,
                right,
            } => self
                .known_const_numeric_comparison(*op, left, right)
                .or_else(|| self.known_const_bool_comparison(*op, left, right))
                .or_else(|| self.known_const_string_comparison(*op, left, right))
                .or_else(|| self.known_const_color_comparison(*op, left, right)),
            _ => None,
        }
    }

    fn known_const_bool_branch_result(&self, statements: &[pine_syntax::Stmt]) -> Option<bool> {
        match &statements.last()?.kind {
            pine_syntax::StmtKind::Expr(expr) => self.known_const_bool_value(expr),
            _ => None,
        }
    }

    fn known_const_bool_switch_result(
        &self,
        selector: Option<&pine_syntax::Expr>,
        arms: &[pine_syntax::SwitchArm],
    ) -> Option<bool> {
        if let Some(selector) = selector {
            let selector_key = self.known_const_switch_key(selector)?;
            for arm in arms {
                match &arm.condition {
                    Some(condition) => {
                        if self.known_const_switch_key(condition)? == selector_key {
                            return self.known_const_bool_switch_arm_result(&arm.result);
                        }
                    }
                    None => return self.known_const_bool_switch_arm_result(&arm.result),
                }
            }
            return None;
        }

        for arm in arms {
            match &arm.condition {
                Some(condition) => {
                    if self.known_const_bool_value(condition)? {
                        return self.known_const_bool_switch_arm_result(&arm.result);
                    }
                }
                None => return self.known_const_bool_switch_arm_result(&arm.result),
            }
        }
        None
    }

    fn known_const_bool_switch_arm_result(
        &self,
        result: &pine_syntax::SwitchArmResult,
    ) -> Option<bool> {
        match result {
            pine_syntax::SwitchArmResult::Expr(expr) => self.known_const_bool_value(expr),
            pine_syntax::SwitchArmResult::Block(statements) => {
                self.known_const_bool_branch_result(statements)
            }
        }
    }

    pub(crate) fn known_const_switch_key(
        &self,
        expr: &pine_syntax::Expr,
    ) -> Option<ConstSwitchKey> {
        let expr = expr.without_groups();
        if let pine_syntax::ExprKind::Identifier(name) = &expr.kind
            && let Some(key) = self.function_param_const_switch_key(name).cloned()
        {
            return Some(key);
        }
        self.known_const_bool_value(expr)
            .map(ConstSwitchKey::Bool)
            .or_else(|| {
                self.known_const_string_value(expr)
                    .map(ConstSwitchKey::String)
            })
            .or_else(|| {
                self.known_const_color_value(expr)
                    .map(ConstSwitchKey::Color)
            })
            .or_else(|| {
                self.known_const_numeric_value(expr)
                    .map(ConstSwitchKey::Numeric)
            })
    }

    fn known_const_numeric_comparison(
        &self,
        op: pine_syntax::BinaryOp,
        left: &pine_syntax::Expr,
        right: &pine_syntax::Expr,
    ) -> Option<bool> {
        let left = self.known_const_numeric_value(left)?;
        let right = self.known_const_numeric_value(right)?;
        if self.legacy.dialect().version() < 5 {
            pine_ir::exact_numeric_comparison(crate::lowering::lower_binary_op(op), left, right)
        } else {
            pine_ir::pine_numeric_comparison(crate::lowering::lower_binary_op(op), left, right)
        }
    }

    fn known_const_bool_comparison(
        &self,
        op: pine_syntax::BinaryOp,
        left: &pine_syntax::Expr,
        right: &pine_syntax::Expr,
    ) -> Option<bool> {
        let left = self.known_const_bool_value(left)?;
        let right = self.known_const_bool_value(right)?;
        match op {
            pine_syntax::BinaryOp::Eq => Some(left == right),
            pine_syntax::BinaryOp::NotEq => Some(left != right),
            _ => None,
        }
    }

    fn known_const_string_comparison(
        &self,
        op: pine_syntax::BinaryOp,
        left: &pine_syntax::Expr,
        right: &pine_syntax::Expr,
    ) -> Option<bool> {
        let left = self.known_const_string_value(left)?;
        let right = self.known_const_string_value(right)?;
        match op {
            pine_syntax::BinaryOp::Eq => Some(left == right),
            pine_syntax::BinaryOp::NotEq => Some(left != right),
            _ => None,
        }
    }

    fn known_const_color_comparison(
        &self,
        op: pine_syntax::BinaryOp,
        left: &pine_syntax::Expr,
        right: &pine_syntax::Expr,
    ) -> Option<bool> {
        let left = self.known_const_color_value(left)?;
        let right = self.known_const_color_value(right)?;
        match op {
            pine_syntax::BinaryOp::Eq => Some(left == right),
            pine_syntax::BinaryOp::NotEq => Some(left != right),
            _ => None,
        }
    }
}

fn merge_string_value_domains(mut left: Vec<String>, right: Vec<String>) -> Option<Vec<String>> {
    for value in right {
        if left.contains(&value) {
            continue;
        }
        if left.len() == MAX_STRING_VALUE_DOMAIN_VALUES {
            return None;
        }
        left.push(value);
    }
    Some(left)
}

fn const_call_name(callee: &pine_syntax::Expr) -> Option<String> {
    match &callee.kind {
        pine_syntax::ExprKind::Identifier(name) => Some(name.clone()),
        pine_syntax::ExprKind::QualifiedName(parts) => Some(parts.join(".")),
        _ => None,
    }
}
