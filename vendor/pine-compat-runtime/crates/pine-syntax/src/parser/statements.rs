use crate::{
    BinaryOp, DeclMode, Expr, ExprKind, FunctionBody, FunctionParam, Span, Stmt, StmtKind,
    TokenKind,
};

use super::{ForInParts, ForParts, Parser};

impl Parser {
    pub(super) fn parse_stmt(&mut self) -> Option<Stmt> {
        self.with_stmt_depth(Self::parse_stmt_inner)
    }

    pub(super) fn with_stmt_depth<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Option<T>,
    ) -> Option<T> {
        if !self.within_depth_budget(1, 0) {
            self.error_here("E_PARSE_STMT_DEPTH", "statement nesting is too deep");
            // Recovery must not recurse through the remaining adversarial
            // blocks, or construct an AST too deep for subsequent passes.
            self.pos = self.tokens.len().saturating_sub(1);
            return None;
        }
        self.stmt_depth += 1;
        let result = parse(self);
        self.stmt_depth -= 1;
        result
    }

    fn parse_stmt_inner(&mut self) -> Option<Stmt> {
        if self.at(TokenKind::Import) {
            return self.parse_import_decl();
        }

        if let Some(statement) = self.parse_phase_j_decl()? {
            return Some(statement);
        }

        if self.at(TokenKind::If) {
            return self.parse_if_stmt();
        }

        if self.at(TokenKind::For) {
            return self.parse_for_stmt();
        }

        if self.at(TokenKind::While) {
            return self.parse_while_stmt();
        }

        if self.at(TokenKind::Break) {
            let span = self.expect(TokenKind::Break, "expected `break`")?;
            return Some(Stmt {
                span,
                kind: StmtKind::Break,
            });
        }

        if self.at(TokenKind::Continue) {
            let span = self.expect(TokenKind::Continue, "expected `continue`")?;
            return Some(Stmt {
                span,
                kind: StmtKind::Continue,
            });
        }

        if self.at(TokenKind::LBracket) && self.looks_like_tuple_decl() {
            return self.parse_tuple_decl();
        }

        let mode = if self.source_version >= 5
            && matches!(&self.current().kind, TokenKind::Identifier(name) if name == "const")
        {
            self.bump();
            Some(DeclMode::Const)
        } else if self.at(TokenKind::Var) {
            self.bump();
            Some(DeclMode::Var)
        } else if self.at(TokenKind::Varip) {
            self.bump();
            Some(DeclMode::Varip)
        } else {
            None
        };

        if let Some((declared_type, name, start)) = self.try_parse_typed_decl_name() {
            self.expect(TokenKind::Eq, "expected `=` in variable declaration")?;
            let value = self.parse_expr(0)?;
            return Some(Stmt {
                span: start.merge(value.span),
                kind: StmtKind::Decl {
                    mode: mode.unwrap_or(DeclMode::Normal),
                    declared_type: Some(declared_type),
                    name,
                    value,
                },
            });
        }

        if mode == Some(DeclMode::Const) {
            self.error_here("E_PARSE_DECL", "`const` requires a typed declaration");
            return None;
        }

        if let TokenKind::Identifier(name) = self.current().kind.clone() {
            let start = self.current().span;
            if self.looks_like_function_decl() {
                return self.parse_function_decl();
            }

            if self.nth_at(1, TokenKind::Eq) || mode.is_some() {
                self.bump();
                self.expect(TokenKind::Eq, "expected `=` in variable declaration")?;
                let value = self.parse_expr(0)?;
                return Some(Stmt {
                    span: start.merge(value.span),
                    kind: StmtKind::Decl {
                        mode: mode.unwrap_or(DeclMode::Normal),
                        declared_type: None,
                        name,
                        value,
                    },
                });
            }

            if self.nth_at(1, TokenKind::ColonEq) {
                self.bump();
                self.bump();
                let value = self.parse_expr(0)?;
                return Some(Stmt {
                    span: start.merge(value.span),
                    kind: StmtKind::Reassign { name, value },
                });
            }

            if let Some(op) = self.compound_assignment_op(1) {
                self.bump();
                self.bump();
                let right = self.parse_expr(0)?;
                let value = Expr {
                    span: start.merge(right.span),
                    kind: ExprKind::Binary {
                        op,
                        left: Box::new(Expr {
                            span: start,
                            kind: ExprKind::Identifier(name.clone()),
                        }),
                        right: Box::new(right),
                    },
                };
                return Some(Stmt {
                    span: value.span,
                    kind: StmtKind::Reassign { name, value },
                });
            }

            if self.get_call_field_reassign_colon_eq_offset().is_some() {
                return self.parse_array_field_reassign(name, start);
            }

            if let Some(colon_eq_offset) = self.nested_field_reassign_colon_eq_offset() {
                let mut path = (2..colon_eq_offset)
                    .step_by(2)
                    .filter_map(|offset| {
                        if let TokenKind::Identifier(name) = &self.tokens[self.pos + offset].kind {
                            Some(name.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                let field = path.pop()?;
                for _ in 0..=colon_eq_offset {
                    self.bump();
                }
                let value = self.parse_expr(0)?;
                return Some(Stmt {
                    span: start.merge(value.span),
                    kind: StmtKind::FieldReassign {
                        receiver: name,
                        path,
                        field,
                        value,
                    },
                });
            }

            if self.nth_at(1, TokenKind::Dot)
                && self
                    .tokens
                    .get(self.pos + 2)
                    .is_some_and(|token| matches!(token.kind, TokenKind::Identifier(_)))
                && (self.nth_at(3, TokenKind::ColonEq) || self.compound_assignment_op(3).is_some())
            {
                let compound_op = self.compound_assignment_op(3);
                self.bump();
                self.bump();
                let TokenKind::Identifier(field) = self.current().kind.clone() else {
                    self.error_here("E_PARSE_ASSIGN", "expected field name after `.`");
                    return None;
                };
                let field_span = start.merge(self.current().span);
                self.bump();
                self.bump(); // The reassignment operator was checked above.
                let mut value = self.parse_expr(0)?;
                if let Some(op) = compound_op {
                    value = Expr {
                        span: start.merge(value.span),
                        kind: ExprKind::Binary {
                            op,
                            left: Box::new(Expr {
                                span: field_span,
                                kind: ExprKind::QualifiedName(vec![name.clone(), field.clone()]),
                            }),
                            right: Box::new(value),
                        },
                    };
                }
                if name == "strategy" {
                    return Some(Stmt {
                        span: start.merge(value.span),
                        kind: StmtKind::Unsupported {
                            feature: "strategy state variable mutation".to_owned(),
                        },
                    });
                }
                return Some(Stmt {
                    span: start.merge(value.span),
                    kind: StmtKind::FieldReassign {
                        path: Vec::new(),
                        receiver: name,
                        field,
                        value,
                    },
                });
            }
        } else if mode.is_some() {
            self.error_here("E_PARSE_DECL", "expected identifier after declaration mode");
            return None;
        }

        let expr = self.parse_expr(0)?;
        Some(Stmt {
            span: expr.span,
            kind: StmtKind::Expr(expr),
        })
    }

    fn parse_array_field_reassign(&mut self, first: String, start: Span) -> Option<Stmt> {
        self.bump();
        self.expect(TokenKind::Dot, "expected `.` before `get`")?;
        let TokenKind::Identifier(method) = self.current().kind.clone() else {
            self.error_here("E_PARSE_ASSIGN", "expected `get` after `.`");
            return None;
        };
        if method != "get" {
            self.error_here(
                "E_PARSE_ASSIGN",
                "expected `get` before chained field mutation",
            );
            return None;
        }
        self.bump();
        self.expect(TokenKind::LParen, "expected `(` after `get`")?;

        let (array, index) = if first == "array" {
            let array = self.parse_expr(0)?;
            self.expect(TokenKind::Comma, "expected `,` after array receiver")?;
            let index = self.parse_expr(0)?;
            (array, index)
        } else {
            let array = Expr {
                span: start,
                kind: ExprKind::Identifier(first),
            };
            let index = self.parse_expr(0)?;
            (array, index)
        };

        self.expect(TokenKind::RParen, "expected `)` after array get index")?;
        self.expect(TokenKind::Dot, "expected `.` before field name")?;
        let TokenKind::Identifier(field) = self.current().kind.clone() else {
            self.error_here("E_PARSE_ASSIGN", "expected field name after `.`");
            return None;
        };
        self.bump();
        self.expect(
            TokenKind::ColonEq,
            "expected `:=` in chained field mutation",
        )?;
        let value = self.parse_expr(0)?;
        Some(Stmt {
            span: start.merge(value.span),
            kind: StmtKind::ArrayFieldReassign {
                array,
                index,
                field,
                value,
            },
        })
    }

    fn parse_tuple_decl(&mut self) -> Option<Stmt> {
        let start = self.expect(TokenKind::LBracket, "expected `[`")?;
        let mut names = Vec::new();

        loop {
            match self.current().kind.clone() {
                TokenKind::Identifier(name) => {
                    names.push(name);
                    self.bump();
                }
                _ => {
                    self.error_here("E_PARSE_DECL", "expected identifier in tuple declaration");
                    return None;
                }
            }

            if self.at(TokenKind::Comma) {
                self.bump();
                continue;
            }
            break;
        }

        self.expect(TokenKind::RBracket, "expected `]` after tuple declaration")?;
        self.expect(TokenKind::Eq, "expected `=` in tuple declaration")?;
        let value = self.parse_expr(0)?;

        Some(Stmt {
            span: start.merge(value.span),
            kind: StmtKind::TupleDecl { names, value },
        })
    }

    fn parse_if_stmt(&mut self) -> Option<Stmt> {
        let start = self.expect(TokenKind::If, "expected `if`")?;
        let condition = self.parse_expr(0)?;
        self.expect(TokenKind::Newline, "expected newline after `if` condition")?;
        let then_branch = self.parse_indented_block()?;
        let mut span = then_branch
            .last()
            .map_or(condition.span, |statement| statement.span);

        let else_branch = if self.at(TokenKind::Else) {
            self.bump();
            if self.at(TokenKind::If) {
                let nested_if = self.parse_stmt()?;
                span = nested_if.span;
                vec![nested_if]
            } else {
                self.expect(TokenKind::Newline, "expected newline after `else`")?;
                let branch = self.parse_indented_block()?;
                span = branch.last().map_or(span, |statement| statement.span);
                branch
            }
        } else {
            Vec::new()
        };

        Some(Stmt {
            span: start.merge(span),
            kind: StmtKind::If {
                condition,
                then_branch,
                else_branch,
            },
        })
    }

    fn parse_for_stmt(&mut self) -> Option<Stmt> {
        let start = self.expect(TokenKind::For, "expected `for`")?;
        if self.at(TokenKind::LBracket) {
            let (key, value) = self.parse_for_in_pair()?;
            let parts = self.parse_for_in_tail(start, Some(key), value)?;
            return Some(Stmt {
                span: parts.start.merge(parts.span),
                kind: StmtKind::ForIn {
                    index: parts.index,
                    value: parts.value,
                    iterable: parts.iterable,
                    body: parts.body,
                },
            });
        }
        let counter = self.parse_for_counter()?;
        if self.at(TokenKind::Comma) {
            self.bump();
            let value = self.parse_for_counter()?;
            let parts = self.parse_for_in_tail(start, Some(counter), value)?;
            return Some(Stmt {
                span: parts.start.merge(parts.span),
                kind: StmtKind::ForIn {
                    index: parts.index,
                    value: parts.value,
                    iterable: parts.iterable,
                    body: parts.body,
                },
            });
        }
        if self.nth_identifier_is(0, "in") {
            let parts = self.parse_for_in_tail(start, None, counter)?;
            return Some(Stmt {
                span: parts.start.merge(parts.span),
                kind: StmtKind::ForIn {
                    index: parts.index,
                    value: parts.value,
                    iterable: parts.iterable,
                    body: parts.body,
                },
            });
        }
        let parts = self.parse_for_range_tail(start, counter)?;

        Some(Stmt {
            span: parts.start.merge(parts.span),
            kind: StmtKind::For {
                counter: parts.counter,
                from: parts.from,
                to: parts.to,
                step: parts.step,
                body: parts.body,
            },
        })
    }

    fn parse_while_stmt(&mut self) -> Option<Stmt> {
        let start = self.expect(TokenKind::While, "expected `while`")?;
        let condition = self.parse_expr(0)?;
        self.expect(
            TokenKind::Newline,
            "expected newline after `while` condition",
        )?;
        let body = self.parse_indented_block()?;
        let span = body
            .last()
            .map_or(condition.span, |statement| statement.span);

        Some(Stmt {
            span: start.merge(span),
            kind: StmtKind::While { condition, body },
        })
    }

    pub(super) fn parse_for_counter(&mut self) -> Option<String> {
        match self.current().kind.clone() {
            TokenKind::Identifier(name) => {
                self.bump();
                Some(name)
            }
            _ => {
                self.error_here("E_PARSE_FOR", "expected loop counter name");
                None
            }
        }
    }

    pub(super) fn parse_for_in_pair(&mut self) -> Option<(String, String)> {
        self.expect(TokenKind::LBracket, "expected `[` before for...in pair")?;
        let key = self.parse_for_counter()?;
        self.expect(TokenKind::Comma, "expected `,` in for...in pair")?;
        let value = self.parse_for_counter()?;
        self.expect(TokenKind::RBracket, "expected `]` after for...in pair")?;
        Some((key, value))
    }

    pub(super) fn parse_for_range_tail(
        &mut self,
        start: Span,
        counter: String,
    ) -> Option<ForParts> {
        self.expect(TokenKind::Eq, "expected `=` after loop counter")?;
        let from = self.parse_expr(0)?;
        self.expect(TokenKind::To, "expected `to` in for loop")?;
        let to = self.parse_expr(0)?;
        let step = if self.at(TokenKind::By) {
            self.bump();
            Some(self.parse_expr(0)?)
        } else {
            None
        };
        self.expect(TokenKind::Newline, "expected newline after `for` range")?;
        let body = self.parse_indented_block()?;
        let span = body.last().map_or(
            step.as_ref().map_or(to.span, |step| step.span),
            |statement| statement.span,
        );

        Some(ForParts {
            start,
            counter,
            from,
            to,
            step,
            body,
            span,
        })
    }

    pub(super) fn parse_for_in_tail(
        &mut self,
        start: Span,
        index: Option<String>,
        value: String,
    ) -> Option<ForInParts> {
        if !self.nth_identifier_is(0, "in") {
            self.error_here("E_PARSE_FOR", "expected `in` in for...in loop");
            return None;
        }
        self.bump();
        let iterable = self.parse_expr(0)?;
        self.expect(
            TokenKind::Newline,
            "expected newline after `for...in` iterable",
        )?;
        let body = self.parse_indented_block()?;
        let span = body
            .last()
            .map_or(iterable.span, |statement| statement.span);

        Some(ForInParts {
            start,
            index,
            value,
            iterable,
            body,
            span,
        })
    }

    pub(super) fn parse_function_decl(&mut self) -> Option<Stmt> {
        let start = self.current().span;
        let TokenKind::Identifier(name) = self.current().kind.clone() else {
            return None;
        };
        self.bump();
        self.expect(TokenKind::LParen, "expected `(` after function name")?;
        let mut params = Vec::new();

        if !self.at(TokenKind::RParen) {
            loop {
                let Some(param) = self.parse_function_param() else {
                    self.error_here("E_PARSE_FUNCTION", "expected parameter name");
                    return None;
                };
                params.push(param);

                if self.at(TokenKind::Comma) {
                    self.bump();
                    continue;
                }
                break;
            }
        }

        self.expect(TokenKind::RParen, "expected `)` after function parameters")?;
        self.expect(TokenKind::Arrow, "expected `=>` in function declaration")?;
        let (body, span) = if self.at(TokenKind::Newline) {
            self.bump();
            let block = self.parse_indented_block()?;
            let span = block.last().map_or(start, |statement| statement.span);
            (FunctionBody::Block(block), span)
        } else {
            let body = self.parse_expr(0)?;
            let span = body.span;
            (FunctionBody::Expr(body), span)
        };

        Some(Stmt {
            span: start.merge(span),
            kind: StmtKind::Function { name, params, body },
        })
    }

    pub(super) fn parse_function_param(&mut self) -> Option<FunctionParam> {
        let mut param = self.parse_function_param_head()?;
        if self.at(TokenKind::Eq) {
            if self.source_version < 5 {
                self.error_here(
                    "E_PARSE_FUNCTION",
                    "default parameters require the modern function subset",
                );
                return None;
            }
            self.bump();
            let value = self.parse_expr(0)?;
            param.span = param.span.merge(value.span);
            param.default_value = Some(value);
        }
        Some(param)
    }

    fn parse_function_param_head(&mut self) -> Option<FunctionParam> {
        let TokenKind::Identifier(first) = self.current().kind.clone() else {
            return None;
        };
        let start = self.current().span;

        if first == "series" && self.source_version >= 5 {
            self.bump();
            if matches!(&self.current().kind, TokenKind::Identifier(name)
                if matches!(name.as_str(), "series" | "simple" | "const" | "input"))
            {
                self.error_here("E_PARSE_FUNCTION", "expected type after `series`");
                return None;
            }
            let mut param = self.parse_function_param_head()?;
            let Some(type_name) = param.type_name else {
                self.error_here(
                    "E_PARSE_FUNCTION",
                    "expected typed parameter after `series`",
                );
                return None;
            };
            param.type_name = Some(format!("series {type_name}"));
            param.span = start.merge(param.span);
            return Some(param);
        }

        // Keep the qualifier in the type spelling: removing it would let a
        // constant argument silently weaken an explicitly series or simple parameter.
        if first == "simple" && self.source_version >= 5 {
            let TokenKind::Identifier(type_name) = self.tokens.get(self.pos + 1)?.kind.clone()
            else {
                return None;
            };
            if !matches!(
                type_name.as_str(),
                "int" | "float" | "bool" | "string" | "color"
            ) {
                self.error_here(
                    "E_PARSE_FUNCTION",
                    &format!("expected scalar type after `{first}`"),
                );
                return None;
            }
            let TokenKind::Identifier(name) = self.tokens.get(self.pos + 2)?.kind.clone() else {
                return None;
            };
            let end = self.tokens.get(self.pos + 2)?.span;
            for _ in 0..3 {
                self.bump();
            }
            return Some(FunctionParam {
                default_value: None,
                type_name: Some(format!("{first} {type_name}")),
                name,
                span: start.merge(end),
            });
        }

        if first == "array" && self.nth_at(1, TokenKind::Lt) {
            if self.nth_at(3, TokenKind::Gt) {
                let TokenKind::Identifier(element_type) =
                    self.tokens.get(self.pos + 2)?.kind.clone()
                else {
                    return None;
                };
                let TokenKind::Identifier(name) = self.tokens.get(self.pos + 4)?.kind.clone()
                else {
                    return None;
                };
                let end = self.tokens.get(self.pos + 4)?.span;
                for _ in 0..5 {
                    self.bump();
                }
                return Some(FunctionParam {
                    default_value: None,
                    type_name: Some(format!("array<{element_type}>")),
                    name,
                    span: start.merge(end),
                });
            }

            if self.nth_at(3, TokenKind::Dot) && self.nth_at(5, TokenKind::Gt) {
                let TokenKind::Identifier(namespace) = self.tokens.get(self.pos + 2)?.kind.clone()
                else {
                    return None;
                };
                let TokenKind::Identifier(type_name) = self.tokens.get(self.pos + 4)?.kind.clone()
                else {
                    return None;
                };
                let TokenKind::Identifier(name) = self.tokens.get(self.pos + 6)?.kind.clone()
                else {
                    return None;
                };
                let end = self.tokens.get(self.pos + 6)?.span;
                for _ in 0..7 {
                    self.bump();
                }
                return Some(FunctionParam {
                    default_value: None,
                    type_name: Some(format!("array<{namespace}.{type_name}>")),
                    name,
                    span: start.merge(end),
                });
            }
        }

        if self.nth_at(1, TokenKind::LBracket) && self.nth_at(2, TokenKind::RBracket) {
            let TokenKind::Identifier(name) = self.tokens.get(self.pos + 3)?.kind.clone() else {
                return None;
            };
            let end = self.tokens.get(self.pos + 3)?.span;
            for _ in 0..4 {
                self.bump();
            }
            return Some(FunctionParam {
                default_value: None,
                type_name: Some(format!("array<{first}>")),
                name,
                span: start.merge(end),
            });
        }

        if self.nth_at(1, TokenKind::Dot)
            && self.nth_at(3, TokenKind::LBracket)
            && self.nth_at(4, TokenKind::RBracket)
        {
            let TokenKind::Identifier(type_name) = self.tokens.get(self.pos + 2)?.kind.clone()
            else {
                return None;
            };
            let TokenKind::Identifier(name) = self.tokens.get(self.pos + 5)?.kind.clone() else {
                return None;
            };
            let end = self.tokens.get(self.pos + 5)?.span;
            for _ in 0..6 {
                self.bump();
            }
            return Some(FunctionParam {
                default_value: None,
                type_name: Some(format!("array<{first}.{type_name}>")),
                name,
                span: start.merge(end),
            });
        }

        if self.nth_at(1, TokenKind::Dot) {
            let TokenKind::Identifier(second) = self.tokens.get(self.pos + 2)?.kind.clone() else {
                return None;
            };
            let TokenKind::Identifier(name) = self.tokens.get(self.pos + 3)?.kind.clone() else {
                return None;
            };
            let end = self.tokens.get(self.pos + 3)?.span;
            self.bump();
            self.bump();
            self.bump();
            self.bump();
            return Some(FunctionParam {
                default_value: None,
                type_name: Some(format!("{first}.{second}")),
                name,
                span: start.merge(end),
            });
        }

        if let Some(next) = self.tokens.get(self.pos + 1)
            && let TokenKind::Identifier(name) = next.kind.clone()
        {
            let end = next.span;
            self.bump();
            self.bump();
            return Some(FunctionParam {
                default_value: None,
                type_name: Some(first),
                name,
                span: start.merge(end),
            });
        }

        self.bump();
        Some(FunctionParam {
            default_value: None,
            type_name: None,
            name: first,
            span: start,
        })
    }

    pub(super) fn parse_indented_block(&mut self) -> Option<Vec<Stmt>> {
        // Comment-only and blank physical lines produce newlines but no layout
        // tokens. Pine permits them before the first statement in a block.
        self.skip_newlines();
        self.expect(TokenKind::Indent, "expected indented block")?;
        let mut statements = Vec::new();

        loop {
            self.skip_newlines();
            if self.at(TokenKind::Dedent) {
                self.bump();
                break;
            }
            if self.at(TokenKind::Eof) {
                self.error_here("E_PARSE_BLOCK", "expected dedent before end of file");
                break;
            }

            match self.parse_stmt() {
                Some(statement) => statements.push(statement),
                None => self.recover_stmt(),
            }
            self.skip_statement_separator();
            self.skip_newlines();
        }

        if statements.is_empty() {
            self.error_here("E_PARSE_BLOCK", "expected statement in block");
            return None;
        }

        Some(statements)
    }

    fn compound_assignment_op(&self, offset: usize) -> Option<BinaryOp> {
        match self.tokens.get(self.pos + offset)?.kind {
            TokenKind::PlusEq => Some(BinaryOp::Add),
            TokenKind::MinusEq => Some(BinaryOp::Sub),
            TokenKind::StarEq => Some(BinaryOp::Mul),
            TokenKind::SlashEq => Some(BinaryOp::Div),
            TokenKind::PercentEq => Some(BinaryOp::Mod),
            _ => None,
        }
    }

    pub(super) fn looks_like_function_decl(&self) -> bool {
        if !self.nth_at(1, TokenKind::LParen) {
            return false;
        }

        let mut depth = 0_u32;
        let mut index = self.pos + 1;
        while let Some(token) = self.tokens.get(index) {
            match token.kind {
                TokenKind::Newline | TokenKind::Eof => return false,
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return self.tokens.get(index + 1).is_some_and(|next| {
                            std::mem::discriminant(&next.kind)
                                == std::mem::discriminant(&TokenKind::Arrow)
                        });
                    }
                }
                _ => {}
            }
            index += 1;
        }

        false
    }

    fn looks_like_tuple_decl(&self) -> bool {
        if !self.at(TokenKind::LBracket) {
            return false;
        }

        let mut depth = 0_u32;
        let mut index = self.pos;
        while let Some(token) = self.tokens.get(index) {
            match token.kind {
                TokenKind::Newline | TokenKind::Eof => return false,
                TokenKind::LBracket => depth += 1,
                TokenKind::RBracket => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return self
                            .tokens
                            .get(index + 1)
                            .is_some_and(|next| matches!(next.kind, TokenKind::Eq));
                    }
                }
                _ => {}
            }
            index += 1;
        }

        false
    }
}
