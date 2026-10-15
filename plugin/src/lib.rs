use std::collections::{HashMap, HashSet};

use swc_core::{
    common::{EqIgnoreSpan, SyntaxContext, DUMMY_SP},
    ecma::{
        ast::*,
        utils::{private_ident, quote_ident},
        visit::{Visit, VisitMut, VisitMutWith, VisitWith},
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
    is_matching_imports: HashSet<Id>,
    p_imports: HashSet<Id>,
    namespaces: HashSet<Id>,
    const_bindings: HashMap<Id, Expr>,
    mutated_bindings: HashSet<Id>,
    non_exhaustive_error: Option<Expr>,
    needs_non_exhaustive_error_import: bool,
}

struct MatchChain {
    input: Box<Expr>,
    arms: Vec<MatchArm>,
    fallback: Fallback,
}

#[derive(Clone)]
#[allow(clippy::vec_box)]
struct MatchArm {
    patterns: Vec<Box<Expr>>,
    guard: Option<Box<Expr>>,
    handler: Box<Expr>,
}

#[derive(Clone)]
enum Fallback {
    Otherwise(Box<Expr>),
    Matched(Box<Expr>),
    Exhaustive,
}

/// Where a statement match delivers its result.
#[derive(Clone)]
enum Sink {
    Return,
    Assign(Expr),
}

impl Sink {
    fn deliver(&self, value: Expr) -> Stmt {
        match self {
            Sink::Return => return_stmt(value),
            Sink::Assign(target) => assign_stmt(target.clone(), value),
        }
    }

    /// Tests branches in order, then runs the fallback.
    fn if_chain(&self, branches: Vec<(Expr, Stmt)>, fallback: Stmt) -> Vec<Stmt> {
        match self {
            // Returning branches need no `else`.
            Sink::Return => branches
                .into_iter()
                .map(|(test, cons)| if_stmt(test, cons, None))
                .chain([fallback])
                .collect(),
            Sink::Assign(_) => vec![branches
                .into_iter()
                .rev()
                .fold(fallback, |alt, (test, cons)| if_stmt(test, cons, Some(alt)))],
        }
    }

    fn end_case(&self, mut cons: Vec<Stmt>) -> Vec<Stmt> {
        if let Sink::Assign(_) = self {
            cons.push(break_stmt());
        }
        cons
    }
}

/// The value a match tests and the input its handlers receive. They differ
/// once a shared object prefix is cached.
struct MatchInputs {
    test: Expr,
    handler: Expr,
}

impl MatchInputs {
    fn new(input: Expr) -> Self {
        Self {
            test: input.clone(),
            handler: input,
        }
    }
}

/// A `P` API reference, after resolving const aliases.
#[derive(Clone, Copy)]
enum PApi<'a> {
    /// `P.name`
    Member(&'a str),
    /// `P.name(...)`
    Call(&'a str, &'a CallExpr),
    /// `P.base.method(...)`
    Chain(&'a str, &'a str, &'a CallExpr),
    /// `pattern.optional()`, `pattern.and(other)` or `pattern.or(other)` on a
    /// chainable pattern
    Modifier(&'a str, &'a Expr, &'a CallExpr),
}

#[derive(Clone, Default)]
struct SelectionBinding {
    name: Option<String>,
    value: Expr,
    capture_group: Option<Id>,
    projection: Option<Vec<String>>,
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
            let pending_import_before = self.needs_non_exhaustive_error_import;
            match self.compile_chain(chain) {
                Some(compiled) => *expr = compiled,
                None => self.needs_non_exhaustive_error_import = pending_import_before,
            }
        } else if let Some(compiled) = self.compile_is_matching(expr) {
            *expr = compiled;
        }
    }

    fn visit_mut_arrow_expr(&mut self, arrow: &mut ArrowExpr) {
        if let BlockStmtOrExpr::Expr(expr) = &*arrow.body {
            if let Some(chain) = self.parse_match_chain(expr) {
                let pending_import_before = self.needs_non_exhaustive_error_import;
                if let Some(stmts) = self.compile_arrow_match_stmts(chain) {
                    *arrow.body = BlockStmtOrExpr::BlockStmt(block_stmt(stmts));
                    return;
                }
                self.needs_non_exhaustive_error_import = pending_import_before;
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

            if import.type_only || !is_ts_pattern_import(&import.src.value.to_string_lossy()) {
                continue;
            }

            for specifier in &import.specifiers {
                match specifier {
                    ImportSpecifier::Named(named) if named.is_type_only => continue,
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
                            "isMatching" => {
                                self.is_matching_imports.insert(named.local.to_id());
                            }
                            "P" | "Pattern" => {
                                self.p_imports.insert(named.local.to_id());
                            }
                            "NonExhaustiveError" => {
                                self.non_exhaustive_error = Some(Expr::Ident(named.local.clone()));
                            }
                            _ => {}
                        }
                    }
                    ImportSpecifier::Namespace(namespace) => {
                        self.namespaces.insert(namespace.local.to_id());
                        self.non_exhaustive_error = Some(member_expr(
                            Expr::Ident(namespace.local.clone()),
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

        let bindings = self.const_bindings.keys().cloned().collect();
        module.visit_with(&mut MutationCollector {
            bindings: &bindings,
            mutated: &mut self.mutated_bindings,
        });
    }

    fn resolve_pattern_expr<'a>(&'a self, expr: &'a Expr) -> &'a Expr {
        let mut current = expr;
        for _ in 0..16 {
            current = match current {
                Expr::TsConstAssertion(assertion) => &assertion.expr,
                Expr::TsAs(assertion) => &assertion.expr,
                Expr::TsSatisfies(assertion) => &assertion.expr,
                Expr::Ident(ident) => match self.const_bindings.get(&ident.to_id()) {
                    Some(next)
                        if !self.mutated_bindings.contains(&ident.to_id())
                            && self.safe_const_pattern(next) =>
                    {
                        next
                    }
                    _ => return current,
                },
                _ => return current,
            };
        }
        current
    }

    fn safe_const_pattern(&self, expr: &Expr) -> bool {
        match expr {
            Expr::TsConstAssertion(assertion) => self.safe_const_pattern(&assertion.expr),
            Expr::TsAs(assertion) => self.safe_const_pattern(&assertion.expr),
            Expr::TsSatisfies(assertion) => self.safe_const_pattern(&assertion.expr),
            Expr::Lit(Lit::Regex(_)) => false,
            Expr::Lit(_) | Expr::Arrow(_) | Expr::Fn(_) => true,
            Expr::Unary(unary) => self.safe_const_pattern(&unary.arg),
            Expr::Tpl(tpl) => tpl.exprs.is_empty(),
            Expr::Object(object) => object.props.iter().all(|prop| match prop {
                PropOrSpread::Prop(prop) => match &**prop {
                    Prop::KeyValue(key_value) => {
                        !matches!(key_value.key, PropName::Computed(_))
                            && self.safe_const_pattern(&key_value.value)
                    }
                    _ => false,
                },
                PropOrSpread::Spread(_) => false,
            }),
            Expr::Array(array) => array
                .elems
                .iter()
                .flatten()
                .all(|elem| elem.spread.is_none() && self.safe_const_pattern(&elem.expr)),
            Expr::Member(_) => self.is_static_pattern_member(expr),
            Expr::Call(call) => {
                let Callee::Expr(callee) = &call.callee else {
                    return false;
                };
                // `P.name(...)`, or a method such as `.optional()` on one.
                let pure_callee = self.is_static_pattern_member(callee)
                    || matches!(&**callee, Expr::Member(member)
                        if matches!(&*member.obj, Expr::Call(_)) && self.safe_const_pattern(&member.obj));
                pure_callee
                    && call
                        .args
                        .iter()
                        .all(|arg| self.safe_const_pattern(&arg.expr))
            }
            _ => false,
        }
    }

    fn is_static_pattern_member(&self, expr: &Expr) -> bool {
        let Expr::Member(member) = expr else {
            return false;
        };
        match &*member.obj {
            Expr::Ident(ident) => {
                self.p_imports.contains(&ident.to_id())
                    || (self.namespaces.contains(&ident.to_id())
                        && member_name(member) == Some("P"))
            }
            Expr::Member(_) => self.is_static_pattern_member(&member.obj),
            _ => false,
        }
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

        if self.is_imported_call(call, &self.match_imports, "match") {
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

    /// Whether `call` calls the ts-pattern export `name`, imported as one of
    /// `imports` or through a namespace.
    fn is_imported_call(&self, call: &CallExpr, imports: &HashSet<Id>, name: &str) -> bool {
        match &call.callee {
            Callee::Expr(callee) => match &**callee {
                Expr::Ident(ident) => imports.contains(&ident.to_id()),
                Expr::Member(member) => self.is_namespace_member(member, name),
                _ => false,
            },
            _ => false,
        }
    }

    /// Lowers `isMatching(pattern, value)` and `isMatching(pattern)` like
    /// `match(value).with(pattern, () => true).otherwise(() => false)`. Only
    /// patterns that evaluate nothing compile, so argument order is kept.
    fn compile_is_matching(&mut self, expr: &Expr) -> Option<Expr> {
        let call = as_call(expr)?;
        if !self.is_imported_call(call, &self.is_matching_imports, "isMatching") {
            return None;
        }
        let param = private_ident!("_tsPatternValue");
        let (pattern, input) = match call.args.as_slice() {
            [pattern] => (&pattern.expr, Box::new(ident_expr(&param))),
            [pattern, value] => (&pattern.expr, value.expr.clone()),
            _ => return None,
        };
        let constant = |value| {
            Box::new(arrow_expr(
                vec![],
                BlockStmtOrExpr::Expr(Box::new(bool_lit(value))),
            ))
        };
        let chain = MatchChain {
            input,
            arms: vec![MatchArm {
                patterns: vec![pattern.clone()],
                guard: None,
                handler: constant(true),
            }],
            fallback: Fallback::Otherwise(constant(false)),
        };
        if needs_prepared_expression(self, &chain) {
            return None;
        }
        let test = self.compile_ternary_chain(chain)?;
        Some(match call.args.len() {
            1 => arrow_expr(
                vec![Pat::Ident(param.into())],
                BlockStmtOrExpr::Expr(Box::new(test)),
            ),
            _ => test,
        })
    }

    /// Returns `name` when `expr` resolves to `P.<name>`.
    fn p_member<'a>(&'a self, expr: &'a Expr) -> Option<&'a str> {
        let Expr::Member(member) = self.resolve_pattern_expr(expr) else {
            return None;
        };
        let is_p = match &*member.obj {
            Expr::Ident(ident) => self.p_imports.contains(&ident.to_id()),
            Expr::Member(parent) => self.is_namespace_member(parent, "P"),
            _ => false,
        };
        is_p.then(|| member_name(member)).flatten()
    }

    fn p_api<'a>(&'a self, expr: &'a Expr) -> Option<PApi<'a>> {
        match self.resolve_pattern_expr(expr) {
            Expr::Call(call) => self.p_call_api(call),
            expr => self.p_member(expr).map(PApi::Member),
        }
    }

    fn p_call_api<'a>(&'a self, call: &'a CallExpr) -> Option<PApi<'a>> {
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        if has_spread_arg(call) {
            return None;
        }
        if let Some(name) = self.p_member(callee) {
            return Some(PApi::Call(name, call));
        }
        let Expr::Member(member) = &**callee else {
            return None;
        };
        let method = member_name(member)?;
        if matches!(method, "optional" | "and" | "or") {
            return Some(PApi::Modifier(method, &member.obj, call));
        }
        Some(PApi::Chain(self.p_member(&member.obj)?, method, call))
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
        if self.p_member(callee).is_some() {
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
        let (mut stmts, chain) = self.snapshot_chain(chain);
        if chain.arms.iter().all(is_switchable_arm) {
            stmts.extend(self.compile_switch_stmts(chain));
        } else {
            stmts.extend(self.compile_body(
                &Sink::Return,
                &MatchInputs::new(*chain.input),
                chain.arms,
                chain.fallback,
                false,
            )?);
        }
        Some(stmts)
    }

    fn compile_chain(&mut self, chain: MatchChain) -> Option<Expr> {
        if needs_prepared_expression(self, &chain) {
            return self.compile_prepared_expression(chain);
        }
        self.compile_ternary_chain(chain)
    }

    /// Evaluates the input and the values the chain's patterns and callbacks
    /// evaluate once, in source order, into the returned bindings, and
    /// rewrites the chain to read them.
    fn prepare_chain(&self, chain: MatchChain) -> (Vec<(Ident, Expr)>, MatchChain) {
        let input = private_ident!("_tsPatternInput");
        let mut params = vec![input.clone()];
        let mut args = vec![*chain.input];
        let mut arms = Vec::with_capacity(chain.arms.len());
        for arm in chain.arms {
            let mut patterns = Vec::with_capacity(arm.patterns.len());
            for pattern in arm.patterns {
                let mut pattern = self.resolve_pattern_expr(&pattern).clone();
                prepare_pattern_expr(self, &mut pattern, &mut params, &mut args);
                patterns.push(Box::new(pattern));
            }
            let guard = arm
                .guard
                .map(|guard| prepare_callback(guard, "_tsPatternGuard", &mut params, &mut args));
            let handler =
                prepare_callback(arm.handler, "_tsPatternHandler", &mut params, &mut args);
            arms.push(MatchArm {
                patterns,
                guard,
                handler,
            });
        }
        let fallback = match chain.fallback {
            Fallback::Otherwise(handler) => Fallback::Otherwise(prepare_callback(
                handler,
                "_tsPatternOtherwise",
                &mut params,
                &mut args,
            )),
            fallback => fallback,
        };
        let chain = MatchChain {
            input: Box::new(ident_expr(&input)),
            arms,
            fallback,
        };
        (params.into_iter().zip(args).collect(), chain)
    }

    /// Declares the input, and anything preparation evaluates, as constants.
    fn snapshot_chain(&self, chain: MatchChain) -> (Vec<Stmt>, MatchChain) {
        let (bindings, chain) = if needs_prepared_expression(self, &chain) {
            self.prepare_chain(chain)
        } else {
            let input = private_ident!("_tsPatternInput");
            let bindings = vec![(input.clone(), *chain.input)];
            let input = Box::new(ident_expr(&input));
            (bindings, MatchChain { input, ..chain })
        };
        let stmts = bindings
            .into_iter()
            .map(|(ident, value)| const_stmt(ident, value))
            .collect();
        (stmts, chain)
    }

    /// An expression cannot declare constants, so prepared bindings become
    /// the parameters of an immediately invoked arrow.
    fn compile_prepared_expression(&mut self, chain: MatchChain) -> Option<Expr> {
        let (bindings, chain) = self.prepare_chain(chain);
        let body = match chain.fallback {
            Fallback::Otherwise(_) => {
                BlockStmtOrExpr::Expr(Box::new(self.compile_ternary_chain(chain)?))
            }
            fallback => BlockStmtOrExpr::BlockStmt(block_stmt(self.compile_body(
                &Sink::Return,
                &MatchInputs::new(*chain.input),
                chain.arms,
                fallback,
                false,
            )?)),
        };
        let (params, args) = bindings
            .into_iter()
            .map(|(ident, value)| (Pat::Ident(ident.into()), value))
            .unzip();
        Some(call_expr(paren_expr(arrow_expr(params, body)), args))
    }

    /// Lowers literal-only arms on a snapshotted input to a `switch`.
    fn compile_switch_stmts(&mut self, chain: MatchChain) -> Vec<Stmt> {
        let input = *chain.input;
        let mut cases = Vec::new();

        for arm in chain.arms {
            let handler_call = handler_result(arm.handler, vec![input.clone(), input.clone()]);
            let mut arm_patterns = arm.patterns.into_iter().peekable();

            while let Some(pattern) = arm_patterns.next() {
                // Patterns of one arm fall through to its handler.
                let cons = if arm_patterns.peek().is_some() {
                    Vec::new()
                } else {
                    vec![return_stmt(handler_call.clone())]
                };
                cases.push(switch_case(Some(*pattern), cons));
            }
        }

        let fallback = self.fallback_stmt(&Sink::Return, chain.fallback, input.clone());
        cases.push(switch_case(None, vec![fallback]));
        vec![switch_stmt(input, cases)]
    }

    fn compile_assign_match_stmts(&mut self, target: Expr, chain: MatchChain) -> Option<Vec<Stmt>> {
        let (mut stmts, chain) =
            if can_inline_input(&chain.input) && !needs_prepared_expression(self, &chain) {
                (Vec::new(), chain)
            } else {
                self.snapshot_chain(chain)
            };
        stmts.extend(self.compile_body(
            &Sink::Assign(target),
            &MatchInputs::new(*chain.input),
            chain.arms,
            chain.fallback,
            false,
        )?);
        Some(stmts)
    }

    /// Lowers arms to statements. `known_match_object` means an enclosing
    /// switch already proved `inputs.test` is an object.
    fn compile_body(
        &mut self,
        sink: &Sink,
        inputs: &MatchInputs,
        arms: Vec<MatchArm>,
        fallback: Fallback,
        known_match_object: bool,
    ) -> Option<Vec<Stmt>> {
        if known_match_object {
            if let Some((prefix, prefixed_arms)) = common_object_prefix(&arms) {
                let cached = private_ident_for_prop("_tsPattern", &prefix);
                let mut stmts = vec![const_stmt(
                    cached.clone(),
                    prop_access(inputs.test.clone(), &prefix)?,
                )];
                let inputs = MatchInputs {
                    test: ident_expr(&cached),
                    handler: inputs.handler.clone(),
                };
                stmts.extend(self.compile_body(sink, &inputs, prefixed_arms, fallback, false)?);
                return Some(stmts);
            }
        }

        if let Some(path) = common_switch_path(&arms) {
            return self.compile_path_switch(
                sink,
                inputs,
                arms,
                fallback,
                &path,
                known_match_object,
            );
        }

        let mut stmts = Vec::new();
        let mut branches = Vec::with_capacity(arms.len());
        for (arm_index, arm) in arms.into_iter().enumerate() {
            let (test, handler_input, captures) =
                self.arm_parts(inputs.test.clone(), inputs.handler.clone(), &arm, arm_index)?;
            stmts.extend(captures.into_iter().map(let_decl));
            let result = handler_result(arm.handler, vec![handler_input, inputs.handler.clone()]);
            branches.push((test, sink.deliver(result)));
        }
        let fallback = self.fallback_stmt(sink, fallback, inputs.handler.clone());
        stmts.extend(sink.if_chain(branches, fallback));
        Some(stmts)
    }

    fn compile_path_switch(
        &mut self,
        sink: &Sink,
        inputs: &MatchInputs,
        arms: Vec<MatchArm>,
        fallback: Fallback,
        path: &[PropName],
        known_match_object: bool,
    ) -> Option<Vec<Stmt>> {
        let discriminant = prop_access_path(inputs.test.clone(), path)?;
        let guard = unary(
            UnaryOp::Bang,
            paren_expr(object_path_base_test(
                inputs.test.clone(),
                path,
                known_match_object,
            )?),
        );
        let default = self.fallback_stmt(sink, fallback.clone(), inputs.handler.clone());
        let mut cases = Vec::new();

        for (value, group) in group_arms_by_path(arms, path)? {
            let (group_arms, group_fallback) = strip_group_arms(group, path, fallback.clone())?;
            let cons = if group_arms.is_empty() {
                vec![self.fallback_stmt(sink, group_fallback, inputs.handler.clone())]
            } else {
                self.compile_body(sink, inputs, group_arms, group_fallback, true)?
            };
            cases.push(switch_case(Some(value), sink.end_case(cons)));
        }
        cases.push(switch_case(None, sink.end_case(vec![default.clone()])));

        let switch = switch_stmt(discriminant, cases);
        Some(match sink {
            Sink::Return => vec![
                if_stmt(guard, Stmt::Block(block_stmt(vec![default])), None),
                switch,
            ],
            Sink::Assign(_) => vec![if_stmt(guard, default, Some(switch))],
        })
    }

    fn fallback_stmt(&mut self, sink: &Sink, fallback: Fallback, input: Expr) -> Stmt {
        match fallback {
            Fallback::Otherwise(handler) => sink.deliver(handler_result(handler, vec![input])),
            Fallback::Matched(handler) => {
                sink.deliver(handler_result(handler, vec![input.clone(), input]))
            }
            Fallback::Exhaustive => Stmt::Throw(ThrowStmt {
                span: DUMMY_SP,
                arg: Box::new(Expr::New(NewExpr {
                    span: DUMMY_SP,
                    ctxt: Default::default(),
                    callee: Box::new(self.exhaustive_error_callee()),
                    args: Some(vec![ExprOrSpread {
                        spread: None,
                        expr: Box::new(input),
                    }]),
                    type_args: None,
                })),
            }),
        }
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
                    let pending_import_before = self.needs_non_exhaustive_error_import;
                    let Some(mut assign_stmts) =
                        self.compile_assign_match_stmts(ident_expr(&result), chain)
                    else {
                        self.needs_non_exhaustive_error_import = pending_import_before;
                        rewritten.push(Stmt::Decl(Decl::Var(var)));
                        continue;
                    };

                    // Initialize the original binding only after the match succeeds.
                    // Declaring it earlier would end its TDZ and turn const into let.
                    var.decls[0].init = Some(Box::new(ident_expr(&result)));
                    rewritten.push(let_decl(result));
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

        for (arm_index, arm) in chain.arms.into_iter().enumerate().rev() {
            let (test, handler_input, captures) =
                self.arm_parts(input.clone(), input.clone(), &arm, arm_index)?;
            let branch = cond_expr(
                test,
                handler_result(arm.handler, vec![handler_input, input.clone()]),
                expression,
            );
            expression = if captures.is_empty() {
                branch
            } else {
                // An expression cannot declare the capture bindings.
                let args = vec![undefined_expr(self.unresolved_ctxt); captures.len()];
                let params = captures
                    .into_iter()
                    .map(|ident| Pat::Ident(ident.into()))
                    .collect();
                call_expr(
                    paren_expr(arrow_expr(params, BlockStmtOrExpr::Expr(Box::new(branch)))),
                    args,
                )
            };
        }

        Some(expression)
    }

    fn arm_parts(
        &self,
        match_input: Expr,
        guard_input: Expr,
        arm: &MatchArm,
        arm_index: usize,
    ) -> Option<(Expr, Expr, Vec<Ident>)> {
        let mut selections = Vec::new();
        let pattern_test = if arm.patterns.len() == 1
            && matches!(*arm.patterns[0], Expr::Invalid(_))
        {
            bool_lit(true)
        } else {
            arm.patterns
                .iter()
                .map(|pattern| {
                    self.pattern_test_with_selections(match_input.clone(), pattern, &mut selections)
                })
                .try_fold(None, |acc, test| {
                    let test = test?;
                    Some(Some(match acc {
                        Some(acc) => or(acc, test),
                        None => test,
                    }))
                })??
        };

        let mut test = match &arm.guard {
            Some(guard) => and(pattern_test, predicate_result(guard, guard_input)),
            None => pattern_test,
        };
        let mut captures = Vec::with_capacity(selections.len());
        let mut grouped_captures: Vec<(Id, Ident)> = Vec::new();
        let mut handler_selections = Vec::with_capacity(selections.len());
        for (selection_index, selection) in selections.into_iter().enumerate() {
            if !selection_needs_capture(&selection.value) {
                handler_selections.push(selection);
                continue;
            }

            let existing_capture = selection.capture_group.as_ref().and_then(|group| {
                grouped_captures
                    .iter()
                    .find(|(capture_group, _)| capture_group == group)
                    .map(|(_, capture)| capture.clone())
            });
            let capture = if let Some(capture) = existing_capture {
                capture
            } else {
                let capture = private_ident_for_index(
                    "_tsPatternSelected",
                    arm_index.saturating_mul(16) + selection_index,
                );
                let mut replacer = SelectionCapture {
                    target: selection.value.clone(),
                    capture: capture.clone(),
                    replaced: false,
                };
                test.visit_mut_with(&mut replacer);
                if !replacer.replaced {
                    test = seq_expr(assign_expr(capture.clone(), selection.value), test);
                }
                if let Some(group) = selection.capture_group.clone() {
                    grouped_captures.push((group, capture.clone()));
                }
                captures.push(capture.clone());
                capture
            };
            handler_selections.push(SelectionBinding {
                name: selection.name,
                value: selection_capture_projection(&capture, selection.projection),
                capture_group: None,
                projection: None,
            });
        }

        let handler_input = selection_handler_input(match_input, handler_selections)?;
        Some((test, handler_input, captures))
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

    fn pattern_test_with_selections(
        &self,
        value: Expr,
        pattern: &Expr,
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        let pattern = self.resolve_pattern_expr(pattern);

        if let Some((call, base)) = self.select_call(pattern) {
            let (name, inner) = match (base, call.args.as_slice()) {
                (None, []) => (None, None),
                (Some(base), []) => (None, Some(base)),
                (None, [arg]) => match selection_name(&arg.expr) {
                    Some(name) => (Some(name), None),
                    None => (None, Some(&*arg.expr)),
                },
                (Some(base), [name]) => (Some(selection_name(&name.expr)?), Some(base)),
                (None, [name, inner]) => (Some(selection_name(&name.expr)?), Some(&*inner.expr)),
                _ => return None,
            };
            if name.as_deref() == Some("__proto__") {
                return None;
            }
            selections.push(SelectionBinding {
                name,
                value: value.clone(),
                ..Default::default()
            });
            return match inner {
                Some(inner) => self.pattern_test_with_selections(value, inner, selections),
                None => Some(bool_lit(true)),
            };
        }

        if let Some(api) = self.p_api(pattern) {
            return self.p_api_test(value, api, selections);
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
                Some(strict_eq(value, undefined_expr(self.unresolved_ctxt)))
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

    fn p_api_test(
        &self,
        value: Expr,
        api: PApi,
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        let ctxt = self.unresolved_ctxt;
        match api {
            PApi::Member("_") => Some(bool_lit(true)),
            PApi::Member(name @ ("string" | "number" | "boolean" | "bigint" | "symbol")) => {
                Some(typeof_eq(value, name))
            }
            PApi::Member("nullish") => Some(or(
                strict_eq(value.clone(), null_lit()),
                strict_eq(value, undefined_expr(ctxt)),
            )),
            PApi::Member("nonNullable") => Some(and(
                strict_ne(value.clone(), null_lit()),
                strict_ne(value, undefined_expr(ctxt)),
            )),
            PApi::Call("optional", call) => {
                self.optional_test(value, single_arg(call)?, selections)
            }
            PApi::Call("not", call) => Some(unary(
                UnaryOp::Bang,
                self.unselected_test(value, single_arg(call)?)?,
            )),
            PApi::Call("union", call) => join_tests(
                call.args
                    .iter()
                    .map(|arg| self.unselected_test(value.clone(), &arg.expr)),
                or,
            ),
            PApi::Call("intersection", call) => join_tests(
                call.args
                    .iter()
                    .map(|arg| self.unselected_test(value.clone(), &arg.expr)),
                and,
            ),
            PApi::Call("array", call) => self.array_of_test(value, call),
            PApi::Call("set", call) => self.set_test(value, call),
            PApi::Call("map", call) => self.map_test(value, call),
            PApi::Call("record", call) => self.record_test(value, call, selections),
            PApi::Call("instanceOf", call) => {
                Some(bin(BinaryOp::InstanceOf, value, single_arg(call)?.clone()))
            }
            PApi::Call("when", call) => Some(predicate_result(single_arg(call)?, value)),
            PApi::Modifier(method, base, _) if !self.has_pattern_method(base, method) => None,
            PApi::Modifier("optional", base, call) if call.args.is_empty() => {
                self.optional_test(value, base, selections)
            }
            PApi::Modifier(method @ ("and" | "or"), base, call) => {
                Some((if method == "and" { and } else { or })(
                    self.unselected_test(value.clone(), base)?,
                    self.unselected_test(value, single_arg(call)?)?,
                ))
            }
            PApi::Chain("string", method, call) => string_chain_test(value, method, call),
            PApi::Chain("number", method, call) => number_chain_test(value, method, call, ctxt),
            _ => None,
        }
    }

    /// Whether ts-pattern attaches `method` to the pattern `base` evaluates to.
    fn has_pattern_method(&self, base: &Expr, method: &str) -> bool {
        const CHAINABLE: &[&str] = &["optional", "and", "or", "select"];
        const ARRAY_CHAINABLE: &[&str] = &["optional", "select"];
        let methods = match self.p_api(base) {
            Some(PApi::Member(
                "_" | "any" | "unknown" | "string" | "number" | "bigint" | "boolean" | "symbol"
                | "nullish" | "nonNullable",
            )) => CHAINABLE,
            Some(PApi::Call(
                "optional" | "set" | "map" | "record" | "intersection" | "union" | "not" | "select"
                | "instanceOf" | "shape",
                _,
            )) => CHAINABLE,
            Some(PApi::Call("array", _)) => ARRAY_CHAINABLE,
            Some(PApi::Chain("string" | "number" | "bigint", _, _)) => CHAINABLE,
            Some(PApi::Modifier("optional", base, _)) if self.has_pattern_method(base, "and") => {
                CHAINABLE
            }
            Some(PApi::Modifier("optional", _, _)) => ARRAY_CHAINABLE,
            Some(PApi::Modifier(..)) => CHAINABLE,
            _ => &[],
        };
        methods.contains(&method)
    }

    /// Tests a nested pattern that must not contain selections.
    fn unselected_test(&self, value: Expr, pattern: &Expr) -> Option<Expr> {
        let mut selections = Vec::new();
        let test = self.pattern_test_with_selections(value, pattern, &mut selections)?;
        selections.is_empty().then_some(test)
    }

    fn optional_test(
        &self,
        value: Expr,
        pattern: &Expr,
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        let undefined = undefined_expr(self.unresolved_ctxt);
        let mut inner_selections = Vec::new();
        let test =
            self.pattern_test_with_selections(value.clone(), pattern, &mut inner_selections)?;
        selections.extend(
            inner_selections
                .into_iter()
                .map(|selection| SelectionBinding {
                    value: cond_expr(
                        strict_eq(value.clone(), undefined.clone()),
                        undefined.clone(),
                        selection.value,
                    ),
                    ..selection
                }),
        );
        Some(or(strict_eq(value, undefined), test))
    }

    fn array_of_test(&self, value: Expr, call: &CallExpr) -> Option<Expr> {
        let array_test = array_is_array(value.clone(), self.unresolved_ctxt);
        match call.args.as_slice() {
            [] => Some(array_test),
            [item] => {
                let item_ident = private_ident!("_tsPatternItem");
                let item_test = self.unselected_test(ident_expr(&item_ident), &item.expr)?;
                Some(and(
                    array_test,
                    call_expr(
                        member_expr(value, "every"),
                        vec![arrow_expr(
                            vec![Pat::Ident(item_ident.into())],
                            BlockStmtOrExpr::Expr(Box::new(item_test)),
                        )],
                    ),
                ))
            }
            _ => None,
        }
    }

    fn set_test(&self, value: Expr, call: &CallExpr) -> Option<Expr> {
        let set_test = instance_of(value.clone(), "Set", self.unresolved_ctxt);
        match call.args.as_slice() {
            [] => Some(set_test),
            [item] => {
                let item_ident = private_ident!("_tsPatternItem");
                let item_test = self.unselected_test(ident_expr(&item_ident), &item.expr)?;
                Some(and(
                    set_test,
                    iterable_every(value, Pat::Ident(item_ident.into()), item_test),
                ))
            }
            _ => None,
        }
    }

    fn map_test(&self, value: Expr, call: &CallExpr) -> Option<Expr> {
        let map_test = instance_of(value.clone(), "Map", self.unresolved_ctxt);
        let [key, entry] = call.args.as_slice() else {
            return call.args.is_empty().then_some(map_test);
        };
        let key_ident = private_ident!("_tsPatternKey");
        let value_ident = private_ident!("_tsPatternValue");
        let key_test = self.unselected_test(ident_expr(&key_ident), &key.expr)?;
        let value_test = self.unselected_test(ident_expr(&value_ident), &entry.expr)?;
        Some(and(
            map_test,
            iterable_every(
                call_expr(member_expr(value, "entries"), vec![]),
                Pat::Array(ArrayPat {
                    span: DUMMY_SP,
                    elems: vec![
                        Some(Pat::Ident(key_ident.into())),
                        Some(Pat::Ident(value_ident.into())),
                    ],
                    optional: false,
                    type_ann: None,
                }),
                ordered_and(key_test, value_test),
            ),
        ))
    }

    fn record_test(
        &self,
        value: Expr,
        call: &CallExpr,
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        let ctxt = self.unresolved_ctxt;
        let (key_pattern, value_pattern) = match call.args.as_slice() {
            [value] => (None, &*value.expr),
            [key, value] => (Some(&*key.expr), &*value.expr),
            _ => return None,
        };
        let key_ident = private_ident!("_tsPatternKey");
        let value_ident = private_ident!("_tsPatternValue");
        // Records also match numeric keys through their string form, as
        // ts-pattern does.
        let numeric_key_test = |key: Expr, test: Expr| {
            and(
                typeof_eq(ident_expr(&key_ident), "string"),
                and(
                    unary(
                        UnaryOp::Bang,
                        call_expr(
                            member_expr(global_ident("Number", ctxt), "isNaN"),
                            vec![key],
                        ),
                    ),
                    test,
                ),
            )
        };
        let mut key_selections = Vec::new();
        let mut coerced_key = None;
        let key_test = match key_pattern {
            None => typeof_eq(ident_expr(&key_ident), "string"),
            Some(key_pattern) => {
                let direct_key_test = self.pattern_test_with_selections(
                    ident_expr(&key_ident),
                    key_pattern,
                    &mut key_selections,
                )?;
                if key_selections.is_empty() {
                    let coerced =
                        call_expr(global_ident("Number", ctxt), vec![ident_expr(&key_ident)]);
                    ordered_or(
                        numeric_key_test(coerced.clone(), self.pattern_test(coerced, key_pattern)?),
                        direct_key_test,
                    )
                } else {
                    let coerced_ident = private_ident!("_tsPatternCoercedKey");
                    let test = numeric_key_test(
                        ident_expr(&coerced_ident),
                        self.pattern_test(ident_expr(&coerced_ident), key_pattern)?,
                    );
                    coerced_key = Some((coerced_ident, test));
                    direct_key_test
                }
            }
        };
        let mut value_selections = Vec::new();
        let value_test = self.pattern_test_with_selections(
            ident_expr(&value_ident),
            value_pattern,
            &mut value_selections,
        )?;
        let all_selections = key_selections
            .iter()
            .chain(value_selections.iter())
            .collect::<Vec<_>>();
        if all_selections.iter().any(|selection| {
            selection
                .name
                .as_deref()
                .is_some_and(unsupported_record_selection_name)
        }) {
            return None;
        }

        if all_selections.is_empty() {
            let key_match_ident = private_ident!("_tsPatternRecordKeyMatch");
            let value_match_ident = private_ident!("_tsPatternRecordValueMatch");
            return Some(and(
                and(
                    and(
                        strict_ne(value.clone(), null_lit()),
                        typeof_eq(value.clone(), "object"),
                    ),
                    unary(UnaryOp::Bang, array_is_array(value.clone(), ctxt)),
                ),
                call_expr(
                    member_expr(reflect_own_keys(value.clone(), ctxt), "every"),
                    vec![arrow_expr(
                        vec![Pat::Ident(key_ident.clone().into())],
                        BlockStmtOrExpr::BlockStmt(block_stmt(vec![
                            const_stmt(
                                value_ident,
                                computed_member_expr(value, ident_expr(&key_ident)),
                            ),
                            const_stmt(key_match_ident.clone(), key_test),
                            const_stmt(value_match_ident.clone(), value_test),
                            return_stmt(ordered_and(
                                ident_expr(&key_match_ident),
                                ident_expr(&value_match_ident),
                            )),
                        ])),
                    )],
                ),
            ));
        }

        if mixes_anonymous_selection(all_selections.iter().copied()) {
            return None;
        }
        let group = private_ident!("_tsPatternRecordSelection");
        let result = record_selection_match(RecordSelectionMatch {
            value,
            ctxt,
            key_ident,
            value_ident,
            key_test,
            coerced_key,
            value_test,
            key_selections: &key_selections,
            value_selections: &value_selections,
        });
        selections.extend(all_selections.into_iter().map(|selection| {
            let mut projection = vec!["selection".to_string()];
            projection.extend(selection.name.clone());
            SelectionBinding {
                name: selection.name.clone(),
                value: result.clone(),
                capture_group: Some(group.to_id()),
                projection: Some(projection),
            }
        }));
        Some(member_expr(result, "matched"))
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
            let test = self.pattern_test_with_selections(
                prop_access.clone(),
                &key_value.value,
                selections,
            )?;
            let test = if self.is_optional_pattern(&key_value.value) {
                test
            } else {
                let test = if as_bool_lit(&test) == Some(true) {
                    seq_expr(prop_access, bool_lit(true))
                } else {
                    test
                };
                and(prop_in_object(&key_value.key, value.clone())?, test)
            };
            Some(and(acc, test))
        })
    }

    fn is_optional_pattern(&self, pattern: &Expr) -> bool {
        match self.p_api(pattern) {
            Some(PApi::Call("optional", _)) => true,
            Some(PApi::Modifier("optional", _, call)) => call.args.is_empty(),
            _ => false,
        }
    }

    fn array_test(
        &self,
        value: Expr,
        array: &ArrayLit,
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        let len = array.elems.len();
        let base = and(
            array_is_array(value.clone(), self.unresolved_ctxt),
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

struct MutationCollector<'a> {
    bindings: &'a HashSet<Id>,
    mutated: &'a mut HashSet<Id>,
}

impl Visit for MutationCollector<'_> {
    fn visit_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Assign(assign) => self.mark_assign_target(&assign.left),
            Expr::Update(update) => self.mark_expr(&update.arg),
            Expr::Unary(unary) if unary.op == UnaryOp::Delete => self.mark_expr(&unary.arg),
            _ => {}
        }
        expr.visit_children_with(self);
    }
}

impl MutationCollector<'_> {
    fn mark_assign_target(&mut self, target: &AssignTarget) {
        match target {
            AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) => self.mark_id(ident.to_id()),
            AssignTarget::Simple(SimpleAssignTarget::Member(member)) => self.mark_member(member),
            AssignTarget::Simple(_) | AssignTarget::Pat(_) => {}
        }
    }

    fn mark_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Ident(ident) => self.mark_id(ident.to_id()),
            Expr::Member(member) => self.mark_member(member),
            _ => {}
        }
    }

    fn mark_member(&mut self, member: &MemberExpr) {
        let mut current = &*member.obj;
        loop {
            match current {
                Expr::Ident(ident) => {
                    self.mark_id(ident.to_id());
                    return;
                }
                Expr::Member(member) => current = &member.obj,
                _ => return,
            }
        }
    }

    fn mark_id(&mut self, id: Id) {
        if self.bindings.contains(&id) {
            self.mutated.insert(id);
        }
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

fn needs_prepared_expression(transformer: &TsPatternTransformer, chain: &MatchChain) -> bool {
    chain.arms.iter().any(|arm| {
        arm.patterns
            .iter()
            .any(|pattern| pattern_needs_preparation(transformer, pattern))
            || arm
                .guard
                .as_ref()
                .is_some_and(|guard| !is_function_expr(guard) || guard_requires_snapshot(guard))
            || handler_needs_preparation(&arm.handler)
    }) || matches!(&chain.fallback, Fallback::Otherwise(handler) if handler_needs_preparation(handler))
}

fn guard_requires_snapshot(guard: &Expr) -> bool {
    match guard {
        Expr::Arrow(arrow) => match &*arrow.body {
            BlockStmtOrExpr::Expr(expr) => is_effectful_expr(expr),
            BlockStmtOrExpr::BlockStmt(block) => block_contains_effect(block),
        },
        Expr::Fn(_) => true,
        _ => true,
    }
}

fn block_contains_effect(block: &BlockStmt) -> bool {
    let mut detector = EffectDetector { effect: false };
    block.visit_with(&mut detector);
    detector.effect
}

struct EffectDetector {
    effect: bool,
}

impl Visit for EffectDetector {
    fn visit_expr(&mut self, expr: &Expr) {
        if matches!(
            expr,
            Expr::Call(_)
                | Expr::New(_)
                | Expr::Assign(_)
                | Expr::Update(_)
                | Expr::Await(_)
                | Expr::Yield(_)
                | Expr::TaggedTpl(_)
                | Expr::Seq(_)
        ) {
            self.effect = true;
        }
        expr.visit_children_with(self);
    }
}

/// How a call inside a pattern consumes its arguments.
#[derive(Clone, Copy)]
enum PatternCall {
    /// `P.when(predicate)`
    When,
    /// `P.select(...)` or `pattern.select(...)`
    Select,
    /// Arguments are nested patterns.
    Nested,
    /// `pattern.optional()`, `pattern.and(other)` or `pattern.or(other)`
    Modifier,
    /// Arguments are plain values, e.g. `P.string.startsWith(prefix)`.
    Values,
    /// Unknown calls stay unprepared and fail lowering.
    Opaque,
}

impl TsPatternTransformer {
    fn pattern_call(&self, call: &CallExpr) -> PatternCall {
        let Callee::Expr(callee) = &call.callee else {
            return PatternCall::Opaque;
        };
        if matches!(&**callee, Expr::Member(member) if member_name(member) == Some("select")) {
            return PatternCall::Select;
        }
        match self.p_call_api(call) {
            Some(PApi::Call("when", _)) => PatternCall::When,
            Some(PApi::Call(
                "union" | "intersection" | "optional" | "not" | "array" | "set" | "map" | "record",
                _,
            )) => PatternCall::Nested,
            Some(PApi::Call("instanceOf", _) | PApi::Chain(..)) => PatternCall::Values,
            Some(PApi::Modifier(..)) => PatternCall::Modifier,
            _ => PatternCall::Opaque,
        }
    }
}

fn pattern_needs_preparation(transformer: &TsPatternTransformer, expr: &Expr) -> bool {
    let needs = |expr: &Expr| pattern_needs_preparation(transformer, expr);
    match transformer.resolve_pattern_expr(expr) {
        Expr::Object(object) => object.props.iter().any(|prop| match prop {
            PropOrSpread::Prop(prop) => match &**prop {
                Prop::KeyValue(key_value) => {
                    matches!(key_value.key, PropName::Computed(_)) || needs(&key_value.value)
                }
                _ => true,
            },
            PropOrSpread::Spread(_) => true,
        }),
        Expr::Array(array) => array
            .elems
            .iter()
            .flatten()
            .any(|elem| elem.spread.is_some() || needs(&elem.expr)),
        Expr::Paren(paren) => needs(&paren.expr),
        Expr::Call(call) => match transformer.pattern_call(call) {
            PatternCall::When => call.args.first().is_none_or(|arg| {
                !is_function_expr(&arg.expr) || guard_requires_snapshot(&arg.expr)
            }),
            PatternCall::Select => {
                method_base(call).is_some_and(needs)
                    || match call.args.as_slice() {
                        [] => false,
                        [arg] if selection_name(&arg.expr).is_some() => false,
                        [pattern] | [_, pattern] => needs(&pattern.expr),
                        _ => true,
                    }
            }
            PatternCall::Nested => call.args.iter().any(|arg| needs(&arg.expr)),
            PatternCall::Modifier => {
                method_base(call).is_some_and(needs) || call.args.iter().any(|arg| needs(&arg.expr))
            }
            PatternCall::Values => call
                .args
                .iter()
                .any(|arg| value_needs_preparation(&arg.expr)),
            PatternCall::Opaque => true,
        },
        _ => false,
    }
}

/// The object a method call is made on, e.g. `base` in `base.select()`.
fn method_base(call: &CallExpr) -> Option<&Expr> {
    match &call.callee {
        Callee::Expr(callee) => match &**callee {
            Expr::Member(member) => Some(&member.obj),
            _ => None,
        },
        _ => None,
    }
}

fn method_base_mut(call: &mut CallExpr) -> Option<&mut Expr> {
    match &mut call.callee {
        Callee::Expr(callee) => match &mut **callee {
            Expr::Member(member) => Some(&mut member.obj),
            _ => None,
        },
        _ => None,
    }
}

fn value_needs_preparation(expr: &Expr) -> bool {
    match expr {
        Expr::Lit(Lit::Regex(_)) => true,
        Expr::Lit(_) | Expr::Arrow(_) | Expr::Fn(_) => false,
        Expr::Paren(paren) => value_needs_preparation(&paren.expr),
        _ => true,
    }
}

fn handler_needs_preparation(handler: &Expr) -> bool {
    !is_function_expr(handler)
}

/// Moves the values a pattern evaluates into arguments of the prepared
/// expression so they are evaluated once, in source order.
fn prepare_pattern_expr(
    transformer: &TsPatternTransformer,
    expr: &mut Expr,
    params: &mut Vec<Ident>,
    args: &mut Vec<Expr>,
) {
    match expr {
        Expr::Object(object) => {
            for prop in &mut object.props {
                let PropOrSpread::Prop(prop) = prop else {
                    continue;
                };
                let Prop::KeyValue(key_value) = &mut **prop else {
                    continue;
                };
                if let PropName::Computed(computed) = &mut key_value.key {
                    let ident = private_ident!("_tsPatternKey");
                    params.push(ident.clone());
                    args.push(std::mem::replace(&mut *computed.expr, ident_expr(&ident)));
                }
                prepare_pattern_expr(transformer, &mut key_value.value, params, args);
            }
        }
        Expr::Array(array) => {
            for elem in array.elems.iter_mut().flatten() {
                prepare_pattern_expr(transformer, &mut elem.expr, params, args);
            }
        }
        Expr::Paren(paren) => prepare_pattern_expr(transformer, &mut paren.expr, params, args),
        Expr::Call(call) => match transformer.pattern_call(call) {
            PatternCall::When => {
                if let Some(arg) = call.args.first_mut() {
                    capture_prepared_value(&mut arg.expr, params, args);
                }
            }
            PatternCall::Select => {
                if let Some(base) = method_base_mut(call) {
                    prepare_pattern_expr(transformer, base, params, args);
                }
                match call.args.as_mut_slice() {
                    [arg] if selection_name(&arg.expr).is_some() => {}
                    [pattern] | [_, pattern] => {
                        prepare_pattern_expr(transformer, &mut pattern.expr, params, args)
                    }
                    _ => {}
                }
            }
            PatternCall::Modifier => {
                if let Some(base) = method_base_mut(call) {
                    prepare_pattern_expr(transformer, base, params, args);
                }
                for arg in &mut call.args {
                    prepare_pattern_expr(transformer, &mut arg.expr, params, args);
                }
            }
            PatternCall::Nested => {
                for arg in &mut call.args {
                    prepare_pattern_expr(transformer, &mut arg.expr, params, args);
                }
            }
            PatternCall::Values => {
                for arg in &mut call.args {
                    capture_prepared_value(&mut arg.expr, params, args);
                }
            }
            PatternCall::Opaque => {}
        },
        _ => {}
    }
}

/// Moves a non-function callback into a prepared binding. Creating a function
/// has no effects, so function expressions stay inline.
fn prepare_callback(
    callback: Box<Expr>,
    name: &str,
    params: &mut Vec<Ident>,
    args: &mut Vec<Expr>,
) -> Box<Expr> {
    if is_function_expr(&callback) {
        return callback;
    }
    let ident = private_ident!(DUMMY_SP, name);
    params.push(ident.clone());
    args.push(*callback);
    Box::new(ident_expr(&ident))
}

fn capture_prepared_value(expr: &mut Expr, params: &mut Vec<Ident>, args: &mut Vec<Expr>) {
    if is_function_expr(expr) {
        return;
    }
    let ident = private_ident!("_tsPatternValue");
    params.push(ident.clone());
    args.push(expr.clone());
    *expr = ident_expr(&ident);
}

fn is_effectful_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Call(_)
        | Expr::New(_)
        | Expr::Assign(_)
        | Expr::Update(_)
        | Expr::Await(_)
        | Expr::Yield(_)
        | Expr::TaggedTpl(_)
        | Expr::Seq(_) => true,
        Expr::Object(object) => object.props.iter().any(|prop| match prop {
            PropOrSpread::Prop(prop) => match &**prop {
                Prop::KeyValue(key_value) => {
                    matches!(key_value.key, PropName::Computed(_))
                        || is_effectful_expr(&key_value.value)
                }
                Prop::Getter(_) | Prop::Setter(_) | Prop::Method(_) => true,
                Prop::Assign(_) | Prop::Shorthand(_) => false,
            },
            PropOrSpread::Spread(spread) => is_effectful_expr(&spread.expr),
        }),
        Expr::Array(array) => array
            .elems
            .iter()
            .flatten()
            .any(|elem| elem.spread.is_some() || is_effectful_expr(&elem.expr)),
        Expr::Paren(paren) => is_effectful_expr(&paren.expr),
        _ => false,
    }
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
        guard: Some(call.args[0].expr.clone()),
        handler: call.args[1].expr.clone(),
    })
}

fn selection_needs_capture(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Member(_) | Expr::Call(_) | Expr::New(_) | Expr::Seq(_)
    )
}

fn selection_capture_projection(capture: &Ident, projection: Option<Vec<String>>) -> Expr {
    let mut value = ident_expr(capture);
    if let Some(projection) = projection {
        for prop in projection {
            value = member_expr(value, &prop);
        }
    }
    value
}

fn selection_handler_input(input: Expr, selections: Vec<SelectionBinding>) -> Option<Expr> {
    if selections.is_empty() {
        return Some(input);
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

struct SelectionCapture {
    target: Expr,
    capture: Ident,
    replaced: bool,
}

impl VisitMut for SelectionCapture {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        if !self.replaced && expr.eq_ignore_span(&self.target) {
            *expr = assign_expr(self.capture.clone(), self.target.clone());
            self.replaced = true;
            return;
        }
        expr.visit_mut_children_with(self);
    }
}

fn assign_expr(target: Ident, value: Expr) -> Expr {
    paren_expr(Expr::Assign(AssignExpr {
        span: DUMMY_SP,
        op: AssignOp::Assign,
        left: AssignTarget::Simple(SimpleAssignTarget::Ident(target.into())),
        right: Box::new(value),
    }))
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

fn unsupported_record_selection_name(name: &str) -> bool {
    matches!(
        name,
        "__defineGetter__"
            | "__defineSetter__"
            | "__lookupGetter__"
            | "__lookupSetter__"
            | "__proto__"
            | "constructor"
            | "hasOwnProperty"
            | "isPrototypeOf"
            | "propertyIsEnumerable"
            | "toLocaleString"
            | "toString"
            | "valueOf"
    )
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

fn single_arg(call: &CallExpr) -> Option<&Expr> {
    match call.args.as_slice() {
        [arg] => Some(&arg.expr),
        _ => None,
    }
}

/// Joins tests with `op`; `None` when there are none or one is unsupported.
fn join_tests(
    tests: impl IntoIterator<Item = Option<Expr>>,
    op: fn(Expr, Expr) -> Expr,
) -> Option<Expr> {
    let mut tests = tests.into_iter();
    let first = tests.next()??;
    tests.try_fold(first, |acc, test| Some(op(acc, test?)))
}

/// ts-pattern passes either one anonymous selection or named selections.
fn mixes_anonymous_selection<'a>(
    selections: impl IntoIterator<Item = &'a SelectionBinding>,
) -> bool {
    let (mut anonymous, mut total) = (0, 0);
    for selection in selections {
        anonymous += usize::from(selection.name.is_none());
        total += 1;
    }
    anonymous > 0 && total > 1
}

fn string_chain_test(value: Expr, method: &str, call: &CallExpr) -> Option<Expr> {
    let arg = single_arg(call)?.clone();
    let length = || member_expr(value.clone(), "length");
    let test = match method {
        "startsWith" | "endsWith" | "includes" => {
            call_expr(member_expr(value.clone(), method), vec![arg])
        }
        "regex" => call_expr(member_expr(value.clone(), "match"), vec![arg]),
        "minLength" => bin(BinaryOp::GtEq, length(), arg),
        "length" => strict_eq(length(), arg),
        "maxLength" => bin(BinaryOp::LtEq, length(), arg),
        _ => return None,
    };
    Some(and(typeof_eq(value, "string"), test))
}

fn number_chain_test(
    value: Expr,
    method: &str,
    call: &CallExpr,
    ctxt: SyntaxContext,
) -> Option<Expr> {
    let compare = |op, arg: &ExprOrSpread| bin(op, value.clone(), (*arg.expr).clone());
    let number_method = |name| {
        call_expr(
            member_expr(global_ident("Number", ctxt), name),
            vec![value.clone()],
        )
    };
    let test = match (method, call.args.as_slice()) {
        ("between", [min, max]) => and(compare(BinaryOp::GtEq, min), compare(BinaryOp::LtEq, max)),
        ("lt", [arg]) => compare(BinaryOp::Lt, arg),
        ("gt", [arg]) => compare(BinaryOp::Gt, arg),
        ("lte", [arg]) => compare(BinaryOp::LtEq, arg),
        ("gte", [arg]) => compare(BinaryOp::GtEq, arg),
        ("int", _) => number_method("isInteger"),
        ("finite", _) => number_method("isFinite"),
        ("positive", _) => bin(BinaryOp::Gt, value.clone(), num_lit(0.0)),
        ("negative", _) => bin(BinaryOp::Lt, value.clone(), num_lit(0.0)),
        _ => return None,
    };
    Some(and(typeof_eq(value, "number"), test))
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

/// Returns a call whose arguments are all positional; spread arguments are
/// never lowered.
fn as_call(expr: &Expr) -> Option<&CallExpr> {
    match expr {
        Expr::Call(call) if !has_spread_arg(call) => Some(call),
        _ => None,
    }
}

fn has_spread_arg(call: &CallExpr) -> bool {
    call.args.iter().any(|arg| arg.spread.is_some())
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
            patterns: vec![Box::new(value.clone())],
            guard: arm.guard.clone(),
            handler: arm.handler.clone(),
        });
    }

    Some((prefix, stripped))
}

fn single_object_prop_with_object_value(pattern: &Expr) -> Option<(PropName, &Expr)> {
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

fn private_ident_for_index(prefix: &str, index: usize) -> Ident {
    private_ident!(DUMMY_SP, format!("{prefix}{index}"))
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

    private_ident!(DUMMY_SP, format!("{prefix}{suffix}"))
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

fn if_stmt(test: Expr, cons: Stmt, alt: Option<Stmt>) -> Stmt {
    Stmt::If(IfStmt {
        span: DUMMY_SP,
        test: Box::new(test),
        cons: Box::new(cons),
        alt: alt.map(Box::new),
    })
}

fn switch_case(test: Option<Expr>, cons: Vec<Stmt>) -> SwitchCase {
    SwitchCase {
        span: DUMMY_SP,
        test: test.map(Box::new),
        cons,
    }
}

fn switch_stmt(discriminant: Expr, cases: Vec<SwitchCase>) -> Stmt {
    Stmt::Switch(SwitchStmt {
        span: DUMMY_SP,
        discriminant: Box::new(discriminant),
        cases,
    })
}

fn break_stmt() -> Stmt {
    Stmt::Break(BreakStmt {
        span: DUMMY_SP,
        label: None,
    })
}

fn let_decl(ident: Ident) -> Stmt {
    Stmt::Decl(Decl::Var(Box::new(VarDecl {
        span: DUMMY_SP,
        ctxt: Default::default(),
        kind: VarDeclKind::Let,
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
    body.visit_with(&mut usage);
    !usage.has_mutation && usage.counts.iter().all(|count| *count == 1)
}

struct BindingUsage {
    bindings: Vec<Id>,
    counts: Vec<usize>,
    has_mutation: bool,
}

impl Visit for BindingUsage {
    fn visit_expr(&mut self, expr: &Expr) {
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

        expr.visit_children_with(self);
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

fn arrow_expr(params: Vec<Pat>, body: BlockStmtOrExpr) -> Expr {
    Expr::Arrow(ArrowExpr {
        span: DUMMY_SP,
        ctxt: Default::default(),
        params,
        body: Box::new(body),
        is_async: false,
        is_generator: false,
        type_params: None,
        return_type: None,
    })
}

fn block_stmt(stmts: Vec<Stmt>) -> BlockStmt {
    BlockStmt {
        span: DUMMY_SP,
        ctxt: Default::default(),
        stmts,
    }
}

fn num_lit(value: f64) -> Expr {
    Expr::Lit(Lit::Num(Number {
        span: DUMMY_SP,
        value,
        raw: None,
    }))
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
        _ => bin(BinaryOp::LogicalAnd, left, right),
    }
}

/// `left && right` that evaluates both operands. Callers use the result only
/// as a condition.
fn ordered_and(left: Expr, right: Expr) -> Expr {
    ordered(left, right, and, BinaryOp::BitAnd)
}

/// `left || right` that evaluates both operands. Callers use the result only
/// as a condition.
fn ordered_or(left: Expr, right: Expr) -> Expr {
    ordered(left, right, or, BinaryOp::BitOr)
}

fn ordered(left: Expr, right: Expr, op: fn(Expr, Expr) -> Expr, eager: BinaryOp) -> Expr {
    // Short-circuiting may skip only an operand whose evaluation is unobservable.
    if is_unobservable(&right) {
        return op(left, right);
    }
    let boolean = |expr| unary(UnaryOp::Bang, unary(UnaryOp::Bang, paren_expr(expr)));
    bin(eager, boolean(left), boolean(right))
}

/// Whether evaluating `expr` cannot run user code: it only reads bindings and
/// literals through operators that never coerce objects.
fn is_unobservable(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(_) | Expr::Lit(_) => true,
        Expr::Paren(paren) => is_unobservable(&paren.expr),
        Expr::Unary(unary) => {
            matches!(unary.op, UnaryOp::TypeOf | UnaryOp::Bang | UnaryOp::Void)
                && is_unobservable(&unary.arg)
        }
        Expr::Bin(bin) => {
            matches!(
                bin.op,
                BinaryOp::EqEqEq | BinaryOp::NotEqEq | BinaryOp::LogicalAnd | BinaryOp::LogicalOr
            ) && is_unobservable(&bin.left)
                && is_unobservable(&bin.right)
        }
        Expr::Cond(cond) => {
            is_unobservable(&cond.test) && is_unobservable(&cond.cons) && is_unobservable(&cond.alt)
        }
        _ => false,
    }
}

fn iterable_every(iterable: Expr, left: Pat, test: Expr) -> Expr {
    iife(vec![
        Stmt::ForOf(ForOfStmt {
            span: DUMMY_SP,
            is_await: false,
            left: ForHead::VarDecl(Box::new(VarDecl {
                span: DUMMY_SP,
                ctxt: Default::default(),
                kind: VarDeclKind::Const,
                declare: false,
                decls: vec![VarDeclarator {
                    span: DUMMY_SP,
                    name: left,
                    init: None,
                    definite: false,
                }],
            })),
            right: Box::new(iterable),
            body: Box::new(Stmt::Block(block_stmt(vec![if_stmt(
                unary(UnaryOp::Bang, test),
                return_stmt(bool_lit(false)),
                None,
            )]))),
        }),
        return_stmt(bool_lit(true)),
    ])
}

fn iife(stmts: Vec<Stmt>) -> Expr {
    call_expr(
        paren_expr(arrow_expr(
            vec![],
            BlockStmtOrExpr::BlockStmt(block_stmt(stmts)),
        )),
        vec![],
    )
}

fn or(left: Expr, right: Expr) -> Expr {
    match (as_bool_lit(&left), as_bool_lit(&right)) {
        (Some(true), _) => bool_lit(true),
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

fn seq_expr(first: Expr, second: Expr) -> Expr {
    paren_expr(Expr::Seq(SeqExpr {
        span: DUMMY_SP,
        exprs: vec![Box::new(first), Box::new(second)],
    }))
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

fn global_ident(name: &str, ctxt: SyntaxContext) -> Expr {
    Expr::Ident(Ident::new(name.into(), DUMMY_SP, ctxt))
}

fn undefined_expr(ctxt: SyntaxContext) -> Expr {
    global_ident("undefined", ctxt)
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

fn array_is_array(value: Expr, ctxt: SyntaxContext) -> Expr {
    call_expr(
        member_expr(global_ident("Array", ctxt), "isArray"),
        vec![value],
    )
}

fn reflect_own_keys(value: Expr, ctxt: SyntaxContext) -> Expr {
    call_expr(
        member_expr(global_ident("Reflect", ctxt), "ownKeys"),
        vec![value],
    )
}

struct RecordSelectionMatch<'a> {
    value: Expr,
    ctxt: SyntaxContext,
    key_ident: Ident,
    value_ident: Ident,
    key_test: Expr,
    /// The numeric form of a selecting key pattern and its test.
    coerced_key: Option<(Ident, Expr)>,
    value_test: Expr,
    key_selections: &'a [SelectionBinding],
    value_selections: &'a [SelectionBinding],
}

fn record_selection_match(
    RecordSelectionMatch {
        value,
        ctxt,
        key_ident,
        value_ident,
        key_test,
        coerced_key,
        value_test,
        key_selections,
        value_selections,
    }: RecordSelectionMatch,
) -> Expr {
    let record_value = private_ident!("_tsPatternRecordValue");
    let keys = private_ident!("_tsPatternRecordKeys");
    let selected_keys = private_ident!("_tsPatternRecordSelectedKeys");
    let key_match = private_ident!("_tsPatternRecordKeyMatch");
    let value_match = private_ident!("_tsPatternRecordValueMatch");
    let matched = private_ident!("_tsPatternRecordMatched");
    let mut value_test = value_test;
    let mut value_captures = Vec::new();
    let mut value_arrays = Vec::new();
    for (index, selection) in value_selections.iter().enumerate() {
        let capture = private_ident_for_index("_tsPatternRecordSelectedValue", index);
        let array = private_ident_for_index("_tsPatternRecordSelectedValues", index);
        let mut replacer = SelectionCapture {
            target: selection.value.clone(),
            capture: capture.clone(),
            replaced: false,
        };
        value_test.visit_mut_with(&mut replacer);
        if !replacer.replaced {
            value_test = seq_expr(
                assign_expr(capture.clone(), selection.value.clone()),
                value_test,
            );
        }
        value_captures.push(capture);
        value_arrays.push(array);
    }
    let selection = record_selection_object(
        &selected_keys,
        &value_arrays,
        key_selections,
        value_selections,
    );
    let result = record_result_object(ident_expr(&matched), selection.clone());
    let invalid_result = record_result_object(bool_lit(false), selection);
    let push = |array: &Ident, item: &Ident| {
        expr_stmt(call_expr(
            member_expr(ident_expr(array), "push"),
            vec![ident_expr(item)],
        ))
    };
    let coerced_key_match = coerced_key
        .as_ref()
        .map(|_| private_ident!("_tsPatternRecordCoercedKeyMatch"));
    let mut collect_selections = Vec::new();
    if let Some(coerced_key_match) = &coerced_key_match {
        if let Some((coerced_key_ident, _)) = &coerced_key {
            collect_selections.push(if_stmt(
                ident_expr(coerced_key_match),
                Stmt::Block(block_stmt(vec![push(&selected_keys, coerced_key_ident)])),
                None,
            ));
        }
        if !key_selections.is_empty() {
            collect_selections.push(if_stmt(
                ident_expr(&key_match),
                Stmt::Block(block_stmt(vec![push(&selected_keys, &key_ident)])),
                None,
            ));
        }
    } else if !key_selections.is_empty() {
        collect_selections.push(push(&selected_keys, &key_ident));
    }
    for (array, capture) in value_arrays.iter().zip(&value_captures) {
        collect_selections.push(push(array, capture));
    }
    let mut entry_stmts = vec![const_stmt(
        value_ident,
        computed_member_expr(ident_expr(&record_value), ident_expr(&key_ident)),
    )];
    if let (Some((coerced_key_ident, coerced_key_test)), Some(coerced_key_match)) =
        (coerced_key, &coerced_key_match)
    {
        entry_stmts.push(const_stmt(
            coerced_key_ident,
            cond_expr(
                typeof_eq(ident_expr(&key_ident), "string"),
                call_expr(global_ident("Number", ctxt), vec![ident_expr(&key_ident)]),
                null_lit(),
            ),
        ));
        entry_stmts.push(const_stmt(coerced_key_match.clone(), coerced_key_test));
    }
    entry_stmts.push(const_stmt(key_match.clone(), key_test));
    entry_stmts.push(const_stmt(value_match.clone(), value_test));
    let key_match_result = match coerced_key_match {
        Some(coerced_key_match) => or(ident_expr(&coerced_key_match), ident_expr(&key_match)),
        None => ident_expr(&key_match),
    };
    entry_stmts.push(if_stmt(
        ident_expr(&value_match),
        Stmt::Block(block_stmt(collect_selections)),
        None,
    ));
    entry_stmts.push(return_stmt(and(key_match_result, ident_expr(&value_match))));
    let matcher = call_expr(
        member_expr(ident_expr(&keys), "every"),
        vec![arrow_expr(
            vec![Pat::Ident(key_ident.into())],
            BlockStmtOrExpr::BlockStmt(block_stmt(entry_stmts)),
        )],
    );
    let empty_array = || {
        Expr::Array(ArrayLit {
            span: DUMMY_SP,
            elems: vec![],
        })
    };
    let mut setup_statements = vec![const_stmt(selected_keys, empty_array())];
    setup_statements.extend(value_captures.into_iter().map(let_decl));
    setup_statements.extend(
        value_arrays
            .into_iter()
            .map(|array| const_stmt(array, empty_array())),
    );
    setup_statements.push(if_stmt(
        unary(
            UnaryOp::Bang,
            paren_expr(and(
                and(
                    strict_ne(ident_expr(&record_value), null_lit()),
                    typeof_eq(ident_expr(&record_value), "object"),
                ),
                unary(
                    UnaryOp::Bang,
                    array_is_array(ident_expr(&record_value), ctxt),
                ),
            )),
        ),
        return_stmt(invalid_result),
        None,
    ));
    setup_statements.push(const_stmt(
        keys.clone(),
        reflect_own_keys(ident_expr(&record_value), ctxt),
    ));
    setup_statements.push(const_stmt(matched, matcher));
    setup_statements.push(return_stmt(result));
    call_expr(
        paren_expr(arrow_expr(
            vec![Pat::Ident(record_value.into())],
            BlockStmtOrExpr::BlockStmt(block_stmt(setup_statements)),
        )),
        vec![value],
    )
}

/// Each key selection collects matched keys; each value selection collects
/// its own array of values.
fn record_selection_object(
    selected_keys: &Ident,
    selected_values: &[Ident],
    key_selections: &[SelectionBinding],
    value_selections: &[SelectionBinding],
) -> Expr {
    let arrays = key_selections
        .iter()
        .map(|selection| (selection, selected_keys))
        .chain(value_selections.iter().zip(selected_values));
    let mut props = Vec::new();
    for (selection, array) in arrays {
        let Some(name) = &selection.name else {
            // An anonymous selection is the only one.
            return ident_expr(array);
        };
        props.push(PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
            key: PropName::Ident(IdentName::new(name.as_str().into(), DUMMY_SP)),
            value: Box::new(ident_expr(array)),
        }))));
    }
    Expr::Object(ObjectLit {
        span: DUMMY_SP,
        props,
    })
}

fn record_result_object(matched: Expr, selection: Expr) -> Expr {
    Expr::Object(ObjectLit {
        span: DUMMY_SP,
        props: vec![
            PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                key: PropName::Ident(IdentName::new("matched".into(), DUMMY_SP)),
                value: Box::new(matched),
            }))),
            PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                key: PropName::Ident(IdentName::new("selection".into(), DUMMY_SP)),
                value: Box::new(selection),
            }))),
        ],
    })
}

