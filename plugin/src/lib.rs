use std::collections::{HashMap, HashSet};

use swc_core::{
    common::{SyntaxContext, DUMMY_SP},
    ecma::{
        ast::*,
        utils::{private_ident, quote_ident},
        visit::{VisitMut, VisitMutWith},
    },
    plugin::{plugin_transform, proxies::TransformPluginProgramMetadata},
};

#[plugin_transform]
pub fn process_transform(program: Program, metadata: TransformPluginProgramMetadata) -> Program {
    let mut transformer = TsPatternTransformer {
        unresolved_ctxt: SyntaxContext::empty().apply_mark(metadata.unresolved_mark),
        ..Default::default()
    };
    let mut program = program;
    program.visit_mut_with(&mut transformer);
    program
}

#[derive(Default)]
struct TsPatternTransformer {
    unresolved_ctxt: SyntaxContext,
    match_imports: HashSet<Id>,
    p_imports: HashSet<Id>,
    namespaces: HashSet<Id>,
    const_bindings: HashMap<Id, Expr>,
    non_exhaustive_error: Option<Expr>,
    needs_non_exhaustive_error_import: bool,
}

#[derive(Clone)]
struct MatchChain {
    input: Box<Expr>,
    arms: Vec<MatchArm>,
    fallback: Fallback,
}

#[derive(Clone)]
struct MatchArm {
    patterns: Vec<Box<Expr>>,
    handler_patterns: Vec<Box<Expr>>,
    guard: Option<Box<Expr>>,
    handler: Box<Expr>,
}

#[derive(Clone)]
enum Fallback {
    Otherwise(Box<Expr>),
    Matched(Box<Expr>),
    Exhaustive,
}

#[derive(Clone)]
struct SelectionBinding {
    name: Option<String>,
    value: Expr,
}

impl VisitMut for TsPatternTransformer {
    fn visit_mut_module(&mut self, module: &mut Module) {
        self.collect_imports(module);
        self.collect_const_bindings(module);
        module.visit_mut_children_with(self);
        if self.needs_non_exhaustive_error_import {
            add_non_exhaustive_error_import(module);
        }
    }

    fn visit_mut_block_stmt(&mut self, block: &mut BlockStmt) {
        block.visit_mut_children_with(self);
        block.stmts = self.rewrite_block_stmts(std::mem::take(&mut block.stmts));
    }

    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        expr.visit_mut_children_with(self);

        if let Some(chain) = self.parse_match_chain(expr) {
            if let Some(compiled) = self.compile_chain(chain) {
                *expr = compiled;
            }
        }
    }

    fn visit_mut_arrow_expr(&mut self, arrow: &mut ArrowExpr) {
        if let BlockStmtOrExpr::Expr(expr) = &*arrow.body {
            if let Some(chain) = self.parse_match_chain(expr) {
                if let Some(stmts) = self.compile_arrow_match_stmts(chain) {
                    arrow.body = Box::new(BlockStmtOrExpr::BlockStmt(BlockStmt {
                        span: DUMMY_SP,
                        ctxt: Default::default(),
                        stmts,
                    }));
                    return;
                }
            }
        }

        arrow.visit_mut_children_with(self);
    }
}

impl TsPatternTransformer {
    fn collect_imports(&mut self, module: &Module) {
        for item in &module.body {
            let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item else {
                continue;
            };

            if !is_ts_pattern_import(&import.src.value.to_string_lossy()) {
                continue;
            }

            for specifier in &import.specifiers {
                match specifier {
                    ImportSpecifier::Named(named) => {
                        let imported = named
                            .imported
                            .as_ref()
                            .map(module_export_name)
                            .unwrap_or_else(|| named.local.sym.to_string());

                        match imported.as_str() {
                            "match" => {
                                self.match_imports.insert(named.local.to_id());
                            }
                            "P" | "Pattern" => {
                                self.p_imports.insert(named.local.to_id());
                            }
                            "NonExhaustiveError" => {
                                self.non_exhaustive_error =
                                    Some(Expr::Ident(named.local.clone().into()));
                            }
                            _ => {}
                        }
                    }
                    ImportSpecifier::Namespace(namespace) => {
                        self.namespaces.insert(namespace.local.to_id());
                        self.non_exhaustive_error = Some(member_expr(
                            Expr::Ident(namespace.local.clone().into()),
                            "NonExhaustiveError",
                        ));
                    }
                    ImportSpecifier::Default(_) => {}
                }
            }
        }
    }

    fn collect_const_bindings(&mut self, module: &Module) {
        let mut push_binding = |name: &Pat, init: &Option<Box<Expr>>| {
            let Pat::Ident(ident) = name else {
                return;
            };
            let Some(init) = init else {
                return;
            };
            self.const_bindings
                .insert(ident.id.to_id(), (**init).clone());
        };

        for item in &module.body {
            match item {
                ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) if var.kind == VarDeclKind::Const => {
                    for decl in &var.decls {
                        push_binding(&decl.name, &decl.init);
                    }
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl {
                    decl: Decl::Var(var),
                    ..
                })) if var.kind == VarDeclKind::Const => {
                    for decl in &var.decls {
                        push_binding(&decl.name, &decl.init);
                    }
                }
                _ => {}
            }
        }
    }

    fn resolve_pattern_expr<'a>(&'a self, expr: &'a Expr) -> &'a Expr {
        let mut current = expr;
        for _ in 0..16 {
            current = match current {
                Expr::TsConstAssertion(assertion) => &assertion.expr,
                Expr::TsAs(assertion) => &assertion.expr,
                Expr::TsSatisfies(assertion) => &assertion.expr,
                Expr::Ident(ident) => match self.const_bindings.get(&ident.to_id()) {
                    Some(next) => next,
                    None => return current,
                },
                _ => return current,
            };
        }
        current
    }

    fn parse_match_chain(&self, expr: &Expr) -> Option<MatchChain> {
        let call = as_call(expr)?;
        let (method, receiver) = as_method_call(call)?;

        let fallback = match method.as_str() {
            "otherwise" => {
                if call.args.len() != 1 {
                    return None;
                }
                Fallback::Otherwise(call.args[0].expr.clone())
            }
            "exhaustive" => {
                if !call.args.is_empty() {
                    return None;
                }
                Fallback::Exhaustive
            }
            _ => return None,
        };

        let (input, arms) = self.collect_arms(receiver)?;
        if arms.is_empty() {
            return None;
        }

        Some(MatchChain {
            input,
            arms,
            fallback,
        })
    }

    fn collect_arms(&self, expr: &Expr) -> Option<(Box<Expr>, Vec<MatchArm>)> {
        let call = as_call(expr)?;

        if self.is_match_call(call) {
            if call.args.len() != 1 {
                return None;
            }
            return Some((call.args[0].expr.clone(), Vec::new()));
        }

        let (method, receiver) = as_method_call(call)?;
        let (input, mut arms) = self.collect_arms(receiver)?;

        match method.as_str() {
            "with" => {
                let arm = parse_with_arm(call)?;
                arms.push(arm);
            }
            "when" => {
                let arm = parse_when_arm(call)?;
                arms.push(arm);
            }
            _ => return None,
        }

        Some((input, arms))
    }

    fn is_match_call(&self, call: &CallExpr) -> bool {
        match &call.callee {
            Callee::Expr(callee) => match &**callee {
                Expr::Ident(ident) => self.match_imports.contains(&ident.to_id()),
                Expr::Member(member) => self.is_namespace_member(member, "match"),
                _ => false,
            },
            _ => false,
        }
    }

    fn is_p_member(&self, expr: &Expr, name: &str) -> bool {
        let expr = self.resolve_pattern_expr(expr);
        let Expr::Member(member) = expr else {
            return false;
        };

        if member_name(member) != Some(name) {
            return false;
        }

        match &*member.obj {
            Expr::Ident(ident) => self.p_imports.contains(&ident.to_id()),
            Expr::Member(parent) => self.is_namespace_member(parent, "P"),
            _ => false,
        }
    }

    fn is_p_call<'a>(&'a self, expr: &'a Expr, name: &str) -> Option<&'a CallExpr> {
        let call = as_call(self.resolve_pattern_expr(expr))?;
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };

        if self.is_p_member(callee, name) {
            Some(call)
        } else {
            None
        }
    }

    fn is_p_chain_call<'a>(
        &'a self,
        expr: &'a Expr,
        base: &str,
        method: &str,
    ) -> Option<&'a CallExpr> {
        let call = as_call(self.resolve_pattern_expr(expr))?;
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        let Expr::Member(member) = &**callee else {
            return None;
        };
        if member_name(member) != Some(method) {
            return None;
        }
        self.is_p_member(&member.obj, base).then_some(call)
    }

    fn select_call<'a>(&'a self, expr: &'a Expr) -> Option<(&'a CallExpr, Option<&'a Expr>)> {
        let call = as_call(self.resolve_pattern_expr(expr))?;
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        let Expr::Member(member) = &**callee else {
            return None;
        };
        if member_name(member) != Some("select") {
            return None;
        }

        if matches!(&*member.obj, Expr::Ident(_) | Expr::Member(_))
            && self.is_p_member(callee, "select")
        {
            return Some((call, None));
        }

        Some((call, Some(&member.obj)))
    }

    fn is_namespace_member(&self, member: &MemberExpr, name: &str) -> bool {
        if member_name(member) != Some(name) {
            return false;
        }

        match &*member.obj {
            Expr::Ident(ident) => self.namespaces.contains(&ident.to_id()),
            _ => false,
        }
    }

    fn compile_arrow_match_stmts(&mut self, chain: MatchChain) -> Option<Vec<Stmt>> {
        if chain.arms.iter().all(is_switchable_arm) {
            return Some(self.compile_switch_stmts(chain));
        }

        self.compile_return_stmts(chain)
    }

    fn compile_chain(&mut self, chain: MatchChain) -> Option<Expr> {
        self.compile_ternary_chain(chain)
    }

    fn compile_switch_stmts(&mut self, chain: MatchChain) -> Vec<Stmt> {
        let input_ident = private_ident!("_tsPatternInput");
        let input_expr = ident_expr(&input_ident);
        let mut cases = Vec::new();

        for arm in chain.arms {
            let handler_call =
                handler_result(arm.handler, vec![input_expr.clone(), input_expr.clone()]);
            let mut arm_patterns = arm.patterns.into_iter().peekable();

            while let Some(pattern) = arm_patterns.next() {
                let consequent = if arm_patterns.peek().is_some() {
                    Vec::new()
                } else {
                    vec![return_stmt(handler_call.clone())]
                };

                cases.push(SwitchCase {
                    span: DUMMY_SP,
                    test: Some(pattern),
                    cons: consequent,
                });
            }
        }

        let exhaustive_error_callee =
            matches!(chain.fallback, Fallback::Exhaustive).then(|| self.exhaustive_error_callee());
        cases.push(SwitchCase {
            span: DUMMY_SP,
            test: None,
            cons: fallback_stmts(chain.fallback, input_expr, exhaustive_error_callee),
        });

        vec![
            const_stmt(input_ident.clone(), *chain.input),
            Stmt::Switch(SwitchStmt {
                span: DUMMY_SP,
                discriminant: Box::new(ident_expr(&input_ident)),
                cases,
            }),
        ]
    }

    fn compile_return_stmts(&mut self, chain: MatchChain) -> Option<Vec<Stmt>> {
        let input_ident = private_ident!("_tsPatternInput");
        let input_expr = ident_expr(&input_ident);
        let mut stmts = vec![const_stmt(input_ident, *chain.input)];

        stmts.extend(self.compile_return_body(
            input_expr.clone(),
            input_expr,
            chain.arms,
            chain.fallback,
            false,
        )?);
        Some(stmts)
    }

    fn compile_return_body(
        &mut self,
        match_input_expr: Expr,
        handler_input_expr: Expr,
        arms: Vec<MatchArm>,
        fallback: Fallback,
        known_match_object: bool,
    ) -> Option<Vec<Stmt>> {
        if known_match_object {
            if let Some((prefix, prefixed_arms)) = common_object_prefix(&arms) {
                let cached = private_ident_for_prop("_tsPattern", &prefix);
                let cached_expr = ident_expr(&cached);
                let mut stmts = vec![const_stmt(
                    cached,
                    prop_access(match_input_expr.clone(), &prefix)?,
                )];
                stmts.extend(self.compile_return_body(
                    cached_expr,
                    handler_input_expr,
                    prefixed_arms,
                    fallback,
                    false,
                )?);
                return Some(stmts);
            }
        }

        if let Some(path) = common_switch_path(&arms) {
            return self.compile_return_path_switch(
                match_input_expr,
                handler_input_expr,
                arms,
                fallback,
                &path,
                known_match_object,
            );
        }

        let mut stmts = Vec::new();

        for arm in arms {
            let handler_input = arm_handler_input(
                self,
                match_input_expr.clone(),
                handler_input_expr.clone(),
                &arm,
            )?;
            stmts.push(Stmt::If(IfStmt {
                span: DUMMY_SP,
                test: Box::new(arm_test(
                    self,
                    match_input_expr.clone(),
                    handler_input_expr.clone(),
                    &arm,
                    known_match_object,
                )?),
                cons: Box::new(return_stmt(handler_result(
                    arm.handler,
                    vec![handler_input, handler_input_expr.clone()],
                ))),
                alt: None,
            }));
        }

        let exhaustive_error_callee =
            matches!(fallback, Fallback::Exhaustive).then(|| self.exhaustive_error_callee());
        stmts.extend(fallback_stmts(
            fallback,
            handler_input_expr,
            exhaustive_error_callee,
        ));
        Some(stmts)
    }

    fn compile_return_path_switch(
        &mut self,
        match_input_expr: Expr,
        handler_input_expr: Expr,
        arms: Vec<MatchArm>,
        fallback: Fallback,
        path: &[PropName],
        known_match_object: bool,
    ) -> Option<Vec<Stmt>> {
        let switch_expr = prop_access_path(match_input_expr.clone(), path)?;
        let exhaustive_error_callee =
            matches!(fallback, Fallback::Exhaustive).then(|| self.exhaustive_error_callee());
        let default_fallback = fallback_stmts(
            fallback.clone(),
            handler_input_expr.clone(),
            exhaustive_error_callee.clone(),
        );
        let mut cases = Vec::new();

        for (value, group) in group_arms_by_path(arms, path)? {
            let (group_arms, group_fallback) = strip_group_arms(group, path, fallback.clone())?;
            let cons = if group_arms.is_empty() {
                fallback_stmts(
                    group_fallback.clone(),
                    handler_input_expr.clone(),
                    matches!(group_fallback, Fallback::Exhaustive)
                        .then(|| self.exhaustive_error_callee()),
                )
            } else {
                self.compile_return_body(
                    match_input_expr.clone(),
                    handler_input_expr.clone(),
                    group_arms,
                    group_fallback,
                    true,
                )?
            };

            cases.push(SwitchCase {
                span: DUMMY_SP,
                test: Some(Box::new(value)),
                cons,
            });
        }

        cases.push(SwitchCase {
            span: DUMMY_SP,
            test: None,
            cons: default_fallback.clone(),
        });

        Some(vec![
            Stmt::If(IfStmt {
                span: DUMMY_SP,
                test: Box::new(unary(
                    UnaryOp::Bang,
                    paren_expr(object_path_base_test(
                        match_input_expr.clone(),
                        path,
                        known_match_object,
                    )?),
                )),
                cons: Box::new(Stmt::Block(BlockStmt {
                    span: DUMMY_SP,
                    ctxt: Default::default(),
                    stmts: default_fallback,
                })),
                alt: None,
            }),
            Stmt::Switch(SwitchStmt {
                span: DUMMY_SP,
                discriminant: Box::new(switch_expr),
                cases,
            }),
        ])
    }

    fn compile_assign_match_stmts(&mut self, target: Expr, chain: MatchChain) -> Option<Vec<Stmt>> {
        let (input_expr, mut stmts) = if can_inline_input(&chain.input) {
            (*chain.input, Vec::new())
        } else {
            let input_ident = private_ident!("_tsPatternInput");
            (
                ident_expr(&input_ident),
                vec![const_stmt(input_ident, *chain.input)],
            )
        };

        stmts.extend(self.compile_assign_body(
            target,
            input_expr.clone(),
            input_expr,
            chain.arms,
            chain.fallback,
            false,
        )?);
        Some(stmts)
    }

    fn compile_assign_body(
        &mut self,
        target: Expr,
        match_input_expr: Expr,
        handler_input_expr: Expr,
        arms: Vec<MatchArm>,
        fallback: Fallback,
        known_match_object: bool,
    ) -> Option<Vec<Stmt>> {
        if known_match_object {
            if let Some((prefix, prefixed_arms)) = common_object_prefix(&arms) {
                let cached = private_ident_for_prop("_tsPattern", &prefix);
                let cached_expr = ident_expr(&cached);
                let mut stmts = vec![const_stmt(
                    cached,
                    prop_access(match_input_expr.clone(), &prefix)?,
                )];
                stmts.extend(self.compile_assign_body(
                    target,
                    cached_expr,
                    handler_input_expr,
                    prefixed_arms,
                    fallback,
                    false,
                )?);
                return Some(stmts);
            }
        }

        if let Some(path) = common_switch_path(&arms) {
            return self.compile_assign_path_switch(
                target,
                match_input_expr,
                handler_input_expr,
                arms,
                fallback,
                &path,
                known_match_object,
            );
        }

        let mut current = fallback_stmt(
            target.clone(),
            fallback,
            handler_input_expr.clone(),
            &mut || self.exhaustive_error_callee(),
        );

        for arm in arms.into_iter().rev() {
            let handler_input = arm_handler_input(
                self,
                match_input_expr.clone(),
                handler_input_expr.clone(),
                &arm,
            )?;
            current = Stmt::If(IfStmt {
                span: DUMMY_SP,
                test: Box::new(arm_test(
                    self,
                    match_input_expr.clone(),
                    handler_input_expr.clone(),
                    &arm,
                    known_match_object,
                )?),
                cons: Box::new(assign_stmt(
                    target.clone(),
                    handler_result(arm.handler, vec![handler_input, handler_input_expr.clone()]),
                )),
                alt: Some(Box::new(current)),
            });
        }

        Some(vec![current])
    }

    fn compile_assign_path_switch(
        &mut self,
        target: Expr,
        match_input_expr: Expr,
        handler_input_expr: Expr,
        arms: Vec<MatchArm>,
        fallback: Fallback,
        path: &[PropName],
        known_match_object: bool,
    ) -> Option<Vec<Stmt>> {
        let switch_expr = prop_access_path(match_input_expr.clone(), path)?;
        let default_fallback = fallback_stmt(
            target.clone(),
            fallback.clone(),
            handler_input_expr.clone(),
            &mut || self.exhaustive_error_callee(),
        );
        let mut cases = Vec::new();

        for (value, group) in group_arms_by_path(arms, path)? {
            let (group_arms, group_fallback) = strip_group_arms(group, path, fallback.clone())?;
            let mut cons = if group_arms.is_empty() {
                vec![fallback_stmt(
                    target.clone(),
                    group_fallback,
                    handler_input_expr.clone(),
                    &mut || self.exhaustive_error_callee(),
                )]
            } else {
                self.compile_assign_body(
                    target.clone(),
                    match_input_expr.clone(),
                    handler_input_expr.clone(),
                    group_arms,
                    group_fallback,
                    true,
                )?
            };
            cons.push(break_stmt());

            cases.push(SwitchCase {
                span: DUMMY_SP,
                test: Some(Box::new(value)),
                cons,
            });
        }

        cases.push(SwitchCase {
            span: DUMMY_SP,
            test: None,
            cons: vec![default_fallback.clone(), break_stmt()],
        });

        Some(vec![Stmt::If(IfStmt {
            span: DUMMY_SP,
            test: Box::new(unary(
                UnaryOp::Bang,
                paren_expr(object_path_base_test(
                    match_input_expr.clone(),
                    path,
                    known_match_object,
                )?),
            )),
            cons: Box::new(default_fallback),
            alt: Some(Box::new(Stmt::Switch(SwitchStmt {
                span: DUMMY_SP,
                discriminant: Box::new(switch_expr),
                cases,
            }))),
        })])
    }

    fn rewrite_block_stmts(&mut self, stmts: Vec<Stmt>) -> Vec<Stmt> {
        let mut rewritten = Vec::new();

        for stmt in stmts {
            match stmt {
                Stmt::Decl(Decl::Var(mut var)) if var.decls.len() == 1 => {
                    let decl = &var.decls[0];
                    let Pat::Ident(_) = &decl.name else {
                        rewritten.push(Stmt::Decl(Decl::Var(var)));
                        continue;
                    };
                    let Some(init) = &decl.init else {
                        rewritten.push(Stmt::Decl(Decl::Var(var)));
                        continue;
                    };
                    let Some(chain) = self.parse_match_chain(init) else {
                        rewritten.push(Stmt::Decl(Decl::Var(var)));
                        continue;
                    };
                    let result = private_ident!("_tsPatternResult");
                    let Some(mut assign_stmts) =
                        self.compile_assign_match_stmts(ident_expr(&result), chain)
                    else {
                        rewritten.push(Stmt::Decl(Decl::Var(var)));
                        continue;
                    };

                    // Initialize the original binding only after the match succeeds.
                    // Declaring it earlier would end its TDZ and turn const into let.
                    var.decls[0].init = Some(Box::new(ident_expr(&result)));
                    rewritten.push(var_without_init(VarDeclKind::Let, result));
                    rewritten.append(&mut assign_stmts);
                    rewritten.push(Stmt::Decl(Decl::Var(var)));
                }
                other => rewritten.push(other),
            }
        }

        rewritten
    }

    fn compile_ternary_chain(&mut self, chain: MatchChain) -> Option<Expr> {
        if !can_inline_input(&chain.input) || !matches!(chain.fallback, Fallback::Otherwise(_)) {
            return None;
        }

        let input = *chain.input;
        let Fallback::Otherwise(fallback) = chain.fallback else {
            return None;
        };
        let mut expression = handler_result(fallback, vec![input.clone()]);

        for arm in chain.arms.into_iter().rev() {
            let handler_input = arm_handler_input(self, input.clone(), input.clone(), &arm)?;
            expression = cond_expr(
                arm_test(self, input.clone(), input.clone(), &arm, false)?,
                handler_result(arm.handler, vec![handler_input, input.clone()]),
                expression,
            );
        }

        Some(expression)
    }

    fn exhaustive_error_callee(&mut self) -> Expr {
        match &self.non_exhaustive_error {
            Some(callee) => callee.clone(),
            None => {
                self.needs_non_exhaustive_error_import = true;
                Expr::Ident(quote_ident!("NonExhaustiveError").into())
            }
        }
    }

    fn pattern_test(&self, value: Expr, pattern: &Expr) -> Option<Expr> {
        let mut selections = Vec::new();
        self.pattern_test_with_selections(value, pattern, &mut selections)
    }

    fn pattern_test_with_known_object(&self, value: Expr, pattern: &Expr) -> Option<Expr> {
        let mut selections = Vec::new();
        let pattern = self.resolve_pattern_expr(pattern);
        match pattern {
            Expr::Object(object) => {
                self.object_test_with_base(value, object, &mut selections, bool_lit(true))
            }
            _ => self.pattern_test_with_selections(value, pattern, &mut selections),
        }
    }

    fn pattern_test_with_selections(
        &self,
        value: Expr,
        pattern: &Expr,
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        let pattern = self.resolve_pattern_expr(pattern);

        if let Some((call, base)) = self.select_call(pattern) {
            return match (base, call.args.as_slice()) {
                (None, []) => {
                    selections.push(SelectionBinding { name: None, value });
                    Some(bool_lit(true))
                }
                (Some(base), []) => {
                    selections.push(SelectionBinding {
                        name: None,
                        value: value.clone(),
                    });
                    self.pattern_test_with_selections(value, base, selections)
                }
                (None, [arg]) => {
                    if let Some(name) = selection_name(&arg.expr) {
                        selections.push(SelectionBinding {
                            name: Some(name),
                            value,
                        });
                        Some(bool_lit(true))
                    } else {
                        selections.push(SelectionBinding {
                            name: None,
                            value: value.clone(),
                        });
                        self.pattern_test_with_selections(value, &arg.expr, selections)
                    }
                }
                (Some(base), [arg]) => {
                    if let Some(name) = selection_name(&arg.expr) {
                        selections.push(SelectionBinding {
                            name: Some(name),
                            value: value.clone(),
                        });
                        self.pattern_test_with_selections(value, base, selections)
                    } else {
                        None
                    }
                }
                (None, [name, inner]) => {
                    selections.push(SelectionBinding {
                        name: Some(selection_name(&name.expr)?),
                        value: value.clone(),
                    });
                    self.pattern_test_with_selections(value, &inner.expr, selections)
                }
                _ => None,
            };
        }

        if self.is_p_member(pattern, "_") {
            return Some(bool_lit(true));
        }

        for (name, type_name) in [
            ("string", "string"),
            ("number", "number"),
            ("boolean", "boolean"),
            ("bigint", "bigint"),
            ("symbol", "symbol"),
        ] {
            if self.is_p_member(pattern, name) {
                return Some(typeof_eq(value, type_name));
            }
        }

        if self.is_p_member(pattern, "nullish") {
            return Some(or(
                strict_eq(value.clone(), null_lit()),
                strict_eq(value, undefined_expr()),
            ));
        }

        if self.is_p_member(pattern, "nonNullable") {
            return Some(and(
                strict_ne(value.clone(), null_lit()),
                strict_ne(value, undefined_expr()),
            ));
        }

        if let Some(call) = self.is_p_call(pattern, "optional") {
            if call.args.len() != 1 {
                return None;
            }
            let mut inner_selections = Vec::new();
            let test = self.pattern_test_with_selections(
                value.clone(),
                &call.args[0].expr,
                &mut inner_selections,
            )?;
            selections.extend(
                inner_selections
                    .into_iter()
                    .map(|selection| SelectionBinding {
                        name: selection.name,
                        value: cond_expr(
                            strict_eq(value.clone(), undefined_expr()),
                            undefined_expr(),
                            selection.value,
                        ),
                    }),
            );
            return Some(or(strict_eq(value, undefined_expr()), test));
        }

        if let Some(call) = self.is_p_call(pattern, "not") {
            if call.args.len() != 1 {
                return None;
            }
            let mut inner_selections = Vec::new();
            let test = self.pattern_test_with_selections(
                value,
                &call.args[0].expr,
                &mut inner_selections,
            )?;
            if !inner_selections.is_empty() {
                return None;
            }
            return Some(unary(UnaryOp::Bang, test));
        }

        if let Some(call) = self.is_p_call(pattern, "union") {
            if call.args.is_empty() {
                return None;
            }

            return call
                .args
                .iter()
                .map(|arg| {
                    let mut inner_selections = Vec::new();
                    let test = self.pattern_test_with_selections(
                        value.clone(),
                        &arg.expr,
                        &mut inner_selections,
                    )?;
                    if !inner_selections.is_empty() {
                        return None;
                    }
                    Some(test)
                })
                .try_fold(None, |acc, test| {
                    let test = test?;
                    Some(Some(match acc {
                        Some(acc) => or(acc, test),
                        None => test,
                    }))
                })?;
        }

        if let Some(call) = self.is_p_call(pattern, "array") {
            if call.args.len() > 1 {
                return None;
            }

            let array_test = array_is_array(value.clone());
            if call.args.is_empty() {
                return Some(array_test);
            }

            let item_ident = private_ident!("_tsPatternItem");
            let mut item_selections = Vec::new();
            let item_test = self.pattern_test_with_selections(
                ident_expr(&item_ident),
                &call.args[0].expr,
                &mut item_selections,
            )?;
            if !item_selections.is_empty() {
                return None;
            }
            return Some(and(
                array_test,
                call_member(
                    member_expr(value, "every"),
                    vec![Expr::Arrow(ArrowExpr {
                        span: DUMMY_SP,
                        ctxt: Default::default(),
                        params: vec![Pat::Ident(item_ident.into())],
                        body: Box::new(BlockStmtOrExpr::Expr(Box::new(item_test))),
                        is_async: false,
                        is_generator: false,
                        type_params: None,
                        return_type: None,
                    })],
                ),
            ));
        }

        if let Some(call) = self.is_p_call(pattern, "set") {
            if call.args.len() > 1 {
                return None;
            }

            let set_test = instance_of(value.clone(), "Set");
            if call.args.is_empty() {
                return Some(set_test);
            }

            let item_ident = private_ident!("_tsPatternItem");
            let mut item_selections = Vec::new();
            let item_test = self.pattern_test_with_selections(
                ident_expr(&item_ident),
                &call.args[0].expr,
                &mut item_selections,
            )?;
            if !item_selections.is_empty() {
                return None;
            }
            return Some(and(
                set_test,
                call_member(
                    member_expr(array_from(value), "every"),
                    vec![Expr::Arrow(ArrowExpr {
                        span: DUMMY_SP,
                        ctxt: Default::default(),
                        params: vec![Pat::Ident(item_ident.into())],
                        body: Box::new(BlockStmtOrExpr::Expr(Box::new(item_test))),
                        is_async: false,
                        is_generator: false,
                        type_params: None,
                        return_type: None,
                    })],
                ),
            ));
        }

        if let Some(call) = self.is_p_call(pattern, "map") {
            if call.args.len() > 2 {
                return None;
            }

            let map_test = instance_of(value.clone(), "Map");
            if call.args.is_empty() {
                return Some(map_test);
            }
            if call.args.len() != 2 {
                return None;
            }

            let key_ident = private_ident!("_tsPatternKey");
            let value_ident = private_ident!("_tsPatternValue");
            let mut key_selections = Vec::new();
            let key_test = self.pattern_test_with_selections(
                ident_expr(&key_ident),
                &call.args[0].expr,
                &mut key_selections,
            )?;
            if !key_selections.is_empty() {
                return None;
            }
            let mut value_selections = Vec::new();
            let value_test = self.pattern_test_with_selections(
                ident_expr(&value_ident),
                &call.args[1].expr,
                &mut value_selections,
            )?;
            if !value_selections.is_empty() {
                return None;
            }
            return Some(and(
                map_test,
                call_member(
                    member_expr(
                        array_from(call_member(member_expr(value, "entries"), vec![])),
                        "every",
                    ),
                    vec![Expr::Arrow(ArrowExpr {
                        span: DUMMY_SP,
                        ctxt: Default::default(),
                        params: vec![Pat::Array(ArrayPat {
                            span: DUMMY_SP,
                            elems: vec![
                                Some(Pat::Ident(key_ident.into())),
                                Some(Pat::Ident(value_ident.into())),
                            ],
                            optional: false,
                            type_ann: None,
                        })],
                        body: Box::new(BlockStmtOrExpr::Expr(Box::new(and(key_test, value_test)))),
                        is_async: false,
                        is_generator: false,
                        type_params: None,
                        return_type: None,
                    })],
                ),
            ));
        }

        if let Some(call) = self.is_p_call(pattern, "record") {
            if call.args.is_empty() || call.args.len() > 2 {
                return None;
            }

            let key_ident = private_ident!("_tsPatternKey");
            let value_ident = private_ident!("_tsPatternValue");
            let key_pattern = (call.args.len() == 2).then_some(&call.args[0].expr);
            let value_pattern = &call.args[call.args.len() - 1].expr;
            let mut key_selections = Vec::new();
            let key_test = if let Some(key_pattern) = key_pattern {
                let direct_key_test = self.pattern_test_with_selections(
                    ident_expr(&key_ident),
                    key_pattern,
                    &mut key_selections,
                )?;
                if key_selections.is_empty() {
                    let coerced_key = call_expr(
                        Expr::Ident(quote_ident!("Number").into()),
                        vec![ident_expr(&key_ident)],
                    );
                    or(
                        direct_key_test,
                        and(
                            typeof_eq(ident_expr(&key_ident), "string"),
                            and(
                                unary(
                                    UnaryOp::Bang,
                                    call_member(
                                        member_expr(
                                            Expr::Ident(quote_ident!("Number").into()),
                                            "isNaN",
                                        ),
                                        vec![coerced_key.clone()],
                                    ),
                                ),
                                self.pattern_test(coerced_key, key_pattern)?,
                            ),
                        ),
                    )
                } else {
                    direct_key_test
                }
            } else {
                typeof_eq(ident_expr(&key_ident), "string")
            };
            let mut value_selections = Vec::new();
            let value_test = self.pattern_test_with_selections(
                ident_expr(&value_ident),
                value_pattern,
                &mut value_selections,
            )?;
            selections.extend(record_aggregate_selections(
                value.clone(),
                &key_ident,
                key_selections,
                &value_ident,
                value_selections,
            )?);
            return Some(and(
                and(
                    and(
                        strict_ne(value.clone(), null_lit()),
                        typeof_eq(value.clone(), "object"),
                    ),
                    unary(UnaryOp::Bang, array_is_array(value.clone())),
                ),
                call_member(
                    member_expr(record_entries(value), "every"),
                    vec![Expr::Arrow(ArrowExpr {
                        span: DUMMY_SP,
                        ctxt: Default::default(),
                        params: vec![Pat::Array(ArrayPat {
                            span: DUMMY_SP,
                            elems: vec![
                                Some(Pat::Ident(key_ident.into())),
                                Some(Pat::Ident(value_ident.into())),
                            ],
                            optional: false,
                            type_ann: None,
                        })],
                        body: Box::new(BlockStmtOrExpr::Expr(Box::new(and(key_test, value_test)))),
                        is_async: false,
                        is_generator: false,
                        type_params: None,
                        return_type: None,
                    })],
                ),
            ));
        }

        if let Some(call) = self.is_p_call(pattern, "intersection") {
            if call.args.is_empty() {
                return None;
            }

            return call
                .args
                .iter()
                .map(|arg| {
                    let mut inner_selections = Vec::new();
                    let test = self.pattern_test_with_selections(
                        value.clone(),
                        &arg.expr,
                        &mut inner_selections,
                    )?;
                    if !inner_selections.is_empty() {
                        return None;
                    }
                    Some(test)
                })
                .try_fold(None, |acc, test| {
                    let test = test?;
                    Some(Some(match acc {
                        Some(acc) => and(acc, test),
                        None => test,
                    }))
                })?;
        }

        if let Some(call) = self.is_p_chain_call(pattern, "string", "startsWith") {
            if call.args.len() != 1 {
                return None;
            }
            return Some(and(
                typeof_eq(value.clone(), "string"),
                call_member(
                    member_expr(value, "startsWith"),
                    vec![(*call.args[0].expr).clone()],
                ),
            ));
        }

        if let Some(call) = self.is_p_chain_call(pattern, "string", "endsWith") {
            if call.args.len() != 1 {
                return None;
            }
            return Some(and(
                typeof_eq(value.clone(), "string"),
                call_member(
                    member_expr(value, "endsWith"),
                    vec![(*call.args[0].expr).clone()],
                ),
            ));
        }

        if let Some(call) = self.is_p_chain_call(pattern, "string", "includes") {
            if call.args.len() != 1 {
                return None;
            }
            return Some(and(
                typeof_eq(value.clone(), "string"),
                call_member(
                    member_expr(value, "includes"),
                    vec![(*call.args[0].expr).clone()],
                ),
            ));
        }

        if let Some(call) = self.is_p_chain_call(pattern, "string", "regex") {
            if call.args.len() != 1 {
                return None;
            }
            return Some(and(
                typeof_eq(value.clone(), "string"),
                call_member(
                    member_expr(
                        call_expr(
                            Expr::Ident(quote_ident!("RegExp").into()),
                            vec![(*call.args[0].expr).clone()],
                        ),
                        "test",
                    ),
                    vec![value],
                ),
            ));
        }

        if let Some(call) = self.is_p_chain_call(pattern, "string", "minLength") {
            if call.args.len() != 1 {
                return None;
            }
            return Some(and(
                typeof_eq(value.clone(), "string"),
                bin(
                    BinaryOp::GtEq,
                    member_expr(value, "length"),
                    (*call.args[0].expr).clone(),
                ),
            ));
        }

        if let Some(call) = self.is_p_chain_call(pattern, "string", "length") {
            if call.args.len() != 1 {
                return None;
            }
            return Some(and(
                typeof_eq(value.clone(), "string"),
                strict_eq(member_expr(value, "length"), (*call.args[0].expr).clone()),
            ));
        }

        if let Some(call) = self.is_p_chain_call(pattern, "string", "maxLength") {
            if call.args.len() != 1 {
                return None;
            }
            return Some(and(
                typeof_eq(value.clone(), "string"),
                bin(
                    BinaryOp::LtEq,
                    member_expr(value, "length"),
                    (*call.args[0].expr).clone(),
                ),
            ));
        }

        if let Some(call) = self.is_p_chain_call(pattern, "number", "between") {
            if call.args.len() != 2 {
                return None;
            }
            return Some(and(
                typeof_eq(value.clone(), "number"),
                and(
                    bin(BinaryOp::GtEq, value.clone(), (*call.args[0].expr).clone()),
                    bin(BinaryOp::LtEq, value, (*call.args[1].expr).clone()),
                ),
            ));
        }

        for method in ["lt", "gt", "lte", "gte"] {
            if let Some(call) = self.is_p_chain_call(pattern, "number", method) {
                if call.args.len() != 1 {
                    return None;
                }
                let op = match method {
                    "lt" => BinaryOp::Lt,
                    "gt" => BinaryOp::Gt,
                    "lte" => BinaryOp::LtEq,
                    "gte" => BinaryOp::GtEq,
                    _ => unreachable!(),
                };
                return Some(and(
                    typeof_eq(value.clone(), "number"),
                    bin(op, value, (*call.args[0].expr).clone()),
                ));
            }
        }

        if self.is_p_chain_call(pattern, "number", "int").is_some() {
            return Some(and(
                typeof_eq(value.clone(), "number"),
                call_member(
                    member_expr(Expr::Ident(quote_ident!("Number").into()), "isInteger"),
                    vec![value],
                ),
            ));
        }

        if self.is_p_chain_call(pattern, "number", "finite").is_some() {
            return Some(and(
                typeof_eq(value.clone(), "number"),
                call_member(
                    member_expr(Expr::Ident(quote_ident!("Number").into()), "isFinite"),
                    vec![value],
                ),
            ));
        }

        if self
            .is_p_chain_call(pattern, "number", "positive")
            .is_some()
        {
            return Some(and(
                typeof_eq(value.clone(), "number"),
                bin(
                    BinaryOp::Gt,
                    value,
                    Expr::Lit(Lit::Num(Number {
                        span: DUMMY_SP,
                        value: 0.0,
                        raw: None,
                    })),
                ),
            ));
        }

        if self
            .is_p_chain_call(pattern, "number", "negative")
            .is_some()
        {
            return Some(and(
                typeof_eq(value.clone(), "number"),
                bin(
                    BinaryOp::Lt,
                    value,
                    Expr::Lit(Lit::Num(Number {
                        span: DUMMY_SP,
                        value: 0.0,
                        raw: None,
                    })),
                ),
            ));
        }

        if let Some(call) = self.is_p_call(pattern, "instanceOf") {
            if call.args.len() != 1 {
                return None;
            }
            return Some(Expr::Bin(BinExpr {
                span: DUMMY_SP,
                op: BinaryOp::InstanceOf,
                left: Box::new(value),
                right: call.args[0].expr.clone(),
            }));
        }

        if let Some(call) = self.is_p_call(pattern, "when") {
            if call.args.len() != 1 {
                return None;
            }
            return Some(predicate_result(&call.args[0].expr, value));
        }

        if is_zero_literal(pattern) {
            return Some(call_expr(
                member_expr(
                    Expr::Ident(Ident::new("Object".into(), DUMMY_SP, self.unresolved_ctxt)),
                    "is",
                ),
                vec![value, pattern.clone()],
            ));
        }

        match pattern {
            Expr::Lit(Lit::Str(_))
            | Expr::Lit(Lit::Num(_))
            | Expr::Lit(Lit::Bool(_))
            | Expr::Lit(Lit::BigInt(_))
            | Expr::Lit(Lit::Null(_)) => Some(strict_eq(value, pattern.clone())),
            Expr::Ident(ident) if ident.sym.as_ref() == "undefined" => {
                Some(strict_eq(value, undefined_expr()))
            }
            Expr::Tpl(tpl) if tpl.exprs.is_empty() && tpl.quasis.len() == 1 => {
                Some(strict_eq(value, pattern.clone()))
            }
            Expr::Unary(unary_expr)
                if unary_expr.op == UnaryOp::Minus && is_number_literal(&unary_expr.arg) =>
            {
                Some(strict_eq(value, pattern.clone()))
            }
            Expr::Object(object) => self.object_test(value, object, selections),
            Expr::Array(array) => self.array_test(value, array, selections),
            _ => None,
        }
    }

    fn object_test(
        &self,
        value: Expr,
        object: &ObjectLit,
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        self.object_test_with_base(
            value.clone(),
            object,
            selections,
            and(
                strict_ne(value.clone(), null_lit()),
                typeof_eq(value, "object"),
            ),
        )
    }

    fn object_test_with_base(
        &self,
        value: Expr,
        object: &ObjectLit,
        selections: &mut Vec<SelectionBinding>,
        base: Expr,
    ) -> Option<Expr> {
        object.props.iter().try_fold(base, |acc, prop| {
            let PropOrSpread::Prop(prop) = prop else {
                return None;
            };

            let Prop::KeyValue(key_value) = &**prop else {
                return None;
            };

            let prop_access = prop_access(value.clone(), &key_value.key)?;
            let test =
                self.pattern_test_with_selections(prop_access, &key_value.value, selections)?;
            let test = if self.is_optional_pattern(&key_value.value) {
                test
            } else {
                and(prop_in_object(&key_value.key, value.clone())?, test)
            };
            Some(and(acc, test))
        })
    }

    fn is_optional_pattern(&self, pattern: &Expr) -> bool {
        self.is_p_call(pattern, "optional").is_some()
    }

    fn array_test(
        &self,
        value: Expr,
        array: &ArrayLit,
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        let len = array.elems.len();
        let base = and(
            array_is_array(value.clone()),
            strict_eq(
                member_expr(value.clone(), "length"),
                Expr::Lit(Lit::Num(Number {
                    span: DUMMY_SP,
                    value: len as f64,
                    raw: None,
                })),
            ),
        );

        array
            .elems
            .iter()
            .enumerate()
            .try_fold(base, |acc, (index, elem)| {
                let elem = elem.as_ref()?;
                if elem.spread.is_some() {
                    return None;
                }

                let item = computed_member_expr(
                    value.clone(),
                    Expr::Lit(Lit::Num(Number {
                        span: DUMMY_SP,
                        value: index as f64,
                        raw: None,
                    })),
                );
                let test = self.pattern_test_with_selections(item, &elem.expr, selections)?;
                Some(and(acc, test))
            })
    }
}