fn expr_stmt(expr: Expr) -> Stmt {
    Stmt::Expr(ExprStmt {
        span: DUMMY_SP,
        expr: Box::new(expr),
    })
}

fn instance_of(value: Expr, constructor: &str, ctxt: SyntaxContext) -> Expr {
    Expr::Bin(BinExpr {
        span: DUMMY_SP,
        op: BinaryOp::InstanceOf,
        left: Box::new(value),
        right: Box::new(global_ident(constructor, ctxt)),
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
        assert!(
            output.contains("(_tsPatternSelected0 = _tsPatternData.src)"),
            "{output}"
        );
        assert!(output.contains("return _tsPatternSelected0"), "{output}");
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
            output.contains("(_tsPatternSelected0 = _tsPatternData.src)"),
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
        assert!(output.contains("_tsPatternSelected"), "{output}");
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

        assert!(output.contains("Reflect.ownKeys"), "{output}");
        assert!(output.contains(".join"), "{output}");
        assert!(!output.contains("match(input).with"), "{output}");
    }

    #[test]
    fn prepares_statement_matches_as_constants_with_inline_handlers() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
export const run = (input: unknown) => match(input).with(P.instanceOf(Date), () => "date").otherwise(handle);"#,
        );

        assert!(output.contains("const _tsPatternValue = Date;"), "{output}");
        assert!(
            output.contains("const _tsPatternOtherwise = handle;"),
            "{output}"
        );
        assert!(output.contains("return \"date\""), "{output}");
        assert!(!output.contains(")(input"), "{output}");
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