fn module_export_name(name: &ModuleExportName) -> String {
    match name {
        ModuleExportName::Ident(ident) => ident.sym.to_string(),
        ModuleExportName::Str(str) => str.value.to_string_lossy().to_string(),
    }
}

fn is_ts_pattern_import(source: &str) -> bool {
    source == "ts-pattern" || source.starts_with("npm:ts-pattern")
}

fn add_non_exhaustive_error_import(module: &mut Module) {
    let specifier = || {
        ImportSpecifier::Named(ImportNamedSpecifier {
            span: DUMMY_SP,
            local: quote_ident!("NonExhaustiveError").into(),
            imported: None,
            is_type_only: false,
        })
    };

    for item in &mut module.body {
        let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item else {
            continue;
        };

        if !import.type_only && is_ts_pattern_import(&import.src.value.to_string_lossy()) {
            import.specifiers.push(specifier());
            return;
        }
    }

    module.body.insert(
        0,
        ModuleItem::ModuleDecl(ModuleDecl::Import(ImportDecl {
            span: DUMMY_SP,
            specifiers: vec![specifier()],
            src: Box::new(Str {
                span: DUMMY_SP,
                value: "ts-pattern".into(),
                raw: None,
            }),
            type_only: false,
            with: None,
            phase: Default::default(),
        })),
    );
}

fn parse_with_arm(call: &CallExpr) -> Option<MatchArm> {
    if call.args.len() < 2 {
        return None;
    }

    let mut args = call
        .args
        .iter()
        .map(|arg| arg.expr.clone())
        .collect::<Vec<_>>();
    let handler = args.pop()?;
    let guard = if args.len() >= 2 && is_function_expr(args.last()?) {
        args.pop()
    } else {
        None
    };

    Some(MatchArm {
        handler_patterns: args.clone(),
        patterns: args,
        guard,
        handler,
    })
}

fn parse_when_arm(call: &CallExpr) -> Option<MatchArm> {
    if call.args.len() != 2 {
        return None;
    }

    let pattern = Box::new(Expr::Invalid(Invalid { span: DUMMY_SP }));
    Some(MatchArm {
        patterns: vec![pattern.clone()],
        handler_patterns: vec![pattern],
        guard: Some(call.args[0].expr.clone()),
        handler: call.args[1].expr.clone(),
    })
}

fn arm_test(
    transformer: &TsPatternTransformer,
    match_input: Expr,
    guard_input: Expr,
    arm: &MatchArm,
    known_match_object: bool,
) -> Option<Expr> {
    let pattern_test = if arm.patterns.len() == 1 && matches!(*arm.patterns[0], Expr::Invalid(_)) {
        bool_lit(true)
    } else {
        arm.patterns
            .iter()
            .map(|pattern| {
                if known_match_object {
                    transformer.pattern_test_with_known_object(match_input.clone(), pattern)
                } else {
                    transformer.pattern_test(match_input.clone(), pattern)
                }
            })
            .try_fold(None, |acc, test| {
                let test = test?;
                Some(Some(match acc {
                    Some(acc) => or(acc, test),
                    None => test,
                }))
            })??
    };

    match &arm.guard {
        Some(guard) => Some(and(pattern_test, guard_result(guard, guard_input))),
        None => Some(pattern_test),
    }
}

fn arm_handler_input(
    transformer: &TsPatternTransformer,
    match_input_expr: Expr,
    handler_input_expr: Expr,
    arm: &MatchArm,
) -> Option<Expr> {
    let mut selections = Vec::new();

    for pattern in &arm.patterns {
        transformer.pattern_test_with_selections(
            match_input_expr.clone(),
            pattern,
            &mut selections,
        )?;
    }

    if selections.is_empty() {
        return Some(handler_input_expr);
    }

    if arm.patterns.len() != 1 {
        return None;
    }

    let anonymous_count = selections
        .iter()
        .filter(|selection| selection.name.is_none())
        .count();
    if anonymous_count > 1 || (anonymous_count == 1 && selections.len() > 1) {
        return None;
    }

    if anonymous_count == 1 {
        return selections
            .into_iter()
            .find_map(|selection| selection.name.is_none().then_some(selection.value));
    }

    Some(named_selection_object(selections))
}

fn named_selection_object(selections: Vec<SelectionBinding>) -> Expr {
    Expr::Object(ObjectLit {
        span: DUMMY_SP,
        props: selections
            .into_iter()
            .map(|selection| {
                PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                    key: PropName::Ident(IdentName::new(
                        selection.name.expect("named selection").into(),
                        DUMMY_SP,
                    )),
                    value: Box::new(selection.value),
                })))
            })
            .collect(),
    })
}

fn selection_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(Lit::Str(str)) => Some(str.value.to_string_lossy().to_string()),
        Expr::Tpl(tpl) if tpl.exprs.is_empty() && tpl.quasis.len() == 1 => {
            Some(tpl.quasis[0].raw.to_string())
        }
        _ => None,
    }
}

fn guard_result(guard: &Expr, input_expr: Expr) -> Expr {
    predicate_result(guard, input_expr)
}

fn predicate_result(predicate: &Expr, input_expr: Expr) -> Expr {
    match predicate {
        Expr::Arrow(ArrowExpr {
            params,
            body,
            is_async: false,
            is_generator: false,
            ..
        }) if params.len() == 1 => {
            if let BlockStmtOrExpr::Expr(expr) = &**body {
                if let Some(inlined) =
                    inline_simple_lambda_body(&params[0], input_expr.clone(), (**expr).clone())
                {
                    return inlined;
                }
            }

            call_handler(Box::new(predicate.clone()), vec![input_expr])
        }
        _ => call_handler(Box::new(predicate.clone()), vec![input_expr]),
    }
}

fn can_inline_input(expr: &Expr) -> bool {
    matches!(expr, Expr::Ident(_) | Expr::Lit(_))
}

fn prop_name_eq(left: &PropName, right: &PropName) -> bool {
    match (left, right) {
        (PropName::Ident(left), PropName::Ident(right)) => left.sym == right.sym,
        (PropName::Str(left), PropName::Str(right)) => left.value == right.value,
        (PropName::Num(left), PropName::Num(right)) => left.value == right.value,
        (PropName::BigInt(left), PropName::BigInt(right)) => left.value == right.value,
        (PropName::Computed(_), PropName::Computed(_)) => false,
        _ => false,
    }
}

fn is_switchable_arm(arm: &MatchArm) -> bool {
    arm.guard.is_none()
        && arm
            .patterns
            .iter()
            .all(|pattern| is_switch_literal(pattern))
}

fn is_switch_literal(expr: &Expr) -> bool {
    // switch uses ===, whereas ts-pattern distinguishes +0 from -0.
    if is_zero_literal(expr) {
        return false;
    }
    matches!(
        expr,
        Expr::Lit(Lit::Str(_))
            | Expr::Lit(Lit::Num(_))
            | Expr::Lit(Lit::Bool(_))
            | Expr::Lit(Lit::BigInt(_))
            | Expr::Lit(Lit::Null(_))
    ) || matches!(expr, Expr::Unary(unary) if unary.op == UnaryOp::Minus && is_number_literal(&unary.arg))
}

fn is_number_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::Lit(Lit::Num(_)))
}

fn is_zero_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Lit(Lit::Num(number)) => number.value == 0.0,
        Expr::Unary(unary) if unary.op == UnaryOp::Minus => {
            matches!(&*unary.arg, Expr::Lit(Lit::Num(number)) if number.value == 0.0)
        }
        _ => false,
    }
}

fn is_function_expr(expr: &Expr) -> bool {
    matches!(expr, Expr::Arrow(_) | Expr::Fn(_))
}

fn as_call(expr: &Expr) -> Option<&CallExpr> {
    match expr {
        Expr::Call(call) => Some(call),
        _ => None,
    }
}

fn as_method_call(call: &CallExpr) -> Option<(String, &Expr)> {
    let Callee::Expr(callee) = &call.callee else {
        return None;
    };
    let Expr::Member(member) = &**callee else {
        return None;
    };

    Some((member_name(member)?.to_string(), &member.obj))
}

fn member_name(member: &MemberExpr) -> Option<&str> {
    match &member.prop {
        MemberProp::Ident(ident) => Some(ident.sym.as_ref()),
        MemberProp::PrivateName(_) => None,
        MemberProp::Computed(_) => None,
    }
}

fn prop_access(value: Expr, key: &PropName) -> Option<Expr> {
    match key {
        PropName::Ident(ident) => Some(member_expr(value, ident.sym.as_ref())),
        PropName::Str(str) => Some(computed_member_expr(
            value,
            Expr::Lit(Lit::Str(str.clone())),
        )),
        PropName::Num(num) => Some(computed_member_expr(
            value,
            Expr::Lit(Lit::Num(num.clone())),
        )),
        PropName::BigInt(bigint) => Some(computed_member_expr(
            value,
            Expr::Lit(Lit::BigInt(bigint.clone())),
        )),
        PropName::Computed(computed) => Some(computed_member_expr(value, (*computed.expr).clone())),
    }
}

fn prop_access_path(value: Expr, path: &[PropName]) -> Option<Expr> {
    let mut current = value;
    for segment in path {
        current = prop_access(current, segment)?;
    }
    Some(current)
}

fn object_path_base_test(input: Expr, path: &[PropName], known_input_object: bool) -> Option<Expr> {
    let mut test = bool_lit(true);
    let mut current = input;

    for segment in path {
        let object_test = if known_input_object && matches!(current, Expr::Ident(_)) {
            bool_lit(true)
        } else {
            and(
                strict_ne(current.clone(), null_lit()),
                typeof_eq(current.clone(), "object"),
            )
        };
        test = and(
            test,
            and(object_test, prop_in_object(segment, current.clone())?),
        );
        current = prop_access(current, segment)?;
    }

    Some(test)
}

fn common_object_prefix(arms: &[MatchArm]) -> Option<(PropName, Vec<MatchArm>)> {
    if arms.is_empty() || arms.iter().any(|arm| arm.patterns.len() != 1) {
        return None;
    }

    let (prefix, _) = single_object_prop_with_object_value(&arms[0].patterns[0])?;
    let mut stripped = Vec::new();

    for arm in arms {
        let (key, value) = single_object_prop_with_object_value(&arm.patterns[0])?;
        if !prop_name_eq(&prefix, &key) {
            return None;
        }

        stripped.push(MatchArm {
            patterns: vec![value.clone()],
            handler_patterns: arm.handler_patterns.clone(),
            guard: arm.guard.clone(),
            handler: arm.handler.clone(),
        });
    }

    Some((prefix, stripped))
}

fn single_object_prop_with_object_value(pattern: &Expr) -> Option<(PropName, &Box<Expr>)> {
    let Expr::Object(object) = pattern else {
        return None;
    };
    if object.props.len() != 1 {
        return None;
    }
    let PropOrSpread::Prop(prop) = &object.props[0] else {
        return None;
    };
    let Prop::KeyValue(key_value) = &**prop else {
        return None;
    };
    matches!(&*key_value.value, Expr::Object(_))
        .then_some((key_value.key.clone(), &key_value.value))
}

fn private_ident_for_prop(prefix: &str, prop: &PropName) -> Ident {
    let name = match prop {
        PropName::Ident(ident) => ident.sym.to_string(),
        PropName::Str(string) => string.value.to_string_lossy().to_string(),
        _ => "value".to_string(),
    };
    let sanitized: String = name
        .chars()
        .filter(|char| char.is_ascii_alphanumeric() || *char == '_')
        .collect();
    let mut chars = sanitized.chars();
    let suffix = match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => "Value".to_string(),
    };

    Ident::new(
        format!("{prefix}{suffix}").into(),
        DUMMY_SP,
        Default::default(),
    )
}

fn common_switch_path(arms: &[MatchArm]) -> Option<Vec<PropName>> {
    if arms.is_empty()
        || arms
            .iter()
            .any(|arm| arm.guard.is_some() || arm.patterns.len() != 1)
    {
        return None;
    }

    let mut paths = Vec::new();
    collect_literal_paths(&arms[0].patterns[0], Vec::new(), &mut paths);
    paths.sort_by_key(|path| path.len());

    paths.into_iter().find(|path| {
        let mut values = arms
            .iter()
            .filter_map(|arm| literal_at_path(&arm.patterns[0], path));
        let Some(first) = values.next() else {
            return false;
        };
        values.any(|value| !switch_literals_eq(first, value))
            && arms
                .iter()
                .all(|arm| literal_at_path(&arm.patterns[0], path).is_some())
    })
}

fn collect_literal_paths(pattern: &Expr, prefix: Vec<PropName>, paths: &mut Vec<Vec<PropName>>) {
    let Expr::Object(object) = pattern else {
        return;
    };

    for prop in &object.props {
        let PropOrSpread::Prop(prop) = prop else {
            continue;
        };
        let Prop::KeyValue(key_value) = &**prop else {
            continue;
        };

        let mut next = prefix.clone();
        next.push(key_value.key.clone());

        if is_switch_literal(&key_value.value) {
            paths.push(next);
            continue;
        }

        collect_literal_paths(&key_value.value, next, paths);
    }
}

fn literal_at_path<'a>(pattern: &'a Expr, path: &[PropName]) -> Option<&'a Expr> {
    let mut current = pattern;

    for (index, segment) in path.iter().enumerate() {
        let Expr::Object(object) = current else {
            return None;
        };
        let key_value = object.props.iter().find_map(|prop| {
            let PropOrSpread::Prop(prop) = prop else {
                return None;
            };
            let Prop::KeyValue(key_value) = &**prop else {
                return None;
            };
            prop_name_eq(&key_value.key, segment).then_some(key_value)
        })?;

        if index + 1 == path.len() {
            return is_switch_literal(&key_value.value).then_some(&*key_value.value);
        }

        current = &key_value.value;
    }

    None
}

fn strip_group_arms(
    arms: Vec<MatchArm>,
    path: &[PropName],
    inherited_fallback: Fallback,
) -> Option<(Vec<MatchArm>, Fallback)> {
    let mut stripped = Vec::new();
    let mut fallback = inherited_fallback;

    for arm in arms {
        let pattern = arm.patterns.into_iter().next()?;
        match strip_literal_path(&pattern, path)? {
            Some(pattern) => stripped.push(MatchArm {
                patterns: vec![Box::new(pattern)],
                handler_patterns: arm.handler_patterns,
                guard: arm.guard,
                handler: arm.handler,
            }),
            None => {
                fallback = Fallback::Matched(arm.handler);
                break;
            }
        }
    }

    Some((stripped, fallback))
}

fn strip_literal_path(pattern: &Expr, path: &[PropName]) -> Option<Option<Expr>> {
    let Expr::Object(object) = pattern else {
        return None;
    };

    let mut props = Vec::new();

    for prop in &object.props {
        let PropOrSpread::Prop(prop) = prop else {
            props.push(prop.clone());
            continue;
        };
        let Prop::KeyValue(key_value) = &**prop else {
            props.push(PropOrSpread::Prop(prop.clone()));
            continue;
        };

        if !prop_name_eq(&key_value.key, &path[0]) {
            props.push(PropOrSpread::Prop(prop.clone()));
            continue;
        }

        if path.len() == 1 {
            continue;
        }

        if let Some(stripped) = strip_literal_path(&key_value.value, &path[1..])? {
            props.push(PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                key: key_value.key.clone(),
                value: Box::new(stripped),
            }))));
        }
    }

    (!props.is_empty())
        .then_some(Some(Expr::Object(ObjectLit {
            span: object.span,
            props,
        })))
        .or(Some(None))
}

fn group_arms_by_path(
    arms: Vec<MatchArm>,
    path: &[PropName],
) -> Option<Vec<(Expr, Vec<MatchArm>)>> {
    let mut groups: Vec<(Expr, Vec<MatchArm>)> = Vec::new();

    for arm in arms {
        let value = literal_at_path(&arm.patterns[0], path)?.clone();
        if let Some((_, grouped_arms)) = groups
            .iter_mut()
            .find(|(group_value, _)| switch_literals_eq(group_value, &value))
        {
            grouped_arms.push(arm);
        } else {
            groups.push((value, vec![arm]));
        }
    }

    Some(groups)
}

fn switch_literals_eq(left: &Expr, right: &Expr) -> bool {
    match (left, right) {
        (Expr::Lit(Lit::Str(left)), Expr::Lit(Lit::Str(right))) => left.value == right.value,
        (Expr::Lit(Lit::Num(left)), Expr::Lit(Lit::Num(right))) => left.value == right.value,
        (Expr::Lit(Lit::Bool(left)), Expr::Lit(Lit::Bool(right))) => left.value == right.value,
        (Expr::Lit(Lit::BigInt(left)), Expr::Lit(Lit::BigInt(right))) => left.value == right.value,
        (Expr::Lit(Lit::Null(_)), Expr::Lit(Lit::Null(_))) => true,
        (
            Expr::Unary(UnaryExpr {
                op: UnaryOp::Minus,
                arg: left,
                ..
            }),
            Expr::Unary(UnaryExpr {
                op: UnaryOp::Minus,
                arg: right,
                ..
            }),
        ) => switch_literals_eq(left, right),
        _ => false,
    }
}

fn const_stmt(ident: Ident, init: Expr) -> Stmt {
    Stmt::Decl(Decl::Var(Box::new(VarDecl {
        span: DUMMY_SP,
        ctxt: Default::default(),
        kind: VarDeclKind::Const,
        declare: false,
        decls: vec![VarDeclarator {
            span: DUMMY_SP,
            name: Pat::Ident(ident.into()),
            init: Some(Box::new(init)),
            definite: false,
        }],
    })))
}

fn fallback_stmts(
    fallback: Fallback,
    input_expr: Expr,
    exhaustive_error_callee: Option<Expr>,
) -> Vec<Stmt> {
    match fallback {
        Fallback::Otherwise(handler) => {
            vec![return_stmt(handler_result(handler, vec![input_expr]))]
        }
        Fallback::Matched(handler) => vec![return_stmt(handler_result(
            handler,
            vec![input_expr.clone(), input_expr],
        ))],
        Fallback::Exhaustive => vec![throw_exhaustive_stmt(
            exhaustive_error_callee.expect("exhaustive error callee"),
            input_expr,
        )],
    }
}

fn throw_exhaustive_stmt(callee: Expr, input_expr: Expr) -> Stmt {
    Stmt::Throw(ThrowStmt {
        span: DUMMY_SP,
        arg: Box::new(Expr::New(NewExpr {
            span: DUMMY_SP,
            ctxt: Default::default(),
            callee: Box::new(callee),
            args: Some(vec![ExprOrSpread {
                spread: None,
                expr: Box::new(input_expr),
            }]),
            type_args: None,
        })),
    })
}

fn return_stmt(expr: Expr) -> Stmt {
    Stmt::Return(ReturnStmt {
        span: DUMMY_SP,
        arg: Some(Box::new(expr)),
    })
}

fn assign_stmt(target: Expr, expr: Expr) -> Stmt {
    Stmt::Expr(ExprStmt {
        span: DUMMY_SP,
        expr: Box::new(Expr::Assign(AssignExpr {
            span: DUMMY_SP,
            op: AssignOp::Assign,
            left: AssignTarget::try_from(Box::new(target)).expect("assign target"),
            right: Box::new(expr),
        })),
    })
}

fn fallback_stmt(
    target: Expr,
    fallback: Fallback,
    input_expr: Expr,
    exhaustive_error_callee: &mut impl FnMut() -> Expr,
) -> Stmt {
    match fallback {
        Fallback::Otherwise(handler) => {
            assign_stmt(target, handler_result(handler, vec![input_expr]))
        }
        Fallback::Matched(handler) => assign_stmt(
            target,
            handler_result(handler, vec![input_expr.clone(), input_expr]),
        ),
        Fallback::Exhaustive => throw_exhaustive_stmt(exhaustive_error_callee(), input_expr),
    }
}

fn break_stmt() -> Stmt {
    Stmt::Break(BreakStmt {
        span: DUMMY_SP,
        label: None,
    })
}

fn var_without_init(kind: VarDeclKind, ident: Ident) -> Stmt {
    Stmt::Decl(Decl::Var(Box::new(VarDecl {
        span: DUMMY_SP,
        ctxt: Default::default(),
        kind: if kind == VarDeclKind::Var {
            VarDeclKind::Var
        } else {
            VarDeclKind::Let
        },
        declare: false,
        decls: vec![VarDeclarator {
            span: DUMMY_SP,
            name: Pat::Ident(ident.into()),
            init: None,
            definite: false,
        }],
    })))
}

fn handler_result(handler: Box<Expr>, args: Vec<Expr>) -> Expr {
    if let Expr::Arrow(ArrowExpr {
        params,
        body,
        is_async: false,
        is_generator: false,
        ..
    }) = &*handler
    {
        if let BlockStmtOrExpr::Expr(expr) = &**body {
            if params.is_empty() {
                return *expr.clone();
            }
            if params.len() == 1 {
                if let Some(inlined) =
                    inline_simple_lambda_body(&params[0], args[0].clone(), *expr.clone())
                {
                    return inlined;
                }
            }
        }
    }
    call_handler(handler, args)
}

fn inline_simple_lambda_body(param: &Pat, input_expr: Expr, mut body: Expr) -> Option<Expr> {
    let mut bindings = Vec::new();
    collect_pat_bindings(param, input_expr, &mut bindings)?;
    if !is_simple_inline_body(&body, &bindings) {
        return None;
    }
    body.visit_mut_with(&mut InlineBindings { bindings });
    Some(body)
}

fn collect_pat_bindings(param: &Pat, value: Expr, bindings: &mut Vec<(Id, Expr)>) -> Option<()> {
    match param {
        Pat::Ident(ident) => {
            bindings.push((ident.id.to_id(), value));
            Some(())
        }
        Pat::Object(object) => object.props.iter().try_for_each(|prop| match prop {
            ObjectPatProp::Assign(assign) if assign.value.is_none() => {
                bindings.push((
                    assign.key.id.to_id(),
                    member_expr(value.clone(), assign.key.id.sym.as_ref()),
                ));
                Some(())
            }
            ObjectPatProp::KeyValue(key_value) => collect_pat_bindings(
                &key_value.value,
                prop_access(value.clone(), &key_value.key)?,
                bindings,
            ),
            ObjectPatProp::Assign(_) | ObjectPatProp::Rest(_) => None,
        }),
        Pat::Array(array) => array
            .elems
            .iter()
            .enumerate()
            .try_for_each(|(index, elem)| {
                if let Some(elem) = elem {
                    collect_pat_bindings(
                        elem,
                        computed_member_expr(
                            value.clone(),
                            Expr::Lit(Lit::Num(Number {
                                span: DUMMY_SP,
                                value: index as f64,
                                raw: None,
                            })),
                        ),
                        bindings,
                    )
                } else {
                    Some(())
                }
            }),
        Pat::Assign(_) | Pat::Rest(_) | Pat::Invalid(_) | Pat::Expr(_) => None,
    }
}

fn is_simple_inline_body(body: &Expr, bindings: &[(Id, Expr)]) -> bool {
    let mut usage = BindingUsage {
        bindings: bindings.iter().map(|(id, _)| id.clone()).collect(),
        counts: vec![0; bindings.len()],
        has_mutation: false,
    };
    let mut body = body.clone();
    body.visit_mut_with(&mut usage);
    !usage.has_mutation && usage.counts.iter().all(|count| *count == 1)
}

struct BindingUsage {
    bindings: Vec<Id>,
    counts: Vec<usize>,
    has_mutation: bool,
}

impl VisitMut for BindingUsage {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        match expr {
            Expr::Ident(ident) => {
                if let Some(index) = self.bindings.iter().position(|id| *id == ident.to_id()) {
                    self.counts[index] += 1;
                }
            }
            Expr::Assign(_) | Expr::Update(_) => {
                self.has_mutation = true;
            }
            Expr::Arrow(_) | Expr::Fn(_) | Expr::Class(_) => return,
            _ => {}
        }

        expr.visit_mut_children_with(self);
    }
}

struct InlineBindings {
    bindings: Vec<(Id, Expr)>,
}

impl VisitMut for InlineBindings {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        if let Expr::Ident(ident) = expr {
            if let Some((_, replacement)) =
                self.bindings.iter().find(|(id, _)| *id == ident.to_id())
            {
                *expr = replacement.clone();
                return;
            }
        }

        match expr {
            Expr::Arrow(_) | Expr::Fn(_) | Expr::Class(_) => {}
            _ => expr.visit_mut_children_with(self),
        }
    }
}

fn call_handler(handler: Box<Expr>, args: Vec<Expr>) -> Expr {
    // ts-pattern receives a callback value, not a member reference. Parentheses
    // alone preserve a member's receiver; a comma expression removes it.
    let callee = if is_function_expr(&handler) {
        *handler
    } else {
        Expr::Seq(SeqExpr {
            span: DUMMY_SP,
            exprs: vec![Box::new(bool_lit(false)), handler],
        })
    };
    call_expr(paren_expr(callee), args)
}

fn call_expr(callee: Expr, args: Vec<Expr>) -> Expr {
    Expr::Call(CallExpr {
        span: DUMMY_SP,
        ctxt: Default::default(),
        callee: Callee::Expr(Box::new(callee)),
        args: args
            .into_iter()
            .map(|expr| ExprOrSpread {
                spread: None,
                expr: Box::new(expr),
            })
            .collect(),
        type_args: None,
    })
}

fn call_member(member: Expr, args: Vec<Expr>) -> Expr {
    call_expr(member, args)
}

fn ident_expr(ident: &Ident) -> Expr {
    Expr::Ident(ident.clone())
}

fn member_expr(obj: Expr, prop: &str) -> Expr {
    Expr::Member(MemberExpr {
        span: DUMMY_SP,
        obj: Box::new(obj),
        prop: MemberProp::Ident(IdentName::new(prop.into(), DUMMY_SP)),
    })
}

fn computed_member_expr(obj: Expr, prop: Expr) -> Expr {
    Expr::Member(MemberExpr {
        span: DUMMY_SP,
        obj: Box::new(obj),
        prop: MemberProp::Computed(ComputedPropName {
            span: DUMMY_SP,
            expr: Box::new(prop),
        }),
    })
}

fn prop_in_object(key: &PropName, obj: Expr) -> Option<Expr> {
    Some(bin(BinaryOp::In, prop_key_expr(key)?, obj))
}

fn prop_key_expr(key: &PropName) -> Option<Expr> {
    match key {
        PropName::Ident(ident) => Some(Expr::Lit(Lit::Str(Str {
            span: DUMMY_SP,
            value: ident.sym.clone().into(),
            raw: None,
        }))),
        PropName::Str(str) => Some(Expr::Lit(Lit::Str(str.clone()))),
        PropName::Num(num) => Some(Expr::Lit(Lit::Num(num.clone()))),
        PropName::BigInt(bigint) => Some(Expr::Lit(Lit::BigInt(bigint.clone()))),
        PropName::Computed(computed) => Some((*computed.expr).clone()),
    }
}

fn strict_eq(left: Expr, right: Expr) -> Expr {
    bin(BinaryOp::EqEqEq, left, right)
}

fn strict_ne(left: Expr, right: Expr) -> Expr {
    bin(BinaryOp::NotEqEq, left, right)
}

fn and(left: Expr, right: Expr) -> Expr {
    match (as_bool_lit(&left), as_bool_lit(&right)) {
        (Some(true), _) => right,
        (_, Some(true)) => left,
        (Some(false), _) => bool_lit(false),
        (_, Some(false)) => bool_lit(false),
        _ => bin(BinaryOp::LogicalAnd, left, right),
    }
}

fn or(left: Expr, right: Expr) -> Expr {
    match (as_bool_lit(&left), as_bool_lit(&right)) {
        (Some(true), _) => bool_lit(true),
        (_, Some(true)) => bool_lit(true),
        (Some(false), _) => right,
        (_, Some(false)) => left,
        _ => bin(BinaryOp::LogicalOr, left, right),
    }
}

fn cond_expr(test: Expr, consequent: Expr, alternate: Expr) -> Expr {
    Expr::Cond(CondExpr {
        span: DUMMY_SP,
        test: Box::new(test),
        cons: Box::new(consequent),
        alt: Box::new(alternate),
    })
}

fn bin(op: BinaryOp, left: Expr, right: Expr) -> Expr {
    Expr::Bin(BinExpr {
        span: DUMMY_SP,
        op,
        left: Box::new(left),
        right: Box::new(right),
    })
}

fn unary(op: UnaryOp, arg: Expr) -> Expr {
    Expr::Unary(UnaryExpr {
        span: DUMMY_SP,
        op,
        arg: Box::new(arg),
    })
}

fn paren_expr(expr: Expr) -> Expr {
    Expr::Paren(ParenExpr {
        span: DUMMY_SP,
        expr: Box::new(expr),
    })
}

fn bool_lit(value: bool) -> Expr {
    Expr::Lit(Lit::Bool(Bool {
        span: DUMMY_SP,
        value,
    }))
}

fn as_bool_lit(expr: &Expr) -> Option<bool> {
    match expr {
        Expr::Lit(Lit::Bool(Bool { value, .. })) => Some(*value),
        _ => None,
    }
}

fn null_lit() -> Expr {
    Expr::Lit(Lit::Null(Null { span: DUMMY_SP }))
}

fn undefined_expr() -> Expr {
    Expr::Ident(quote_ident!("undefined").into())
}

fn typeof_eq(value: Expr, type_name: &str) -> Expr {
    strict_eq(
        unary(UnaryOp::TypeOf, value),
        Expr::Lit(Lit::Str(Str {
            span: DUMMY_SP,
            value: type_name.into(),
            raw: None,
        })),
    )
}

fn array_is_array(value: Expr) -> Expr {
    call_expr(
        member_expr(Expr::Ident(quote_ident!("Array").into()), "isArray"),
        vec![value],
    )
}

fn array_from(value: Expr) -> Expr {
    call_expr(
        member_expr(Expr::Ident(quote_ident!("Array").into()), "from"),
        vec![value],
    )
}

fn reflect_own_keys(value: Expr) -> Expr {
    call_expr(
        member_expr(Expr::Ident(quote_ident!("Reflect").into()), "ownKeys"),
        vec![value],
    )
}

fn record_entries(value: Expr) -> Expr {
    let key = private_ident!("_tsPatternRecordKey");
    call_member(
        member_expr(reflect_own_keys(value.clone()), "map"),
        vec![Expr::Arrow(ArrowExpr {
            span: DUMMY_SP,
            ctxt: Default::default(),
            params: vec![Pat::Ident(key.clone().into())],
            body: Box::new(BlockStmtOrExpr::Expr(Box::new(Expr::Array(ArrayLit {
                span: DUMMY_SP,
                elems: vec![
                    Some(ExprOrSpread {
                        spread: None,
                        expr: Box::new(ident_expr(&key)),
                    }),
                    Some(ExprOrSpread {
                        spread: None,
                        expr: Box::new(computed_member_expr(value, ident_expr(&key))),
                    }),
                ],
            })))),
            is_async: false,
            is_generator: false,
            type_params: None,
            return_type: None,
        })],
    )
}

fn record_values(value: Expr) -> Expr {
    let key = private_ident!("_tsPatternRecordKey");
    call_member(
        member_expr(reflect_own_keys(value.clone()), "map"),
        vec![Expr::Arrow(ArrowExpr {
            span: DUMMY_SP,
            ctxt: Default::default(),
            params: vec![Pat::Ident(key.clone().into())],
            body: Box::new(BlockStmtOrExpr::Expr(Box::new(computed_member_expr(
                value,
                ident_expr(&key),
            )))),
            is_async: false,
            is_generator: false,
            type_params: None,
            return_type: None,
        })],
    )
}

fn record_aggregate_selections(
    value: Expr,
    key_ident: &Ident,
    key_selections: Vec<SelectionBinding>,
    value_ident: &Ident,
    value_selections: Vec<SelectionBinding>,
) -> Option<Vec<SelectionBinding>> {
    let mut aggregated = Vec::new();

    for selection in key_selections {
        if !matches!(selection.value, Expr::Ident(ref ident) if ident.to_id() == key_ident.to_id())
        {
            return None;
        }
        aggregated.push(SelectionBinding {
            name: selection.name,
            value: reflect_own_keys(value.clone()),
        });
    }

    for selection in value_selections {
        if !matches!(selection.value, Expr::Ident(ref ident) if ident.to_id() == value_ident.to_id())
        {
            return None;
        }
        aggregated.push(SelectionBinding {
            name: selection.name,
            value: record_values(value.clone()),
        });
    }

    let anonymous_count = aggregated
        .iter()
        .filter(|selection| selection.name.is_none())
        .count();
    (anonymous_count <= 1).then_some(aggregated)
}

fn instance_of(value: Expr, constructor: &str) -> Expr {
    Expr::Bin(BinExpr {
        span: DUMMY_SP,
        op: BinaryOp::InstanceOf,
        left: Box::new(value),
        right: Box::new(Expr::Ident(Ident::new(
            constructor.into(),
            DUMMY_SP,
            Default::default(),
        ))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use swc_core::{
        common::{FileName, Globals, SourceMap, GLOBALS},
        ecma::{
            codegen::{text_writer::JsWriter, Emitter},
            parser::{lexer::Lexer, Parser, StringInput, Syntax, TsSyntax},
            visit::VisitMutWith,
        },
    };

    fn transform(source: &str) -> String {
        GLOBALS.set(&Globals::new(), || {
            let cm: std::sync::Arc<SourceMap> = Default::default();
            let file = cm.new_source_file(FileName::Anon.into(), source.to_string());
            let lexer = Lexer::new(
                Syntax::Typescript(TsSyntax {
                    tsx: false,
                    decorators: false,
                    dts: false,
                    no_early_errors: false,
                    disallow_ambiguous_jsx_like: false,
                }),
                Default::default(),
                StringInput::from(&*file),
                None,
            );
            let mut parser = Parser::new_from(lexer);
            let mut module = parser.parse_module().unwrap();
            module.visit_mut_with(&mut TsPatternTransformer::default());

            let mut output = Vec::new();
            {
                let mut emitter = Emitter {
                    cfg: Default::default(),
                    cm: cm.clone(),
                    comments: None,
                    wr: JsWriter::new(cm, "\n", &mut output, None),
                };
                emitter.emit_module(&module).unwrap();
            }

            String::from_utf8(output).unwrap()
        })
    }

    #[test]
    fn emits_ternary_for_literal_match_expression() {
        let output = transform(
            r#"import { match } from "ts-pattern";
const result = match(input).with("a", () => 1).with("b", () => 2).otherwise(() => 0);"#,
        );

        assert!(output.contains("input === \"a\" ? 1"), "{output}");
        assert!(output.contains(": input === \"b\" ? 2 : 0"), "{output}");
        assert!(!output.contains("switch"), "{output}");
        assert!(!output.contains("(()=>"), "{output}");
    }

    #[test]
    fn emits_switch_block_for_arrow_body_literal_match() {
        let output = transform(
            r#"import { match } from "ts-pattern";
export const run = (command: "start" | "stop" | "pause" | "unknown") => match(command).with("start", () => "running").with("stop", "pause", () => "halted").otherwise(() => "ignored");"#,
        );

        assert!(output.contains("=>{"), "{output}");
        assert!(output.contains("switch"), "{output}");
        assert!(!output.contains("=> (()=>"), "{output}");
    }

    #[test]
    fn emits_switch_block_for_arrow_body_object_discriminant_match() {
        let output = transform(
            r#"import { match } from "ts-pattern";
export const run = (content: { type: "text" } | { type: "img" } | { type: "video" }) => match(content).with({ type: "text" }, () => "<p>...</p>").with({ type: "img" }, () => "<img ... />").with({ type: "video" }, () => "<video ... />").exhaustive();"#,
        );

        assert!(output.contains("=>{"), "{output}");
        assert!(output.contains("switch(_tsPatternInput.type)"), "{output}");
        assert!(output.contains("case \"text\""), "{output}");
        assert!(!output.contains("=>()=>"), "{output}");
    }

    #[test]
    fn emits_if_block_for_arrow_body_exhaustive_match() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
export const run = (result: { type: "error" } | { type: "ok"; data: { type: "img"; src: string } } | { type: "ok"; data: { type: "text"; content: string } }) => match(result).with({ type: "error" }, () => "error").with({ type: "ok", data: { type: "text" } }, ({ data }) => data.content).with({ type: "ok", data: { type: "img", src: P.select() } }, (src) => src).exhaustive();"#,
        );

        assert!(output.contains("=>{"), "{output}");
        assert!(output.contains("throw new NonExhaustiveError"), "{output}");
        assert!(!output.contains("match(result).with"), "{output}");
        assert!(!output.contains("=>()=>"), "{output}");
    }

    #[test]
    fn emits_nested_switch_for_shared_object_discriminants() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
export const run = (result: { type: "error" } | { type: "ok"; data: { type: "img"; src: string } } | { type: "ok"; data: { type: "text"; content: string } }) => match(result).with({ type: "error" }, () => "error").with({ type: "ok", data: { type: "text" } }, ({ data }) => data.content).with({ type: "ok", data: { type: "img", src: P.select() } }, (src) => src).exhaustive();"#,
        );

        assert!(output.contains("switch(_tsPatternInput.type)"), "{output}");
        assert!(
            output.contains("const _tsPatternData = _tsPatternInput.data"),
            "{output}"
        );
        assert!(output.contains("switch(_tsPatternData.type)"), "{output}");
        assert!(output.contains("return _tsPatternData.src"), "{output}");
        assert!(!output.contains("match(result).with"), "{output}");
    }

    #[test]
    fn rewrites_exhaustive_match_variable_initializer_in_block_body() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
export const render = (result: { type: "error" } | { type: "ok"; data: { type: "img"; src: string } } | { type: "ok"; data: { type: "text"; content: string } }) => {
  const html = match(result).with({ type: "error" }, () => "error").with({ type: "ok", data: { type: "text" } }, ({ data }) => data.content).with({ type: "ok", data: { type: "img", src: P.select() } }, (src) => src).exhaustive();
  return html;
};"#,
        );

        assert!(
            output.contains("const html = _tsPatternResult;"),
            "{output}"
        );
        assert!(output.contains("switch(result.type)"), "{output}");
        assert!(
            output.contains("const _tsPatternData = result.data"),
            "{output}"
        );
        assert!(output.contains("switch(_tsPatternData.type)"), "{output}");
        assert!(!output.contains("match(result).with"), "{output}");
    }

    #[test]
    fn caches_nested_object_discriminants_without_repeating_parent_guards() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
export const render = (result: { type: "error" } | { type: "ok"; data: { type: "img"; src: string } } | { type: "ok"; data: { type: "text"; content: string } }) => {
  const html = match(result).with({ type: "error" }, () => "error").with({ type: "ok", data: { type: "text" } }, ({ data }) => data.content).with({ type: "ok", data: { type: "img", src: P.select() } }, (src) => src).exhaustive();
  return html;
};"#,
        );

        assert!(
            output.contains("const _tsPatternData = result.data"),
            "{output}"
        );
        assert!(output.contains("switch(_tsPatternData.type)"), "{output}");
        assert!(
            output.contains("_tsPatternResult = result.data.content"),
            "{output}"
        );
        assert!(
            output.contains("_tsPatternResult = _tsPatternData.src"),
            "{output}"
        );
        assert!(!output.contains("result !== null && typeof result === \"object\" && \"data\" in result && result.data !== null"), "{output}");
    }

    #[test]
    fn emits_ternary_for_object_pattern() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with({ type: "ok", value: P.number }, (value) => value.value).otherwise(() => 0);"#,
        );

        assert!(output.contains("?"), "{output}");
        assert!(output.contains(":"), "{output}");
        assert!(output.contains("typeof"), "{output}");
        assert!(output.contains("value === \"number\""), "{output}");
        assert!(!output.contains("(()=>"), "{output}");
    }

    #[test]
    fn emits_ternary_for_object_discriminant_expression() {
        let output = transform(
            r#"import { match } from "ts-pattern";
const result = match(input).with({ type: "text" }, () => "text").with({ type: "img" }, () => "image").otherwise(() => "other");"#,
        );

        assert!(
            output.contains("input.type === \"text\" ? \"text\""),
            "{output}"
        );
        assert!(
            output.contains("input.type === \"img\" ? \"image\" : \"other\""),
            "{output}"
        );
        assert!(!output.contains("switch"), "{output}");
        assert!(!output.contains("(()=>"), "{output}");
    }

    #[test]
    fn leaves_effectful_input_unchanged_to_avoid_iife() {
        let output = transform(
            r#"import { match } from "ts-pattern";
const result = match(next()).with("a", () => 1).otherwise(() => 0);"#,
        );

        assert!(output.contains("match(next()).with"), "{output}");
        assert!(!output.contains("(()=>{"), "{output}");
    }

    #[test]
    fn inlines_safe_input_ternary() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(value).with(P.union(P.string, P.number), () => "scalar").with(P.boolean, () => "flag").otherwise(() => "empty");"#,
        );

        assert!(!output.contains("_tsPatternInput"), "{output}");
        assert!(!output.contains("(() => \"scalar\")("), "{output}");
        assert!(output.contains("typeof value === \"string\""), "{output}");
        assert!(output.contains("? \"scalar\""), "{output}");
    }

    #[test]
    fn inlines_simple_destructured_guard() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(item).with({ count: P.number }, ({ count }) => count > 5, () => "many").with({ count: P.number }, () => "some").otherwise(() => "none");"#,
        );

        assert!(output.contains("item.count > 5"), "{output}");
        assert!(!output.contains("count })=>count > 5)(item)"), "{output}");
    }

    #[test]
    fn inlines_simple_p_when_predicate() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with({ score: P.when((score) => score < 5) }, () => "low").otherwise(() => "ok");"#,
        );

        assert!(output.contains("input.score < 5"), "{output}");
        assert!(!output.contains("=>score < 5"), "{output}");
        assert!(!output.contains(")(input.score)"), "{output}");
    }

    #[test]
    fn inlines_simple_p_when_type_predicate() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with({ score: P.when((score): score is 5 => score === 5) }, () => "five").otherwise(() => "other");"#,
        );

        assert!(output.contains("input.score === 5"), "{output}");
        assert!(!output.contains("=>score === 5"), "{output}");
        assert!(!output.contains(")(input.score)"), "{output}");
    }

    #[test]
    fn keeps_complex_p_when_predicate_call() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with(P.when((value) => value > 0 && value < 10), () => "ok").otherwise(() => "other");"#,
        );

        assert!(output.contains("=>value > 0 && value < 10"), "{output}");
        assert!(output.contains(")(input)"), "{output}");
    }

    #[test]
    fn inlines_simple_destructured_handler() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with({ type: "ok", value: P.number }, ({ value }) => `ok:${value}`).otherwise(() => "idle");"#,
        );

        assert!(output.contains("`ok:${input.value}`"), "{output}");
        assert!(!output.contains("=> `ok:"), "{output}");
    }

    #[test]
    fn requires_object_key_for_wildcard_property() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with({ storeId: P._ }, () => "store").with({ teamId: P._ }, () => "team").otherwise(() => "none");"#,
        );

        assert!(output.contains("\"storeId\" in input"), "{output}");
        assert!(output.contains("\"teamId\" in input"), "{output}");
    }

    #[test]
    fn requires_object_key_for_undefined_property() {
        let output = transform(
            r#"import { match } from "ts-pattern";
const result = match(input).with({ type: undefined }, () => true).otherwise(() => false);"#,
        );

        assert!(output.contains("\"type\" in input"), "{output}");
        assert!(output.contains("input.type === undefined"), "{output}");
    }

    #[test]
    fn leaves_exhaustive_expression_unchanged_to_avoid_iife() {
        let output = transform(
            r#"import { match } from "ts-pattern";
const result = match(input).with("ok", () => true).exhaustive();"#,
        );

        assert!(output.contains("match(input).with"), "{output}");
        assert!(!output.contains("(()=>{"), "{output}");
    }

    #[test]
    fn supports_deno_npm_ts_pattern_imports() {
        let output = transform(
            r#"import { match } from "npm:ts-pattern";
const result = match(input).with("ok", () => true).otherwise(() => false);"#,
        );

        assert!(
            output.contains("input === \"ok\" ? true : false"),
            "{output}"
        );
        assert!(!output.contains("match(input).with"), "{output}");
    }

    #[test]
    fn supports_select_handlers() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with(P.select(), (value) => value).otherwise(() => 0);"#,
        );

        assert!(!output.contains("match(input).with"), "{output}");
        assert!(output.contains("? input : 0"), "{output}");
    }

    #[test]
    fn supports_nested_select_handlers() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with({ type: "ok", data: { type: "img", src: P.select() } }, (src) => `<img src="${src}" />`).otherwise(() => "?");"#,
        );

        assert!(!output.contains("match(input).with"), "{output}");
        assert!(output.contains("input.data.src"), "{output}");
        assert!(
            output.contains("<img src=\"${input.data.src}\" />"),
            "{output}"
        );
    }

    #[test]
    fn supports_pattern_alias_and_string_chain() {
        let output = transform(
            r#"import { match, Pattern } from "ts-pattern";
const result = match(input).with(Pattern.string.startsWith("TS"), () => true).otherwise(() => false);"#,
        );

        assert!(output.contains("input.startsWith(\"TS\")"), "{output}");
        assert!(!output.contains("match(input).with"), "{output}");
    }

    #[test]
    fn supports_number_chain_and_const_pattern_alias() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const pattern = ["a", P.union("a", "b")] as const;
const result = match(input).with(pattern, (value) => value[0]).with(P.number.gt(3), () => "n").otherwise(() => "x");"#,
        );

        assert!(output.contains("input[0] === \"a\""), "{output}");
        assert!(
            output.contains("input[1] === \"a\" || input[1] === \"b\""),
            "{output}"
        );
        assert!(
            output.contains("typeof input === \"number\" && input > 3"),
            "{output}"
        );
        assert!(!output.contains("match(input).with"), "{output}");
    }

    #[test]
    fn supports_record_select_aggregation() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with(P.record(P.string.select(), P.number), (keys) => keys.join(",")).otherwise(() => "");"#,
        );

        assert!(output.contains("Reflect.ownKeys(input).join"), "{output}");
        assert!(!output.contains("match(input).with"), "{output}");
    }

    #[test]
    fn keeps_unsupported_array_select_pattern_unchanged() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with(P.array(P.select()), (value) => value).otherwise(() => []);"#,
        );

        assert!(output.contains("match(input).with"), "{output}");
    }
}
