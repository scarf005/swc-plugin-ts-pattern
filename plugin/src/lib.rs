use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
};

use swc_core::{
    common::{errors::SourceMapper, BytePos, EqIgnoreSpan, Span, Spanned, SyntaxContext, DUMMY_SP},
    ecma::{
        ast::*,
        utils::{contains_ident_ref, prepend_stmts, private_ident},
        visit::{Visit, VisitMut, VisitMutWith, VisitWith},
    },
    plugin::{errors::HANDLER, plugin_transform, proxies::TransformPluginProgramMetadata},
};

#[plugin_transform]
pub fn process_transform(program: Program, metadata: TransformPluginProgramMetadata) -> Program {
    let on_fallback = match metadata
        .get_transform_plugin_config()
        .and_then(|config| serde_json::from_str::<serde_json::Value>(&config).ok())
        .as_ref()
        .and_then(|config| config.get("onFallback"))
    {
        None => None,
        Some(value) => match value.as_str() {
            Some("warn") => Some(false),
            Some("error") => Some(true),
            _ => {
                HANDLER.with(|handler| {
                    handler.err(r#"swc-plugin-ts-pattern: onFallback must be "warn" or "error""#)
                });
                None
            }
        },
    };
    let mut transformer = TsPatternTransformer {
        unresolved_ctxt: SyntaxContext::empty().apply_mark(metadata.unresolved_mark),
        ..Default::default()
    };
    let mut program = program;
    program.visit_mut_with(&mut transformer);
    if let Some(is_error) = on_fallback {
        for (span, reason) in transformer.fallbacks(&program) {
            let message = format!(
                "swc-plugin-ts-pattern: this match stays on the ts-pattern runtime ({reason})"
            );
            if is_error {
                HANDLER.with(|handler| handler.struct_span_err(span, &message).emit());
            } else {
                // SWC drops plugin warnings, so print them.
                let loc = metadata.source_map.lookup_char_pos(span.lo);
                eprintln!(
                    "{}:{}:{}: warning: {message}",
                    loc.file.name,
                    loc.line,
                    loc.col_display + 1
                );
            }
        }
    }
    program
}

/// Temps by selection name.
type UnionWrites = Vec<(Option<String>, Ident)>;

#[derive(Default)]
struct TsPatternTransformer {
    unresolved_ctxt: SyntaxContext,
    match_imports: HashSet<Id>,
    is_matching_imports: HashSet<Id>,
    p_imports: HashSet<Id>,
    namespaces: HashSet<Id>,
    const_bindings: HashMap<Id, Expr>,
    mutated_bindings: HashSet<Id>,
    /// `const` patterns declared below module level, which resolve only in
    /// regions that run after they are set.
    local_consts: HashSet<Id>,
    /// `const`, class and function names that nothing reassigns, with the
    /// source regions that run only after they hold their value.
    stable_bindings: HashMap<Id, Vec<Span>>,
    /// Temporaries that the current arm's tests assign; `arm_parts` declares
    /// them.
    pending_temps: RefCell<Vec<Ident>>,
    /// The temps of the `P.union` whose alternatives are being lowered, by
    /// selection name. Each matcher in an alternative writes its selections
    /// to them as soon as it matches, like ts-pattern's shared selector.
    union_writes: RefCell<Option<UnionWrites>>,
    /// Where expression temporaries can be declared: the innermost function
    /// body, or `None` where a `let` cannot reach, e.g. parameter defaults.
    hoist_scopes: Vec<Option<Vec<Ident>>>,
}

#[derive(Clone)]
struct MatchChain {
    input: Box<Expr>,
    arms: Vec<MatchArm>,
    /// Runs with the input when no arm matches: the `otherwise` handler, or
    /// for `exhaustive` and `run`, a handler that calls them on ts-pattern
    /// with no arms, so ts-pattern throws its own error or runs its handler.
    fallback: Box<Expr>,
}

#[derive(Clone)]
#[allow(clippy::vec_box)]
struct MatchArm {
    patterns: Vec<Box<Expr>>,
    guard: Option<Box<Expr>>,
    handler: Box<Expr>,
}

/// How `lower_match` handled an expression.
enum Lowered {
    /// Statements that replace the expression's statement.
    Stmts(Vec<Stmt>),
    /// The expression was lowered, or left as is, and fully visited.
    InPlace,
    /// Not a match chain; the caller visits it.
    NotMatch,
}

/// Where a statement match delivers its result.
#[derive(Clone)]
enum Sink {
    Return,
    Assign(Ident),
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
    /// `P.base.method(...)`, or `previous.method(...)` where `previous` is a
    /// chain on the same base
    Chain(&'a str, &'a str, &'a CallExpr, Option<&'a Expr>),
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
        if self.match_imports.is_empty()
            && self.is_matching_imports.is_empty()
            && self.namespaces.is_empty()
        {
            // Nothing in this module can compile.
            return;
        }
        self.collect_const_bindings(module);
        self.collect_stable_bindings(module);
        module.visit_mut_children_with(self);
    }

    fn visit_mut_block_stmt(&mut self, block: &mut BlockStmt) {
        block.stmts = self.rewrite_block_stmts(std::mem::take(&mut block.stmts));
    }

    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        expr.visit_mut_children_with(self);

        if let Some(chain) = self.parse_match_chain(expr) {
            if let Some(compiled) = self.compile_chain(chain) {
                *expr = compiled;
            }
        } else if let Some(compiled) = self.compile_is_matching(expr) {
            *expr = compiled;
        }
    }

    fn visit_mut_arrow_expr(&mut self, arrow: &mut ArrowExpr) {
        self.with_hoist_scope(false, |this| arrow.params.visit_mut_with(this));
        let temps = self.with_hoist_scope(true, |this| {
            if let BlockStmtOrExpr::Expr(expr) = &mut *arrow.body {
                match this.lower_match(expr, &Sink::Return) {
                    Lowered::Stmts(stmts) => {
                        *arrow.body = BlockStmtOrExpr::BlockStmt(block_stmt(stmts));
                        return;
                    }
                    Lowered::InPlace => return,
                    Lowered::NotMatch => {}
                }
            }
            arrow.body.visit_mut_with(this);
        });
        if temps.is_empty() {
            return;
        }
        let body = match std::mem::replace(
            &mut *arrow.body,
            BlockStmtOrExpr::BlockStmt(block_stmt(vec![])),
        ) {
            BlockStmtOrExpr::Expr(expr) => block_stmt(vec![return_stmt(*expr)]),
            BlockStmtOrExpr::BlockStmt(block) => block,
        };
        *arrow.body = BlockStmtOrExpr::BlockStmt(declare_temps(temps, body));
    }

    fn visit_mut_function(&mut self, function: &mut Function) {
        self.with_hoist_scope(false, |this| {
            function.params.visit_mut_with(this);
            function.decorators.visit_mut_with(this);
        });
        let temps = self.with_hoist_scope(true, |this| function.body.visit_mut_with(this));
        if let Some(body) = function.body.take() {
            function.body = Some(declare_temps(temps, body));
        }
    }

    fn visit_mut_constructor(&mut self, constructor: &mut Constructor) {
        self.with_hoist_scope(false, |this| constructor.params.visit_mut_with(this));
        let temps = self.with_hoist_scope(true, |this| constructor.body.visit_mut_with(this));
        if let Some(body) = constructor.body.take() {
            constructor.body = Some(declare_temps(temps, body));
        }
    }

    // Class fields, static blocks and accessors have no function body that
    // `visit_mut_function` declares temporaries in.
    fn visit_mut_class(&mut self, class: &mut Class) {
        self.with_hoist_scope(false, |this| class.visit_mut_children_with(this));
    }

    fn visit_mut_getter_prop(&mut self, getter: &mut GetterProp) {
        self.with_hoist_scope(false, |this| getter.visit_mut_children_with(this));
    }

    fn visit_mut_setter_prop(&mut self, setter: &mut SetterProp) {
        self.with_hoist_scope(false, |this| setter.visit_mut_children_with(this));
    }
}

/// Finds the `match` and `isMatching` calls of the source that stay in the
/// output, outermost chain first.
struct Fallbacks<'a> {
    transformer: &'a TsPatternTransformer,
    found: Vec<(Span, String)>,
}

impl Visit for Fallbacks<'_> {
    fn visit_expr(&mut self, expr: &Expr) {
        let transformer = self.transformer;
        let mut methods = Vec::new();
        let mut current = expr;
        while let Expr::Call(call) = current {
            if call.span.is_dummy() {
                break;
            }
            if transformer.is_imported_call(call, &transformer.is_matching_imports, "isMatching") {
                self.found.push((call.span, "isMatching".into()));
                break;
            }
            if transformer.is_imported_call(call, &transformer.match_imports, "match") {
                let reason = transformer.fallback_reason(expr, &methods);
                self.found.push((call.span, reason));
                break;
            }
            let Some((method, receiver)) = as_method_call(call) else {
                break;
            };
            methods.push((method, call.args.len()));
            current = receiver;
        }
        expr.visit_children_with(self);
    }
}

impl TsPatternTransformer {
    /// The calls this transform left for ts-pattern, with a reason code.
    fn fallbacks(&self, program: &Program) -> Vec<(Span, String)> {
        let mut visitor = Fallbacks {
            transformer: self,
            found: Vec::new(),
        };
        program.visit_with(&mut visitor);
        // An inner chain call reached again from an outer one reports once.
        let mut seen = HashSet::new();
        visitor.found.retain(|(span, _)| seen.insert(*span));
        visitor.found
    }

    /// Why the chain `expr`, whose methods are `methods` from the last, stays
    /// on the runtime.
    fn fallback_reason(&self, expr: &Expr, methods: &[(&str, usize)]) -> String {
        if let Some((method, _)) = methods.iter().find(|(method, _)| {
            !matches!(
                *method,
                "with" | "when" | "otherwise" | "exhaustive" | "run" | "returnType" | "narrow"
            )
        }) {
            return format!("method-{method}");
        }
        match methods.first() {
            Some(("exhaustive" | "run", 0) | ("otherwise", 1)) => {}
            Some(("exhaustive", 1)) if self.parse_match_chain(expr).is_some() => {}
            Some(("exhaustive", _)) => return "exhaustive-handler".into(),
            _ => return "no-terminal".into(),
        }
        match self.parse_match_chain(expr) {
            None => "unsupported-arm".into(),
            Some(chain)
                if needs_prepared_expression(self, &chain)
                    && self.prepare_chain(chain.clone()).is_none() =>
            {
                "unstable-argument".into()
            }
            Some(_) => "unsupported-pattern".into(),
        }
    }

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
                            _ => {}
                        }
                    }
                    ImportSpecifier::Namespace(namespace) => {
                        self.namespaces.insert(namespace.local.to_id());
                    }
                    ImportSpecifier::Default(_) => {}
                }
            }
        }
    }

    /// Records `const` patterns that are safe to inline.
    fn collect_const_bindings(&mut self, module: &Module) {
        let top: HashSet<Id> = module
            .body
            .iter()
            .flat_map(|item| match item {
                ModuleItem::Stmt(Stmt::Decl(Decl::Var(var)))
                | ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl {
                    decl: Decl::Var(var),
                    ..
                })) => var.decls.as_slice(),
                _ => &[],
            })
            .filter_map(|decl| decl.name.as_ident().map(|ident| ident.id.to_id()))
            .collect();
        let mut declarators = ConstDeclarators::default();
        module.visit_with(&mut declarators);
        let bindings = declarators
            .0
            .iter()
            .filter_map(|decl| match (&decl.name, &decl.init) {
                // A function is never a pattern itself; it only matters inside
                // one, e.g. `P.when(fn)`.
                (Pat::Ident(ident), Some(init))
                    if !is_function_expr(init) && self.safe_const_pattern(init) =>
                {
                    Some((ident.id.to_id(), (**init).clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if bindings.is_empty() {
            return;
        }
        self.local_consts = bindings
            .iter()
            .map(|(id, _)| id.clone())
            .filter(|id| !top.contains(id))
            .collect();
        self.const_bindings.extend(bindings);

        let mut uses = BindingUses {
            transformer: self,
            in_pattern: false,
            changed: HashSet::new(),
        };
        module.visit_with(&mut uses);
        self.mutated_bindings = uses.changed;
    }

    /// Records `const`, class and function names that nothing reassigns, with
    /// the source regions that run only after they are set.
    fn collect_stable_bindings(&mut self, module: &Module) {
        let mut writes = BindingWrites::default();
        module.visit_with(&mut writes);
        let BindingWrites {
            mut declared,
            assigned,
        } = writes;
        declared.retain(|id, _| !assigned.contains(id));
        self.stable_bindings = declared;
    }

    fn resolve_pattern_expr<'a>(&'a self, expr: &'a Expr) -> &'a Expr {
        let mut current = expr;
        for _ in 0..16 {
            current = match current {
                Expr::TsConstAssertion(assertion) => &assertion.expr,
                Expr::TsAs(assertion) => &assertion.expr,
                Expr::TsSatisfies(assertion) => &assertion.expr,
                Expr::Ident(ident) => match self.const_binding(ident) {
                    Some(next) => next,
                    None => return current,
                },
                _ => return current,
            };
        }
        current
    }

    /// The pattern an unchanged `const` holds where `ident` reads it. A
    /// `const` below module level resolves only after its declaration runs.
    fn const_binding(&self, ident: &Ident) -> Option<&Expr> {
        let id = ident.to_id();
        let ready = !self.local_consts.contains(&id) || self.in_stable_region(ident);
        (ready && !self.mutated_bindings.contains(&id))
            .then(|| self.const_bindings.get(&id))
            .flatten()
    }

    fn in_stable_region(&self, ident: &Ident) -> bool {
        self.stable_bindings
            .get(&ident.to_id())
            .is_some_and(|regions| {
                regions
                    .iter()
                    .any(|region| region.lo <= ident.span.lo && ident.span.hi <= region.hi)
            })
    }

    /// Whether evaluating `expr` earlier gives the same value and runs no code:
    /// literals, functions, unchanged constants and classes, and the read-only
    /// globals `undefined`, `NaN` and `Infinity`. Other globals can be missing
    /// or change in an earlier arm.
    fn is_stable_value(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Lit(_) | Expr::Arrow(_) | Expr::Fn(_) => true,
            Expr::Paren(paren) => self.is_stable_value(&paren.expr),
            Expr::Ident(ident) => {
                (ident.ctxt == self.unresolved_ctxt
                    && matches!(&*ident.sym, "undefined" | "NaN" | "Infinity"))
                    || self.in_stable_region(ident)
                    || self.const_binding(ident).is_some()
            }
            _ => false,
        }
    }

    fn safe_const_pattern(&self, expr: &Expr) -> bool {
        match expr {
            Expr::TsConstAssertion(assertion) => self.safe_const_pattern(&assertion.expr),
            Expr::TsAs(assertion) => self.safe_const_pattern(&assertion.expr),
            Expr::TsSatisfies(assertion) => self.safe_const_pattern(&assertion.expr),
            Expr::Lit(Lit::Regex(_)) => false,
            Expr::Lit(_) | Expr::Fn(_) => true,
            // An inlined arrow can land in another function, where `this` and
            // `arguments` differ.
            Expr::Arrow(arrow) => {
                let mut outer = OuterFunctionRefs(false);
                arrow.visit_with(&mut outer);
                !outer.0
            }
            Expr::Unary(unary) => self.safe_const_pattern(&unary.arg),
            Expr::Tpl(tpl) => tpl.exprs.is_empty(),
            Expr::Object(_) if has_ambiguous_keys(expr) => false,
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
        let (method, mut receiver) = as_method_call(call)?;
        let fallback = match (method, call.args.as_slice()) {
            ("otherwise", [_]) => None,
            ("exhaustive" | "run", []) => Some(vec![]),
            // ts-pattern reads the handler after the arms run.
            ("exhaustive", [handler])
                if handler.spread.is_none() && self.is_stable_value(&handler.expr) =>
            {
                Some(call.args.clone())
            }
            _ => return None,
        };

        // Validate the whole chain by reference before cloning any of it.
        let mut arm_calls = Vec::new();
        let (callee, input) = loop {
            let call = as_call(receiver)?;
            if self.is_imported_call(call, &self.match_imports, "match") {
                let ([input], Callee::Expr(callee)) = (call.args.as_slice(), &call.callee) else {
                    return None;
                };
                break (callee, &input.expr);
            }
            let (method, next) = as_method_call(call)?;
            // `returnType()` and `narrow()` only change types and return the matcher.
            if !(matches!(method, "returnType" | "narrow") && call.args.is_empty()) {
                arm_calls.push((method, call));
            }
            receiver = next;
        };
        if arm_calls.is_empty() {
            return None;
        }
        let arms = arm_calls
            .into_iter()
            .rev()
            .map(|(method, call)| match method {
                "with" => parse_with_arm(call),
                "when" => parse_when_arm(call),
                _ => None,
            })
            .collect::<Option<_>>()?;

        let fallback = match fallback {
            None => call.args[0].expr.clone(),
            Some(args) => {
                let value = private_ident!("_tsPatternValue");
                let runtime = call_expr(
                    member_expr(call_expr(*callee.clone(), vec![ident_expr(&value)]), method),
                    args.into_iter().map(|arg| *arg.expr).collect(),
                );
                Box::new(arrow_expr(
                    vec![Pat::Ident(value.into())],
                    BlockStmtOrExpr::Expr(Box::new(runtime)),
                ))
            }
        };
        Some(MatchChain {
            input: input.clone(),
            arms,
            fallback,
        })
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
        if has_ambiguous_keys(pattern) {
            return None;
        }
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
            fallback: constant(false),
        };
        if needs_prepared_expression(self, &chain) {
            return None;
        }
        if call.args.len() == 2 {
            return self.compile_ternary_chain(chain, true);
        }
        // The returned predicate is a function of its own.
        let mut test = None;
        let temps =
            self.with_hoist_scope(true, |this| test = this.compile_ternary_chain(chain, true));
        let body = match temps.as_slice() {
            [] => BlockStmtOrExpr::Expr(Box::new(test?)),
            _ => BlockStmtOrExpr::BlockStmt(declare_temps(
                temps,
                block_stmt(vec![return_stmt(test?)]),
            )),
        };
        Some(arrow_expr(vec![Pat::Ident(param.into())], body))
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
            if !self.has_chain_method(&member.obj, method) {
                return None;
            }
            return Some(PApi::Modifier(method, &member.obj, call));
        }
        if let Some(kind) = self.p_member(&member.obj) {
            return Some(PApi::Chain(kind, method, call, None));
        }
        match self.p_api(&member.obj)? {
            PApi::Chain(kind, ..) => Some(PApi::Chain(kind, method, call, Some(&member.obj))),
            _ => None,
        }
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
        if !self.has_chain_method(&member.obj, "select") {
            return None;
        }
        Some((call, Some(&member.obj)))
    }

    /// The name and the inner pattern of a `P.select` call or `.select()` on
    /// `base`. ts-pattern selects any value when the inner pattern is
    /// `undefined`.
    fn select_parts<'a>(
        &self,
        call: &'a CallExpr,
        base: Option<&'a Expr>,
    ) -> Option<(Option<String>, Option<&'a Expr>)> {
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
        let inner = inner.filter(
            |inner| !matches!(inner, Expr::Ident(ident) if self.is_global_undefined(ident)),
        );
        Some((name, inner))
    }

    /// Whether the runtime value of `base` has `method`: `P.when(f)` has no
    /// chain methods and `P.array(x)` has no `and` or `or`.
    fn has_chain_method(&self, base: &Expr, method: &str) -> bool {
        match self.p_api(base) {
            Some(PApi::Call("when", _)) => false,
            Some(PApi::Call("array", _)) => matches!(method, "optional" | "select"),
            _ => true,
        }
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
        let (mut stmts, chain) = self.snapshot_chain(chain)?;
        if chain.arms.iter().all(is_switchable_arm) {
            stmts.extend(self.compile_switch_stmts(chain));
        } else {
            stmts.extend(self.compile_body(
                &Sink::Return,
                &MatchInputs::new(*chain.input),
                chain.arms,
                chain.fallback,
            )?);
        }
        Some(stmts)
    }

    fn compile_chain(&mut self, chain: MatchChain) -> Option<Expr> {
        if needs_prepared_expression(self, &chain) || !can_inline_input(&chain.input) {
            return self.compile_prepared_expression(chain);
        }
        self.compile_ternary_chain(chain, true)
    }

    /// Evaluates the input and the values the chain's patterns and callbacks
    /// evaluate once, in source order, into the returned bindings, and
    /// rewrites the chain to read them. ts-pattern evaluates the arguments of
    /// an arm after the arms before it run, so after an arm that runs code,
    /// an argument that code could change or observe stays on the runtime.
    fn prepare_chain(&self, chain: MatchChain) -> Option<(Vec<(Ident, Expr)>, MatchChain)> {
        let input = private_ident!("_tsPatternInput");
        let mut params = vec![input.clone()];
        let mut args = vec![*chain.input];
        let mut arms = Vec::with_capacity(chain.arms.len());
        let mut quiet = true;
        let reorders = |quiet: bool, args: &[Expr]| {
            !quiet && args.iter().any(|arg| !self.is_stable_value(arg))
        };
        for arm in chain.arms {
            let start = args.len();
            let arm_quiet = arm.guard.is_none()
                && is_quiet_handler(&arm.handler)
                && !arm
                    .patterns
                    .iter()
                    .any(|pattern| self.runs_user_code(pattern));
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
            if reorders(quiet, &args[start..]) {
                return None;
            }
            quiet &= arm_quiet;
            arms.push(MatchArm {
                patterns,
                guard,
                handler,
            });
        }
        let start = args.len();
        let fallback = prepare_callback(
            chain.fallback,
            "_tsPatternOtherwise",
            &mut params,
            &mut args,
        );
        if reorders(quiet, &args[start..]) {
            return None;
        }
        let chain = MatchChain {
            input: Box::new(ident_expr(&input)),
            arms,
            fallback,
        };
        Some((params.into_iter().zip(args).collect(), chain))
    }

    /// Declares the input, and anything preparation evaluates, as constants.
    fn snapshot_chain(&self, chain: MatchChain) -> Option<(Vec<Stmt>, MatchChain)> {
        let (bindings, chain) = if needs_prepared_expression(self, &chain) {
            self.prepare_chain(chain)?
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
        Some((stmts, chain))
    }

    /// An expression cannot declare constants, so prepared bindings become
    /// temporaries of the enclosing function or, where none can be declared,
    /// the parameters of an immediately invoked arrow.
    fn compile_prepared_expression(&mut self, chain: MatchChain) -> Option<Expr> {
        let (bindings, chain) = self.prepare_chain(chain)?;
        if self.can_hoist() {
            // `(input = ..., value = ..., test ? ... : ...)` evaluates in the
            // same order as the arrow's arguments.
            let body = self.compile_ternary_chain(chain, true)?;
            let (temps, mut exprs): (Vec<_>, Vec<_>) = bindings
                .into_iter()
                .map(|(ident, value)| (ident.clone(), Box::new(assign_expr(ident, value))))
                .unzip();
            self.hoist(temps);
            exprs.push(Box::new(body));
            return Some(paren_expr(Expr::Seq(SeqExpr {
                span: DUMMY_SP,
                exprs,
            })));
        }
        let body = BlockStmtOrExpr::Expr(Box::new(self.compile_ternary_chain(chain, true)?));
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

    fn compile_assign_match_stmts(
        &mut self,
        target: Ident,
        chain: MatchChain,
    ) -> Option<Vec<Stmt>> {
        let (mut stmts, chain) =
            if can_inline_input(&chain.input) && !needs_prepared_expression(self, &chain) {
                (Vec::new(), chain)
            } else {
                self.snapshot_chain(chain)?
            };
        stmts.extend(self.compile_body(
            &Sink::Assign(target),
            &MatchInputs::new(*chain.input),
            chain.arms,
            chain.fallback,
        )?);
        Some(stmts)
    }

    /// Lowers arms to statements. Each arm tests the input on its own, as
    /// ts-pattern reads the input again for every arm.
    fn compile_body(
        &mut self,
        sink: &Sink,
        inputs: &MatchInputs,
        arms: Vec<MatchArm>,
        fallback: Box<Expr>,
    ) -> Option<Vec<Stmt>> {
        let mut stmts = Vec::new();
        let mut branches = Vec::with_capacity(arms.len());
        for arm in arms {
            let (test, handler_input, captures) =
                self.arm_parts(inputs.test.clone(), inputs.handler.clone(), &arm)?;
            stmts.extend(captures.into_iter().map(let_decl));
            let result = handler_result(arm.handler, vec![handler_input, inputs.handler.clone()]);
            branches.push((test, sink.deliver(result)));
        }
        let fallback = self.fallback_stmt(sink, fallback, inputs.handler.clone());
        stmts.extend(sink.if_chain(branches, fallback));
        Some(stmts)
    }

    fn fallback_stmt(&mut self, sink: &Sink, fallback: Box<Expr>, input: Expr) -> Stmt {
        sink.deliver(handler_result(fallback, vec![input]))
    }

    /// Lowers matches that initialize a declaration or are returned as
    /// statements, so their bindings need no immediately invoked arrows, and
    /// visits everything else.
    fn rewrite_block_stmts(&mut self, stmts: Vec<Stmt>) -> Vec<Stmt> {
        let mut rewritten = Vec::with_capacity(stmts.len());
        for mut stmt in stmts {
            let lowered = match &mut stmt {
                Stmt::Decl(Decl::Var(var))
                    if var.decls.len() == 1 && matches!(var.decls[0].name, Pat::Ident(_)) =>
                {
                    let Some(init) = &mut var.decls[0].init else {
                        rewritten.push(stmt);
                        continue;
                    };
                    let result = private_ident!("_tsPatternResult");
                    let lowered = self.lower_match(init, &Sink::Assign(result.clone()));
                    if let Lowered::Stmts(stmts) = lowered {
                        // Initialize the original binding only after the match
                        // succeeds. Declaring it earlier would end its TDZ and
                        // turn const into let.
                        **init = ident_expr(&result);
                        rewritten.push(let_decl(result));
                        rewritten.extend(stmts);
                        rewritten.push(stmt);
                        continue;
                    }
                    lowered
                }
                Stmt::Return(ReturnStmt { arg: Some(arg), .. }) => {
                    match self.lower_match(arg, &Sink::Return) {
                        Lowered::Stmts(stmts) => {
                            rewritten.extend(stmts);
                            continue;
                        }
                        lowered => lowered,
                    }
                }
                _ => Lowered::NotMatch,
            };
            if let Lowered::NotMatch = lowered {
                stmt.visit_mut_with(self);
            }
            rewritten.push(stmt);
        }
        rewritten
    }

    /// Lowers a match chain whose result goes to `sink`, after compiling
    /// matches nested in its arguments. Assignments keep a conditional
    /// expression when it needs no immediately invoked arrows.
    fn lower_match(&mut self, expr: &mut Expr, sink: &Sink) -> Lowered {
        if self.parse_match_chain(expr).is_none() {
            return Lowered::NotMatch;
        }
        // Nested matches compile first, so handlers that held them can inline.
        visit_chain_args(expr, self);
        let Some(chain) = self.parse_match_chain(expr) else {
            return Lowered::InPlace;
        };
        let stmts = match sink {
            Sink::Assign(target) => {
                if !needs_prepared_expression(self, &chain) {
                    if let Some(compiled) = self.compile_ternary_chain(chain.clone(), false) {
                        *expr = compiled;
                        return Lowered::InPlace;
                    }
                }
                self.compile_assign_match_stmts(target.clone(), chain.clone())
            }
            Sink::Return => self.compile_arrow_match_stmts(chain.clone()),
        };
        if let Some(stmts) = stmts {
            return Lowered::Stmts(stmts);
        }
        // Fall back to an expression, which may need arrows.
        if let Some(compiled) = self.compile_chain(chain) {
            *expr = compiled;
        }
        Lowered::InPlace
    }

    /// Runs `visit` in a new declaration scope and returns the temporaries
    /// hoisted into it.
    fn with_hoist_scope(&mut self, declarable: bool, visit: impl FnOnce(&mut Self)) -> Vec<Ident> {
        self.hoist_scopes.push(declarable.then(Vec::new));
        visit(self);
        self.hoist_scopes.pop().flatten().unwrap_or_default()
    }

    fn can_hoist(&self) -> bool {
        matches!(self.hoist_scopes.last(), Some(Some(_)))
    }

    fn hoist(&mut self, temps: Vec<Ident>) {
        if let Some(Some(scope)) = self.hoist_scopes.last_mut() {
            scope.extend(temps);
        }
    }

    /// Lowers a chain to nested conditionals. Arms that bind captures declare
    /// them in the enclosing function or, with `allow_arrows`, in an
    /// immediately invoked arrow; otherwise lowering fails.
    fn compile_ternary_chain(&mut self, chain: MatchChain, allow_arrows: bool) -> Option<Expr> {
        if !can_inline_input(&chain.input) {
            return None;
        }

        let input = *chain.input;
        let mut expression = handler_result(chain.fallback, vec![input.clone()]);
        // Each function activation has its own temporaries, so re-entrant
        // matches cannot overwrite them.
        let hoist = allow_arrows && self.can_hoist();
        let mut hoisted = Vec::new();

        for arm in chain.arms.into_iter().rev() {
            let (test, handler_input, captures) =
                self.arm_parts(input.clone(), input.clone(), &arm)?;
            let branch = cond_expr(
                test,
                handler_result(arm.handler, vec![handler_input, input.clone()]),
                expression,
            );
            expression = if captures.is_empty() {
                branch
            } else if hoist {
                hoisted.extend(captures);
                branch
            } else if !allow_arrows {
                return None;
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

        self.hoist(hoisted);
        Some(expression)
    }

    fn arm_parts(
        &self,
        match_input: Expr,
        guard_input: Expr,
        arm: &MatchArm,
    ) -> Option<(Expr, Expr, Vec<Ident>)> {
        // Drop temporaries left by an attempt that failed to lower.
        self.pending_temps.take();
        let mut selections = Vec::new();
        let pattern_test =
            if arm.patterns.len() == 1 && matches!(*arm.patterns[0], Expr::Invalid(_)) {
                bool_lit(true)
            } else if arm.patterns.len() == 1 {
                self.pattern_test_with_selections(
                    match_input.clone(),
                    &arm.patterns[0],
                    &mut selections,
                )?
            } else {
                let mut patterns = Vec::with_capacity(arm.patterns.len());
                for pattern in &arm.patterns {
                    let mut pattern_selections = Vec::new();
                    let test = self.pattern_test_with_selections(
                        match_input.clone(),
                        pattern,
                        &mut pattern_selections,
                    )?;
                    patterns.push((test, pattern_selections));
                }
                if patterns
                    .iter()
                    .any(|(_, selections)| !selections.is_empty())
                {
                    return self.multi_pattern_parts(match_input, guard_input, arm, patterns);
                }
                join_tests(patterns.into_iter().map(|(test, _)| Some(test)), or)?
            };

        // A record that selects nothing makes no selections, so the handler
        // gets the input; this works only when the record makes every
        // selection of the arm, and only when no other pattern, such as
        // `P.optional`, wraps the record result.
        let record_group = selections
            .first()
            .and_then(|selection| selection.capture_group.clone());
        if selections
            .iter()
            .any(|selection| selection.capture_group.is_some())
            && selections.iter().any(|selection| {
                selection.capture_group != record_group || !matches!(selection.value, Expr::Call(_))
            })
        {
            return None;
        }
        let mut test = pattern_test;
        let mut captures = self.pending_temps.take();
        let mut grouped_captures: Vec<(Id, Ident)> = Vec::new();
        let mut handler_selections = Vec::with_capacity(selections.len());
        for selection in selections {
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
                let capture = private_ident!("_tsPatternSelected");
                let mut replacer = ReadOnce::new(selection.value.clone(), capture.clone(), true);
                test.visit_mut_with(&mut replacer);
                if !replacer.found {
                    // ts-pattern reads a selection only after the type tests
                    // before it pass.
                    test = and(
                        test,
                        seq_expr(
                            assign_expr(capture.clone(), selection.value),
                            bool_lit(true),
                        ),
                    );
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
        if let Some(guard) = &arm.guard {
            test = and(test, predicate_result(guard, guard_input));
        }

        let mut handler_input = selection_handler_input(match_input.clone(), handler_selections)?;
        if let Some((_, capture)) = grouped_captures.first() {
            handler_input = cond_expr(
                member_expr(ident_expr(capture), "selected"),
                handler_input,
                match_input,
            );
        }
        Some((test, handler_input, captures))
    }

    fn multi_pattern_parts(
        &self,
        match_input: Expr,
        guard_input: Expr,
        arm: &MatchArm,
        patterns: Vec<(Expr, Vec<SelectionBinding>)>,
    ) -> Option<(Expr, Expr, Vec<Ident>)> {
        // The patterns share one selection object, so names that a failed
        // pattern wrote first come first.
        let names = patterns
            .iter()
            .flat_map(|(_, selections)| selections.iter().map(|selection| &selection.name))
            .collect::<HashSet<_>>();
        if names.len() > 1 {
            return None;
        }
        let (mut test, handler_selections) = self.alternatives_test(patterns)?;
        if let Some(guard) = &arm.guard {
            test = and(test, predicate_result(guard, guard_input));
        }
        let handler_input = selection_handler_input(match_input, handler_selections)?;
        Some((test, handler_input, self.pending_temps.take()))
    }

    /// Tests each of `patterns` in turn until one matches, like `P.union`
    /// and several patterns in one `.with`.
    fn union_test<'a>(
        &self,
        value: Expr,
        patterns: impl IntoIterator<Item = &'a Expr>,
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        let patterns = patterns.into_iter().collect::<Vec<_>>();
        let lower = || {
            patterns
                .iter()
                .map(|pattern| {
                    let mut selected = Vec::new();
                    let test =
                        self.pattern_test_with_selections(value.clone(), pattern, &mut selected)?;
                    Some((test, selected))
                })
                .collect::<Option<Vec<_>>>()
        };
        let (alternatives, temps) = self.with_local_temps(lower)?;
        // ts-pattern sets the names to `undefined` in `getSelectionKeys`
        // order first, so the selection object keeps that key order.
        let mut order = Vec::new();
        for pattern in &patterns {
            self.selection_keys(pattern, &mut order);
        }
        let position = |name: &Option<String>| order.iter().position(|key| key == name);
        let mut names = Vec::new();
        for (_, selected) in &alternatives {
            for selection in selected {
                position(&selection.name)?;
                if !names.contains(&selection.name) {
                    names.push(selection.name.clone());
                }
            }
        }
        names.sort_by_key(position);
        if let Some((test, mut selected)) = self.alternatives_test(alternatives) {
            selected.sort_by_key(|selection| position(&selection.name));
            self.pending_temps.borrow_mut().extend(temps);
            selections.extend(selected);
            return Some(test);
        }
        // ts-pattern sets every name of every alternative to `undefined`
        // first. A name keeps the last value written to it, also by an
        // alternative that failed later.
        if names.contains(&None) && names.len() > 1 {
            return None;
        }
        let writes = names
            .into_iter()
            .map(|name| (name, private_ident!("_tsPatternSelected")))
            .collect::<Vec<_>>();
        let outer = self.union_writes.replace(Some(writes.clone()));
        let tests = lower();
        self.union_writes.replace(outer);
        let tests = tests?
            .into_iter()
            .map(|(test, selected)| selected.is_empty().then_some(test))
            .collect::<Option<Vec<_>>>()?;
        let test = writes.iter().rev().fold(
            join_tests(tests.into_iter().map(Some), or)?,
            |test, (_, temp)| {
                seq_expr(
                    assign_expr(temp.clone(), undefined_expr(self.unresolved_ctxt)),
                    test,
                )
            },
        );
        self.pending_temps
            .borrow_mut()
            .extend(writes.iter().map(|(_, temp)| temp.clone()));
        selections.extend(writes.into_iter().map(|(name, temp)| SelectionBinding {
            name,
            value: ident_expr(&temp),
            ..Default::default()
        }));
        Some(test)
    }

    /// ts-pattern tries the patterns in order with one shared selection
    /// object, so each name takes its value from the pattern that matched.
    /// Patterns that select different names give `None`, because a pattern
    /// that failed can leave names that the matched pattern lacks;
    /// `union_test` writes those through `union_writes`. Returns the joined
    /// test and selections that read capture temps, which go to
    /// `pending_temps`.
    fn alternatives_test(
        &self,
        patterns: Vec<(Expr, Vec<SelectionBinding>)>,
    ) -> Option<(Expr, Vec<SelectionBinding>)> {
        let mut names: Vec<Option<String>> = Vec::new();
        for selection in &patterns.first()?.1 {
            if !names.contains(&selection.name) {
                names.push(selection.name.clone());
            }
        }
        if patterns.iter().any(|(_, selections)| {
            selections.iter().any(|selection| {
                selection.capture_group.is_some() || !names.contains(&selection.name)
            }) || names
                .iter()
                .any(|name| !selections.iter().any(|selection| &selection.name == name))
        }) {
            return None;
        }
        let named_captures = names
            .iter()
            .map(|name| (name.clone(), private_ident!("_tsPatternSelected")))
            .collect::<Vec<_>>();
        let capture_for = |name: &Option<String>| {
            named_captures
                .iter()
                .find(|(capture_name, _)| capture_name == name)
                .map(|(_, capture)| capture.clone())
        };
        let mut tests = Vec::with_capacity(patterns.len());
        for (mut test, selections) in patterns {
            for selection in selections {
                let capture = capture_for(&selection.name)?;
                let mut replacer = ReadOnce::new(selection.value.clone(), capture.clone(), true);
                // A temp holds its final value only after the whole test,
                // e.g. the items that `P.array` collects.
                if !matches!(selection.value, Expr::Ident(_)) {
                    test.visit_mut_with(&mut replacer);
                }
                if !replacer.found {
                    test = and(
                        test,
                        seq_expr(assign_expr(capture, selection.value), bool_lit(true)),
                    );
                }
            }
            tests.push(Some(test));
        }
        let test = join_tests(tests, or)?;
        let mut temps = self.pending_temps.borrow_mut();
        let selections = named_captures
            .into_iter()
            .map(|(name, capture)| {
                temps.push(capture.clone());
                SelectionBinding {
                    name,
                    value: ident_expr(&capture),
                    ..Default::default()
                }
            })
            .collect();
        Some((test, selections))
    }

    /// Adds the selection names of `pattern` to `keys` in the order of
    /// ts-pattern's `getSelectionKeys`.
    fn selection_keys(&self, pattern: &Expr, keys: &mut Vec<Option<String>>) {
        let pattern = self.resolve_pattern_expr(pattern);
        if let Some((call, base)) = self.select_call(pattern) {
            if let Some((name, inner)) = self.select_parts(call, base) {
                keys.push(name);
                if let Some(inner) = inner {
                    self.selection_keys(inner, keys);
                }
            }
            return;
        }
        let patterns: Vec<&Expr> = match (self.p_api(pattern), pattern) {
            (Some(PApi::Call("not" | "shape" | "when", _) | PApi::Chain(..)), _) => vec![],
            (Some(PApi::Call(_, call)), _) => call.args.iter().map(|arg| &*arg.expr).collect(),
            (Some(PApi::Modifier(_, base, call)), _) => [base]
                .into_iter()
                .chain(call.args.iter().map(|arg| &*arg.expr))
                .collect(),
            (None, Expr::Object(object)) => own_keys_order(object)
                .into_iter()
                .filter_map(|prop| Some(&*prop.as_prop()?.as_key_value()?.value))
                .collect(),
            (None, Expr::Array(array)) => array
                .elems
                .iter()
                .flatten()
                .map(|elem| &*elem.expr)
                .collect(),
            _ => vec![],
        };
        for pattern in patterns {
            self.selection_keys(pattern, keys);
        }
    }

    /// Whether `selections` come in the `getSelectionKeys` order of
    /// `pattern`. `P.optional` and `P.array` set the names to `undefined` in
    /// that order when they match nothing, e.g. for `undefined` or `[]`.
    fn in_selection_key_order(&self, pattern: &Expr, selections: &[SelectionBinding]) -> bool {
        let mut keys = Vec::new();
        self.selection_keys(pattern, &mut keys);
        // A repeated name keeps the place where it first appears.
        let mut names = Vec::new();
        for selection in selections {
            if !names.contains(&&selection.name) {
                names.push(&selection.name);
            }
        }
        names
            .into_iter()
            .map(|name| keys.iter().position(|key| key == name))
            .collect::<Option<Vec<_>>>()
            .is_some_and(|positions| positions.is_sorted())
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
        let writes = self.union_writes.borrow().clone();
        // Object and array literals pass the selector on to their parts.
        let Some(writes) = writes.filter(|_| {
            !matches!(
                self.resolve_pattern_expr(pattern),
                Expr::Object(_) | Expr::Array(_)
            )
        }) else {
            return self.lower_pattern(value, pattern, selections);
        };
        // A matcher collects its own selections and passes them on only when
        // it matches.
        let start = selections.len();
        self.union_writes.replace(None);
        let test = self.lower_pattern(value, pattern, selections);
        self.union_writes.replace(Some(writes.clone()));
        let mut test = test?;
        for selection in selections.drain(start..) {
            let (_, temp) = writes
                .iter()
                .find(|(name, _)| name == &selection.name)
                .filter(|_| selection.capture_group.is_none())?;
            test = and(
                test,
                seq_expr(assign_expr(temp.clone(), selection.value), bool_lit(true)),
            );
        }
        Some(test)
    }

    fn lower_pattern(
        &self,
        value: Expr,
        pattern: &Expr,
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        let pattern = self.resolve_pattern_expr(pattern);

        if let Some((call, base)) = self.select_call(pattern) {
            let (name, inner) = self.select_parts(call, base)?;
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

        // ts-pattern compares literals with Object.is: -0 !== 0 and NaN matches NaN.
        let is_nan = matches!(pattern, Expr::Ident(ident) if ident.sym == "NaN" && ident.ctxt == self.unresolved_ctxt);
        if is_zero_literal(pattern) || is_nan {
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
            Expr::Ident(ident) if self.is_global_undefined(ident) => {
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
            PApi::Member("_" | "any") => Some(bool_lit(true)),
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
            // `P.shape(p)` is `P.when(isMatching(p))`, which drops selections.
            PApi::Call("shape", call) => self.unselected_test(value, single_arg(call)?),
            PApi::Call("not", call) => Some(unary(
                UnaryOp::Bang,
                self.unselected_test(value, single_arg(call)?)?,
            )),
            PApi::Call("union", call) => {
                self.union_test(value, call.args.iter().map(|arg| &*arg.expr), selections)
            }
            // ts-pattern tests every pattern in order and keeps their
            // selections only when all match, like `&&` with outer selections.
            PApi::Call("intersection", call) => join_tests(
                call.args.iter().map(|arg| {
                    self.pattern_test_with_selections(value.clone(), &arg.expr, selections)
                }),
                and,
            ),
            PApi::Call("array", call) => self.array_of_test(value, call, selections),
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
            PApi::Modifier("and", base, call) => Some(and(
                self.pattern_test_with_selections(value.clone(), base, selections)?,
                self.pattern_test_with_selections(value, single_arg(call)?, selections)?,
            )),
            PApi::Modifier("or", base, call) => {
                self.union_test(value, [base, single_arg(call)?], selections)
            }
            PApi::Chain(kind, method, call, previous) => {
                let test = match kind {
                    "string" => string_chain_test(value.clone(), method, call),
                    "number" => number_chain_test(value.clone(), method, call, ctxt),
                    "bigint" => bigint_chain_test(value.clone(), method, call),
                    _ => None,
                }?;
                // ts-pattern chains a refinement as `P.intersection(previous, next)`.
                match previous {
                    Some(previous) => Some(and(self.unselected_test(value, previous)?, test)),
                    None => Some(test),
                }
            }
            _ => None,
        }
    }

    /// How many leading arguments of `call` are patterns: those of a `.with`
    /// arm before its handler, of `P` APIs, and `isMatching`'s first.
    fn pattern_arg_count(&self, call: &CallExpr) -> Option<usize> {
        if self.is_imported_call(call, &self.is_matching_imports, "isMatching") {
            return Some(1);
        }
        if let Some(("with", receiver)) = as_method_call(call) {
            return self
                .is_match_chain(receiver)
                .then(|| call.args.len().saturating_sub(1));
        }
        match self.p_call_api(call)? {
            PApi::Modifier(method, base, _) if !self.has_pattern_method(base, method) => None,
            _ => Some(call.args.len()),
        }
    }

    /// Whether `expr` is `match(...)` followed by arm calls.
    fn is_match_chain(&self, mut expr: &Expr) -> bool {
        while let Some(call) = as_call(expr) {
            if self.is_imported_call(call, &self.match_imports, "match") {
                return true;
            }
            let Some((_, receiver)) = as_method_call(call) else {
                return false;
            };
            expr = receiver;
        }
        false
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
            Some(PApi::Chain("string" | "number" | "bigint", ..)) => CHAINABLE,
            Some(PApi::Modifier("optional", base, _)) if self.has_pattern_method(base, "and") => {
                CHAINABLE
            }
            Some(PApi::Modifier("optional", _, _)) => ARRAY_CHAINABLE,
            Some(PApi::Modifier(..)) => CHAINABLE,
            _ => &[],
        };
        methods.contains(&method)
    }

    fn is_global_undefined(&self, ident: &Ident) -> bool {
        ident.sym == "undefined" && ident.ctxt == self.unresolved_ctxt
    }

    /// Whether `pattern` is a type test that no number can pass; testing a
    /// number against it runs no user code.
    fn rejects_numbers(&self, pattern: &Expr) -> bool {
        let pattern = self.resolve_pattern_expr(pattern);
        if let Some((call, base)) = self.select_call(pattern) {
            // A selection matches what its inner pattern matches.
            return self
                .select_parts(call, base)
                .and_then(|(_, inner)| inner)
                .is_some_and(|inner| self.rejects_numbers(inner));
        }
        match self.p_api(pattern) {
            Some(PApi::Member("string" | "symbol" | "boolean" | "bigint" | "nullish")) => true,
            Some(PApi::Chain("string", method, ..)) => {
                matches!(
                    method,
                    "startsWith" | "endsWith" | "includes" | "minLength" | "length" | "maxLength"
                )
            }
            Some(PApi::Call("union", call)) => {
                call.args.iter().all(|arg| self.rejects_numbers(&arg.expr))
            }
            Some(_) => false,
            None => match pattern {
                Expr::Lit(Lit::Str(_) | Lit::Bool(_) | Lit::Null(_) | Lit::BigInt(_)) => true,
                Expr::Ident(ident) => self.is_global_undefined(ident),
                _ => false,
            },
        }
    }

    /// Lowers a test that runs inside a callback, returning the temporaries
    /// it assigns so the callback declares them locally.
    fn with_local_temps<T>(&self, lower: impl FnOnce() -> Option<T>) -> Option<(T, Vec<Ident>)> {
        let outer = self.pending_temps.take();
        let lowered = lower();
        let local = self.pending_temps.replace(outer);
        Some((lowered?, local))
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
        if !self.in_selection_key_order(pattern, &inner_selections) {
            return None;
        }
        selections.extend(
            inner_selections
                .into_iter()
                .map(|selection| SelectionBinding {
                    // Selecting the value itself already gives `undefined`.
                    value: if selection.value.eq_ignore_span(&value) {
                        selection.value
                    } else {
                        cond_expr(
                            strict_eq(value.clone(), undefined.clone()),
                            undefined.clone(),
                            selection.value,
                        )
                    },
                    ..selection
                }),
        );
        Some(or(strict_eq(value, undefined), test))
    }

    /// ts-pattern collects each selection of the items into an array, which
    /// is empty for an empty array.
    fn array_of_test(
        &self,
        value: Expr,
        call: &CallExpr,
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        let array_test = array_is_array(value.clone(), self.unresolved_ctxt);
        let [item] = call.args.as_slice() else {
            return call.args.is_empty().then_some(array_test);
        };
        let item_ident = private_ident!("_tsPatternItem");
        let ((mut item_test, item_selections), mut temps) = self.with_local_temps(|| {
            let mut item_selections = Vec::new();
            let test = self.pattern_test_with_selections(
                ident_expr(&item_ident),
                &item.expr,
                &mut item_selections,
            )?;
            Some((test, item_selections))
        })?;
        if !self.in_selection_key_order(&item.expr, &item_selections) {
            return None;
        }
        // A name that one item selects twice gets both values; that stays on
        // the runtime, as do nested record results.
        let mut arrays: Vec<(Option<String>, Ident)> = Vec::new();
        let mut pushes = Vec::new();
        for selection in item_selections {
            if selection.capture_group.is_some()
                || arrays.iter().any(|(name, _)| name == &selection.name)
            {
                return None;
            }
            let array = private_ident!("_tsPatternSelectedItems");
            let item_value = if selection_needs_capture(&selection.value) {
                let capture = private_ident!("_tsPatternSelected");
                let mut replacer = ReadOnce::new(selection.value.clone(), capture.clone(), true);
                item_test.visit_mut_with(&mut replacer);
                if !replacer.found {
                    item_test = and(
                        item_test,
                        seq_expr(
                            assign_expr(capture.clone(), selection.value),
                            bool_lit(true),
                        ),
                    );
                }
                temps.push(capture.clone());
                ident_expr(&capture)
            } else {
                selection.value
            };
            pushes.push(call_expr(
                member_expr(ident_expr(&array), "push"),
                vec![item_value],
            ));
            arrays.push((selection.name, array));
        }
        if let Some(pushed) = pushes.into_iter().reduce(seq_expr) {
            item_test = and(item_test, seq_expr(pushed, bool_lit(true)));
        }
        let body = if temps.is_empty() {
            BlockStmtOrExpr::Expr(Box::new(item_test))
        } else {
            let mut stmts = temps.into_iter().map(let_decl).collect::<Vec<_>>();
            stmts.push(return_stmt(item_test));
            BlockStmtOrExpr::BlockStmt(block_stmt(stmts))
        };
        // ts-pattern reads `length`, and calls `every` only on a non-empty
        // array.
        let items_test = or(
            strict_eq(member_expr(value.clone(), "length"), num_lit(0.0)),
            call_expr(
                member_expr(value, "every"),
                vec![arrow_expr(vec![Pat::Ident(item_ident.into())], body)],
            ),
        );
        let items_test = arrays.iter().rev().fold(items_test, |test, (_, array)| {
            let empty = Expr::Array(ArrayLit {
                span: DUMMY_SP,
                elems: vec![],
            });
            seq_expr(assign_expr(array.clone(), empty), test)
        });
        for (name, array) in arrays {
            selections.push(SelectionBinding {
                name,
                value: ident_expr(&array),
                ..Default::default()
            });
            self.pending_temps.borrow_mut().push(array);
        }
        Some(and(array_test, items_test))
    }

    fn set_test(&self, value: Expr, call: &CallExpr) -> Option<Expr> {
        let set_test = instance_of(value.clone(), "Set", self.unresolved_ctxt);
        match call.args.as_slice() {
            [] => Some(set_test),
            [item] => {
                let item_ident = private_ident!("_tsPatternItem");
                let (item_test, temps) = self.with_local_temps(|| {
                    self.unselected_test(ident_expr(&item_ident), &item.expr)
                })?;
                Some(and(
                    set_test,
                    iterable_every(value, Pat::Ident(item_ident.into()), item_test, temps),
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
        let (test, temps) = self.with_local_temps(|| {
            let key_test = self.unselected_test(ident_expr(&key_ident), &key.expr)?;
            let value_test = self.unselected_test(ident_expr(&value_ident), &entry.expr)?;
            Some(ordered_and(key_test, value_test))
        })?;
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
                test,
                temps,
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
        let ((key_test, coerced_key, key_selections, value_test, value_selections), temps) =
            self.with_local_temps(|| {
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
                        if self.rejects_numbers(key_pattern) {
                            // The numeric form of a key can never match.
                            direct_key_test
                        } else if key_selections.is_empty() {
                            let coerced = call_expr(
                                global_ident("Number", ctxt),
                                vec![ident_expr(&key_ident)],
                            );
                            ordered_or(
                                numeric_key_test(
                                    coerced.clone(),
                                    self.pattern_test(coerced, key_pattern)?,
                                ),
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
                Some((
                    key_test,
                    coerced_key,
                    key_selections,
                    value_test,
                    value_selections,
                ))
            })?;
        let all_selections = key_selections
            .iter()
            .chain(value_selections.iter())
            .collect::<Vec<_>>();
        // A nested record forwards its selections only for entries that
        // selected, so it must make all selections of the record.
        let group = value_selections
            .first()
            .and_then(|selection| selection.capture_group.clone());
        if all_selections
            .iter()
            .any(|selection| selection.capture_group.is_some())
            && (group.is_none()
                || !key_selections.is_empty()
                || !matches!(self.p_api(value_pattern), Some(PApi::Call("record", _)))
                || value_selections
                    .iter()
                    .any(|selection| selection.capture_group != group))
        {
            return None;
        }
        // Each key selection collects the key, so a key that a `P.union`
        // selects in part stays on the runtime. A name that the key and the
        // value both select collects both into one array.
        if key_selections.iter().any(|selection| {
            !matches!(&selection.value, Expr::Ident(ident) if ident.to_id() == key_ident.to_id())
                || value_selections
                    .iter()
                    .any(|value| value.name == selection.name)
        }) || all_selections.iter().any(|selection| {
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
                        BlockStmtOrExpr::BlockStmt(block_stmt(
                            temps
                                .into_iter()
                                .map(let_decl)
                                .chain([
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
                                ])
                                .collect(),
                        )),
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
            temps,
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
        own_keys_order(object)
            .into_iter()
            .try_fold(base, |acc, prop| {
                let PropOrSpread::Prop(prop) = prop else {
                    return None;
                };

                let Prop::KeyValue(key_value) = &**prop else {
                    return None;
                };

                let prop_access = prop_access(value.clone(), &key_value.key)?;
                let first_selection = selections.len();
                let test = self.pattern_test_with_selections(
                    prop_access.clone(),
                    &key_value.value,
                    selections,
                )?;
                let test = self.read_property(
                    prop_access,
                    &key_value.key,
                    test,
                    &mut selections[first_selection..],
                );
                // ts-pattern checks `key in value` before it reads an optional key too.
                let in_object = prop_in_object(&key_value.key, value.clone())?;
                let test = if self.is_optional_pattern(&key_value.value) {
                    seq_expr(in_object, test)
                } else {
                    and(in_object, test)
                };
                Some(and(acc, test))
            })
    }

    /// Makes `test` and the selections it added read `access` once, first.
    /// ts-pattern reads each property once per arm before it tests the
    /// value, even when the pattern ignores the value.
    fn read_property(
        &self,
        access: Expr,
        key: &PropName,
        mut test: Expr,
        selections: &mut [SelectionBinding],
    ) -> Expr {
        let mut reads = CountReads {
            target: &access,
            count: 0,
        };
        test.visit_with(&mut reads);
        let test_reads = reads.count;
        for selection in selections.iter() {
            selection.value.visit_with(&mut reads);
        }
        if reads.count == 0 {
            return seq_expr(access, test);
        }
        if reads.count == 1 && test_reads == 1 && starts_with_read(&test, &access) {
            return test;
        }
        let temp = private_ident_for_prop("_tsPattern", key);
        test.visit_mut_with(&mut ReadOnce::new(access.clone(), temp.clone(), false));
        for selection in selections {
            selection
                .value
                .visit_mut_with(&mut ReadOnce::new(access.clone(), temp.clone(), false));
        }
        self.pending_temps.borrow_mut().push(temp.clone());
        seq_expr(assign_expr(temp, access), test)
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
        let elems = array
            .elems
            .iter()
            .map(Option::as_ref)
            .collect::<Option<Vec<_>>>()?;
        // Any iteration of a `P.array` pattern, also in another module, makes
        // it variadic, so only a fresh `P.array(...)` is a single element.
        if elems.iter().any(|elem| {
            elem.spread.is_none()
                && matches!(&*elem.expr, Expr::Ident(_))
                && matches!(self.p_api(&elem.expr), Some(PApi::Call("array", _)))
        }) {
            return None;
        }
        let spreads = elems.iter().filter(|elem| elem.spread.is_some()).count();
        let Some(middle) = elems.iter().position(|elem| elem.spread.is_some()) else {
            let base = and(
                array_is_array(value.clone(), self.unresolved_ctxt),
                strict_eq(
                    member_expr(value.clone(), "length"),
                    num_lit(elems.len() as f64),
                ),
            );
            return self.elements_test(base, &value, &elems, selections);
        };
        // `...P.array(pattern)` and its `.select()` iterate to a variadic
        // pattern. ts-pattern throws on several of them.
        let rest = &elems[middle].expr;
        let is_array = |pattern: &Expr| matches!(self.p_api(pattern), Some(PApi::Call("array", _)));
        let variadic = matches!(&**rest, Expr::Call(_))
            && match self.select_call(rest) {
                Some((_, base)) => base.is_some_and(is_array),
                None => is_array(rest),
            };
        if spreads > 1 || !variadic {
            return None;
        }
        // Like ts-pattern: compare `length`, then slice the head, the tail
        // and the rest, then test them in that order.
        let (head, tail) = (&elems[..middle], &elems[middle + 1..]);
        let [head_ident, tail_ident, rest_ident] = [
            private_ident!("_tsPatternHead"),
            private_ident!("_tsPatternTail"),
            private_ident!("_tsPatternRest"),
        ];
        self.pending_temps.borrow_mut().extend([
            head_ident.clone(),
            tail_ident.clone(),
            rest_ident.clone(),
        ]);
        let slice = |args| call_expr(member_expr(value.clone(), "slice"), args);
        let tail_len = || unary(UnaryOp::Minus, num_lit(tail.len() as f64));
        let slices = [
            assign_expr(
                head_ident.clone(),
                slice(vec![num_lit(0.0), num_lit(head.len() as f64)]),
            ),
            assign_expr(
                tail_ident.clone(),
                if tail.is_empty() {
                    Expr::Array(ArrayLit {
                        span: DUMMY_SP,
                        elems: vec![],
                    })
                } else {
                    slice(vec![tail_len()])
                },
            ),
            assign_expr(
                rest_ident.clone(),
                slice(vec![
                    num_lit(head.len() as f64),
                    if tail.is_empty() {
                        Expr::Ident(Ident::new(
                            "Infinity".into(),
                            DUMMY_SP,
                            self.unresolved_ctxt,
                        ))
                    } else {
                        tail_len()
                    },
                ]),
            ),
        ]
        .into_iter()
        .rev()
        .fold(bool_lit(true), |rest, read| seq_expr(read, rest));
        let base = and(
            and(
                array_is_array(value.clone(), self.unresolved_ctxt),
                unary(
                    UnaryOp::Bang,
                    paren_expr(bin(
                        BinaryOp::Lt,
                        member_expr(value.clone(), "length"),
                        num_lit((head.len() + tail.len()) as f64),
                    )),
                ),
            ),
            slices,
        );
        let base = self.elements_test(base, &ident_expr(&head_ident), head, selections)?;
        let base = self.elements_test(base, &ident_expr(&tail_ident), tail, selections)?;
        Some(and(
            base,
            self.pattern_test_with_selections(ident_expr(&rest_ident), rest, selections)?,
        ))
    }

    /// Adds a test of each `elems` pattern against the same index of `value`.
    fn elements_test(
        &self,
        base: Expr,
        value: &Expr,
        elems: &[&ExprOrSpread],
        selections: &mut Vec<SelectionBinding>,
    ) -> Option<Expr> {
        elems
            .iter()
            .enumerate()
            .try_fold(base, |acc, (index, elem)| {
                if elem.spread.is_some() {
                    return None;
                }
                let index = PropName::Num(Number {
                    span: DUMMY_SP,
                    value: index as f64,
                    raw: None,
                });
                let item = prop_access(value.clone(), &index)?;
                let first_selection = selections.len();
                let test =
                    self.pattern_test_with_selections(item.clone(), &elem.expr, selections)?;
                let test =
                    self.read_property(item, &index, test, &mut selections[first_selection..]);
                Some(and(acc, test))
            })
    }
}

/// Finds const bindings whose value may change after initialization: any
/// assigned through, and object or array literals referenced anywhere but in a
/// pattern argument, where a callee could mutate them.
struct BindingUses<'a> {
    transformer: &'a TsPatternTransformer,
    in_pattern: bool,
    changed: HashSet<Id>,
}

impl Visit for BindingUses<'_> {
    fn visit_module_decl(&mut self, decl: &ModuleDecl) {
        // Importers could mutate exported literals.
        match decl {
            ModuleDecl::ExportDecl(ExportDecl {
                decl: Decl::Var(var),
                ..
            }) => {
                for decl in &var.decls {
                    if let Pat::Ident(ident) = &decl.name {
                        self.escape(&ident.id);
                    }
                }
            }
            ModuleDecl::ExportNamed(NamedExport {
                src: None,
                specifiers,
                ..
            }) => {
                for specifier in specifiers {
                    if let ExportSpecifier::Named(ExportNamedSpecifier {
                        orig: ModuleExportName::Ident(ident),
                        ..
                    }) = specifier
                    {
                        self.escape(ident);
                    }
                }
            }
            _ => {}
        }
        decl.visit_children_with(self);
    }

    fn visit_prop(&mut self, prop: &Prop) {
        if let Prop::Shorthand(ident) = prop {
            if !self.in_pattern {
                self.escape(ident);
            }
        }
        prop.visit_children_with(self);
    }

    fn visit_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Ident(ident) => {
                if !self.in_pattern {
                    self.escape(ident);
                }
                return;
            }
            // Spreading `P.array(...)` marks it variadic for all later uses.
            Expr::Array(array) => {
                for elem in array.elems.iter().flatten() {
                    if let (Some(_), Expr::Ident(ident)) = (elem.spread, &*elem.expr) {
                        self.change(ident.to_id());
                    }
                }
                expr.visit_children_with(self);
                return;
            }
            // Pattern literals and type assertions keep their context.
            Expr::Object(_)
            | Expr::Paren(_)
            | Expr::TsAs(_)
            | Expr::TsConstAssertion(_)
            | Expr::TsSatisfies(_) => {
                expr.visit_children_with(self);
                return;
            }
            Expr::Call(call) => {
                if let Some(patterns) = self.transformer.pattern_arg_count(call) {
                    let outer = std::mem::replace(&mut self.in_pattern, false);
                    call.callee.visit_with(self);
                    for (index, arg) in call.args.iter().enumerate() {
                        self.in_pattern = index < patterns;
                        arg.visit_with(self);
                    }
                    self.in_pattern = outer;
                    return;
                }
            }
            Expr::Assign(assign) => self.change_assign_target(&assign.left),
            Expr::Update(update) => self.change_expr(&update.arg),
            Expr::Unary(unary) if unary.op == UnaryOp::Delete => self.change_expr(&unary.arg),
            _ => {}
        }
        let outer = std::mem::replace(&mut self.in_pattern, false);
        expr.visit_children_with(self);
        self.in_pattern = outer;
    }
}

impl BindingUses<'_> {
    fn escape(&mut self, ident: &Ident) {
        let id = ident.to_id();
        if self
            .transformer
            .const_bindings
            .get(&id)
            .is_some_and(is_mutable_literal)
        {
            self.changed.insert(id);
        }
    }

    fn change_assign_target(&mut self, target: &AssignTarget) {
        match target {
            AssignTarget::Simple(SimpleAssignTarget::Ident(ident)) => self.change(ident.to_id()),
            AssignTarget::Simple(SimpleAssignTarget::Member(member)) => self.change_member(member),
            AssignTarget::Simple(_) | AssignTarget::Pat(_) => {}
        }
    }

    fn change_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Ident(ident) => self.change(ident.to_id()),
            Expr::Member(member) => self.change_member(member),
            _ => {}
        }
    }

    fn change_member(&mut self, member: &MemberExpr) {
        let mut current = &*member.obj;
        loop {
            match current {
                Expr::Ident(ident) => {
                    self.change(ident.to_id());
                    return;
                }
                Expr::Member(member) => current = &member.obj,
                _ => return,
            }
        }
    }

    fn change(&mut self, id: Id) {
        if self.transformer.const_bindings.contains_key(&id) {
            self.changed.insert(id);
        }
    }
}

/// Whether a pattern value is an object or array that code could mutate.
fn is_mutable_literal(expr: &Expr) -> bool {
    match expr {
        Expr::Object(_) | Expr::Array(_) => true,
        Expr::Paren(paren) => is_mutable_literal(&paren.expr),
        Expr::TsAs(assertion) => is_mutable_literal(&assertion.expr),
        Expr::TsConstAssertion(assertion) => is_mutable_literal(&assertion.expr),
        Expr::TsSatisfies(assertion) => is_mutable_literal(&assertion.expr),
        _ => false,
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
    }) || handler_needs_preparation(&chain.fallback)
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

/// Whether a handler is an arrow with plain parameters whose expression body
/// calls and mutates nothing.
fn is_quiet_handler(handler: &Expr) -> bool {
    match handler {
        Expr::Arrow(arrow) if arrow.params.iter().all(Pat::is_ident) => match &*arrow.body {
            BlockStmtOrExpr::Expr(body) => {
                let mut detector = EffectDetector { effect: false };
                body.visit_with(&mut detector);
                !detector.effect
            }
            BlockStmtOrExpr::BlockStmt(_) => false,
        },
        Expr::Paren(paren) => is_quiet_handler(&paren.expr),
        _ => false,
    }
}

fn block_contains_effect(block: &BlockStmt) -> bool {
    let mut detector = EffectDetector { effect: false };
    block.visit_with(&mut detector);
    detector.effect
}

/// Collects `const`, class and function declarations with the source regions
/// that run only after they are set, and every name that a write can rebind.
#[derive(Default)]
struct BindingWrites {
    declared: HashMap<Id, Vec<Span>>,
    assigned: HashSet<Id>,
}

impl BindingWrites {
    /// Lets a `const` or class name be read in the later statements of its own
    /// list, except hoisted function declarations, which can run earlier.
    fn declare_in_list(&mut self, items: &[(Option<&Decl>, bool, Span)]) {
        for (index, (decl, _, _)) in items.iter().enumerate() {
            let names: Vec<Id> = match decl {
                Some(Decl::Class(class)) => vec![class.ident.to_id()],
                Some(Decl::Var(var)) if var.kind == VarDeclKind::Const => var
                    .decls
                    .iter()
                    .filter_map(|decl| match &decl.name {
                        Pat::Ident(ident) => Some(ident.id.to_id()),
                        _ => None,
                    })
                    .collect(),
                _ => continue,
            };
            let regions: Vec<Span> = items[index + 1..]
                .iter()
                .filter(|(_, hoisted, _)| !hoisted)
                .map(|(_, _, span)| *span)
                .collect();
            for name in names {
                self.declared.insert(name, regions.clone());
            }
        }
    }
}

impl Visit for BindingWrites {
    fn visit_module_items(&mut self, items: &[ModuleItem]) {
        let items_info: Vec<_> = items
            .iter()
            .map(|item| {
                let decl = match item {
                    ModuleItem::Stmt(Stmt::Decl(decl))
                    | ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl { decl, .. })) => {
                        Some(decl)
                    }
                    _ => None,
                };
                let hoisted = matches!(decl, Some(Decl::Fn(_)))
                    || matches!(
                        item,
                        ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(ExportDefaultDecl {
                            decl: DefaultDecl::Fn(_),
                            ..
                        }))
                    );
                (decl, hoisted, item.span())
            })
            .collect();
        self.declare_in_list(&items_info);
        items.visit_children_with(self);
    }

    fn visit_stmts(&mut self, stmts: &[Stmt]) {
        let items_info: Vec<_> = stmts
            .iter()
            .map(|stmt| {
                let decl = match stmt {
                    Stmt::Decl(decl) => Some(decl),
                    _ => None,
                };
                (decl, matches!(decl, Some(Decl::Fn(_))), stmt.span())
            })
            .collect();
        self.declare_in_list(&items_info);
        stmts.visit_children_with(self);
    }

    // Function declarations are set before any code in their scope runs.
    fn visit_fn_decl(&mut self, function: &FnDecl) {
        self.declared.insert(
            function.ident.to_id(),
            vec![Span::new(BytePos(0), BytePos(u32::MAX))],
        );
        function.visit_children_with(self);
    }

    // `var` can redeclare a function name and assign it.
    fn visit_var_decl(&mut self, var: &VarDecl) {
        if var.kind != VarDeclKind::Const {
            for decl in &var.decls {
                decl.name.visit_with(&mut BindingNames(&mut self.assigned));
            }
        }
        var.visit_children_with(self);
    }

    fn visit_for_head(&mut self, head: &ForHead) {
        if let ForHead::Pat(pat) = head {
            pat.visit_with(&mut BindingNames(&mut self.assigned));
        }
        head.visit_children_with(self);
    }

    fn visit_assign_target(&mut self, target: &AssignTarget) {
        target.visit_with(&mut BindingNames(&mut self.assigned));
    }

    fn visit_update_expr(&mut self, update: &UpdateExpr) {
        if let Expr::Ident(ident) = &*update.arg {
            self.assigned.insert(ident.to_id());
        }
        update.visit_children_with(self);
    }
}

/// Every `const` declarator with an initializer, in any scope.
#[derive(Default)]
struct ConstDeclarators(Vec<VarDeclarator>);

impl Visit for ConstDeclarators {
    fn visit_var_decl(&mut self, var: &VarDecl) {
        if var.kind == VarDeclKind::Const {
            self.0.extend(var.decls.iter().cloned());
        }
        var.visit_children_with(self);
    }
}

/// Finds `this`, `arguments`, `super` and `new.target`.
struct OuterFunctionRefs(bool);

impl Visit for OuterFunctionRefs {
    fn visit_this_expr(&mut self, _: &ThisExpr) {
        self.0 = true;
    }

    fn visit_super(&mut self, _: &Super) {
        self.0 = true;
    }

    fn visit_meta_prop_expr(&mut self, _: &MetaPropExpr) {
        self.0 = true;
    }

    fn visit_ident(&mut self, ident: &Ident) {
        self.0 |= &*ident.sym == "arguments";
    }
}

struct BindingNames<'a>(&'a mut HashSet<Id>);

impl Visit for BindingNames<'_> {
    fn visit_binding_ident(&mut self, ident: &BindingIdent) {
        self.0.insert(ident.to_id());
    }
}

struct EffectDetector {
    effect: bool,
}

impl Visit for EffectDetector {
    fn visit_expr(&mut self, expr: &Expr) {
        if runs_code(expr) || matches!(expr, Expr::Seq(_)) {
            self.effect = true;
        }
        expr.visit_children_with(self);
    }
}

/// Whether evaluating this node itself calls or mutates something.
fn runs_code(expr: &Expr) -> bool {
    match expr {
        Expr::Call(_)
        | Expr::New(_)
        | Expr::Assign(_)
        | Expr::Update(_)
        | Expr::Await(_)
        | Expr::Yield(_)
        | Expr::TaggedTpl(_)
        | Expr::Class(_)
        | Expr::JSXElement(_)
        | Expr::JSXFragment(_) => true,
        Expr::Unary(unary) => unary.op == UnaryOp::Delete,
        Expr::OptChain(chain) => matches!(&*chain.base, OptChainBase::Call(_)),
        _ => false,
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
    /// Whether testing `pattern` can call code from the source, such as a
    /// `P.when` predicate, an `instanceof` check, or a getter or proxy trap
    /// on the input that a property read, `in` check or iteration runs.
    fn runs_user_code(&self, pattern: &Expr) -> bool {
        let runs = |expr: &Expr| self.runs_user_code(expr);
        match self.resolve_pattern_expr(pattern) {
            Expr::Object(object) => !object.props.is_empty(),
            Expr::Array(_) => true,
            Expr::Paren(paren) => runs(&paren.expr),
            Expr::Call(call) => match self.pattern_call(call) {
                _ if matches!(
                    self.p_call_api(call),
                    Some(PApi::Call("array" | "set" | "map" | "record", _))
                ) =>
                {
                    true
                }
                PatternCall::Select | PatternCall::Nested | PatternCall::Modifier => {
                    method_base(call).is_some_and(runs)
                        || call.args.iter().any(|arg| runs(&arg.expr))
                }
                PatternCall::Values => matches!(self.p_call_api(call), Some(PApi::Call(..))),
                PatternCall::When | PatternCall::Opaque => true,
            },
            _ => false,
        }
    }

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
                "union" | "intersection" | "optional" | "not" | "array" | "set" | "map" | "record"
                | "shape",
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

/// Visits the arguments of every call along a match chain, leaving the chain
/// itself for the caller to lower.
fn visit_chain_args(expr: &mut Expr, visitor: &mut impl VisitMut) {
    let mut current = expr;
    while let Expr::Call(call) = current {
        call.args.visit_mut_with(visitor);
        let Callee::Expr(callee) = &mut call.callee else {
            return;
        };
        let Expr::Member(member) = &mut **callee else {
            return;
        };
        current = &mut member.obj;
    }
}

/// Whether an object literal in `pattern` creates other keys than its props
/// list: `{ a: 1, a: 2 }` keeps only `a: 2`, `{ [k]: 1, a: 2 }` may too, and
/// `{ __proto__: x }` sets a prototype.
fn has_ambiguous_keys(pattern: &Expr) -> bool {
    struct Finder(bool);
    impl Visit for Finder {
        fn visit_object_lit(&mut self, object: &ObjectLit) {
            let mut seen = HashSet::new();
            self.0 |= object
                .props
                .iter()
                .filter_map(|prop| prop.as_prop()?.as_key_value())
                .any(|key_value| match static_key(&key_value.key) {
                    Some(key) => key == "__proto__" || !seen.insert(key),
                    None => object.props.len() > 1,
                });
            object.visit_children_with(self);
        }
    }
    let mut finder = Finder(false);
    pattern.visit_with(&mut finder);
    finder.0
}

/// The key a non-computed property name creates, if the plugin can spell it.
fn static_key(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(str) => Some(str.value.to_string_lossy().into_owned()),
        // Rust and JavaScript spell safe integers alike.
        PropName::Num(num) if num.value.fract() == 0.0 && num.value < 2f64.powi(53) => {
            Some(num.value.to_string())
        }
        PropName::BigInt(bigint) => Some(bigint.value.to_string()),
        _ => None,
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
    if args.iter().any(|pattern| has_ambiguous_keys(pattern)) {
        return None;
    }
    // ts-pattern reads `.with(pattern, guard, handler)` only with exactly
    // three arguments; otherwise every argument before the handler is a pattern.
    let guard = if args.len() == 2 && is_function_expr(&args[1]) {
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
        Expr::Member(_) | Expr::Call(_) | Expr::New(_) | Expr::Seq(_) | Expr::Cond(_)
    )
}

fn selection_capture_projection(capture: &Ident, projection: Option<Vec<String>>) -> Expr {
    let mut value = ident_expr(capture);
    if let Some(projection) = projection {
        for prop in projection {
            value = named_member_expr(value, &prop);
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

/// Replaces every read of `target` with `temp`; with `assign`, the first
/// read, which is also evaluated first, becomes `(temp = target)`.
struct ReadOnce {
    target: Expr,
    temp: Ident,
    assign: bool,
    found: bool,
}

impl ReadOnce {
    fn new(target: Expr, temp: Ident, assign: bool) -> Self {
        Self {
            target,
            temp,
            assign,
            found: false,
        }
    }
}

impl VisitMut for ReadOnce {
    fn visit_mut_expr(&mut self, expr: &mut Expr) {
        if expr.eq_ignore_span(&self.target) {
            *expr = if self.assign && !self.found {
                assign_expr(self.temp.clone(), self.target.clone())
            } else {
                ident_expr(&self.temp)
            };
            self.found = true;
            return;
        }
        expr.visit_mut_children_with(self);
    }
}

/// Whether evaluating `expr` reads `target` before it reads or runs anything
/// else that code could observe.
fn starts_with_read(expr: &Expr, target: &Expr) -> bool {
    if expr.eq_ignore_span(target) {
        return true;
    }
    match expr {
        Expr::Paren(paren) => starts_with_read(&paren.expr, target),
        Expr::Bin(bin) => starts_with_read(&bin.left, target),
        Expr::Unary(unary) => starts_with_read(&unary.arg, target),
        Expr::Seq(seq) => starts_with_read(&seq.exprs[0], target),
        Expr::Cond(cond) => starts_with_read(&cond.test, target),
        Expr::Member(member) => starts_with_read(&member.obj, target),
        Expr::Call(CallExpr {
            callee: Callee::Expr(callee),
            args,
            ..
        }) => {
            starts_with_read(callee, target)
                || (is_quiet_callee(callee)
                    && args.first().is_some_and(|arg| {
                        arg.spread.is_none() && starts_with_read(&arg.expr, target)
                    }))
        }
        _ => false,
    }
}

/// Whether evaluating `callee` reads nothing that code could observe.
fn is_quiet_callee(callee: &Expr) -> bool {
    match callee {
        Expr::Ident(_) | Expr::Arrow(_) | Expr::Fn(_) => true,
        Expr::Paren(paren) => is_quiet_callee(&paren.expr),
        Expr::Member(member) => matches!(&*member.obj, Expr::Ident(_)),
        _ => false,
    }
}

struct CountReads<'a> {
    target: &'a Expr,
    count: usize,
}

impl Visit for CountReads<'_> {
    fn visit_expr(&mut self, expr: &Expr) {
        if expr.eq_ignore_span(self.target) {
            self.count += 1;
            return;
        }
        expr.visit_children_with(self);
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
                    key: selection_prop_name(&selection.name.expect("named selection")),
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
            Some(tpl.quasis[0].cooked.as_ref()?.to_string_lossy().to_string())
        }
        _ => None,
    }
}

/// A key for the selection `name`, quoted when it is not an identifier.
fn selection_prop_name(name: &str) -> PropName {
    if Ident::verify_symbol(name).is_ok() {
        PropName::Ident(IdentName::new(name.into(), DUMMY_SP))
    } else {
        PropName::Str(Str {
            span: DUMMY_SP,
            value: name.into(),
            raw: None,
        })
    }
}

/// Reads the selection `name` from `obj`.
fn named_member_expr(obj: Expr, name: &str) -> Expr {
    match selection_prop_name(name) {
        PropName::Str(str) => computed_member_expr(obj, Expr::Lit(Lit::Str(str))),
        _ => member_expr(obj, name),
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

/// ts-pattern compares as `min <= value && max >= value` for `between`.
fn bigint_chain_test(value: Expr, method: &str, call: &CallExpr) -> Option<Expr> {
    let compare = |op, arg: &ExprOrSpread| bin(op, value.clone(), (*arg.expr).clone());
    let test = match (method, call.args.as_slice()) {
        ("between", [min, max]) => and(
            bin(BinaryOp::LtEq, (*min.expr).clone(), value.clone()),
            bin(BinaryOp::GtEq, (*max.expr).clone(), value.clone()),
        ),
        ("lt", [arg]) => compare(BinaryOp::Lt, arg),
        ("gt", [arg]) => compare(BinaryOp::Gt, arg),
        ("lte", [arg]) => compare(BinaryOp::LtEq, arg),
        ("gte", [arg]) => compare(BinaryOp::GtEq, arg),
        ("positive", _) => bin(BinaryOp::Gt, value.clone(), num_lit(0.0)),
        ("negative", _) => bin(BinaryOp::Lt, value.clone(), num_lit(0.0)),
        _ => return None,
    };
    Some(and(typeof_eq(value, "bigint"), test))
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

fn as_method_call(call: &CallExpr) -> Option<(&str, &Expr)> {
    let Callee::Expr(callee) = &call.callee else {
        return None;
    };
    let Expr::Member(member) = &**callee else {
        return None;
    };

    Some((member_name(member)?, &member.obj))
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

/// ts-pattern tests keys in `Reflect.ownKeys` order: array indexes in
/// ascending order, then the other keys in source order.
fn own_keys_order(object: &ObjectLit) -> Vec<&PropOrSpread> {
    let mut props = object.props.iter().collect::<Vec<_>>();
    props.sort_by_key(|prop| {
        prop.as_prop()
            .and_then(|prop| prop.as_key_value())
            .and_then(|key_value| static_key(&key_value.key))
            .and_then(|key| array_index(&key))
            .map_or((1, 0), |index| (0, index))
    });
    props
}

fn array_index(key: &str) -> Option<u32> {
    let index = key.parse::<u32>().ok()?;
    (index != u32::MAX && index.to_string() == key).then_some(index)
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

fn assign_stmt(target: Ident, expr: Expr) -> Stmt {
    expr_stmt(Expr::Assign(AssignExpr {
        span: DUMMY_SP,
        op: AssignOp::Assign,
        left: AssignTarget::Simple(SimpleAssignTarget::Ident(target.into())),
        right: Box::new(expr),
    }))
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

/// Declares `temps` at the start of `body`.
fn declare_temps(temps: Vec<Ident>, mut body: BlockStmt) -> BlockStmt {
    // After directives such as `"use strict"`, which must stay first.
    prepend_stmts(&mut body.stmts, temps.into_iter().map(let_decl));
    body
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
        // Array destructuring iterates its value, which indexing does not.
        Pat::Array(_) | Pat::Assign(_) | Pat::Rest(_) | Pat::Invalid(_) | Pat::Expr(_) => None,
    }
}

/// Substituting arguments delays their evaluation to each use, which is
/// unobservable only when the body runs no code before reading them and
/// nested functions do not capture them. A call runs after its callee and
/// arguments, so `match(v).exhaustive()` may read `v`. An identifier or
/// literal may be read any number of times; other arguments must be read
/// exactly once.
fn is_simple_inline_body(body: &Expr, bindings: &[(Id, Expr)]) -> bool {
    let mut usage = BindingUsage {
        bindings,
        counts: vec![0; bindings.len()],
        unsafe_body: false,
        ran_code: false,
        function_depth: 0,
    };
    body.visit_with(&mut usage);
    !usage.unsafe_body
        && usage
            .counts
            .iter()
            .zip(bindings)
            .all(|(count, (_, value))| {
                *count == 1 || matches!(value, Expr::Ident(_) | Expr::Lit(_))
            })
}

struct BindingUsage<'a> {
    bindings: &'a [(Id, Expr)],
    counts: Vec<usize>,
    unsafe_body: bool,
    /// Whether a call or `new` already ran before the current read.
    ran_code: bool,
    function_depth: usize,
}

impl BindingUsage<'_> {
    fn count(&mut self, ident: &Ident) {
        let id = ident.to_id();
        if let Some(index) = self.bindings.iter().position(|(binding, _)| *binding == id) {
            if self.function_depth > 0 || self.ran_code {
                self.unsafe_body = true;
            }
            self.counts[index] += 1;
        }
    }
}

impl Visit for BindingUsage<'_> {
    fn visit_prop(&mut self, prop: &Prop) {
        if let Prop::Shorthand(ident) = prop {
            self.count(ident);
        }
        prop.visit_children_with(self);
    }

    fn visit_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Ident(ident) => self.count(ident),
            Expr::Arrow(_) | Expr::Fn(_) => {
                self.function_depth += 1;
                expr.visit_children_with(self);
                self.function_depth -= 1;
                return;
            }
            Expr::Call(_) | Expr::New(_) if self.function_depth == 0 => {
                expr.visit_children_with(self);
                self.ran_code = true;
                return;
            }
            _ if self.function_depth == 0 && runs_code(expr) => self.unsafe_body = true,
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

    fn visit_mut_prop(&mut self, prop: &mut Prop) {
        if let Prop::Shorthand(ident) = prop {
            let id = ident.to_id();
            if let Some((_, replacement)) = self.bindings.iter().find(|(binding, _)| *binding == id)
            {
                *prop = Prop::KeyValue(KeyValueProp {
                    key: PropName::Ident(ident.clone().into()),
                    value: Box::new(replacement.clone()),
                });
                return;
            }
        }
        prop.visit_mut_children_with(self);
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

fn iterable_every(iterable: Expr, left: Pat, test: Expr, temps: Vec<Ident>) -> Expr {
    let mut stmts = temps.into_iter().map(let_decl).collect::<Vec<_>>();
    stmts.extend([
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
    ]);
    iife(stmts)
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
    /// Temporaries the key and value tests assign.
    temps: Vec<Ident>,
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
        temps,
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
    let mut capture_decls = Vec::new();
    let mut value_arrays = Vec::new();
    // The result of a nested record value, read once for all its selections.
    let mut group_capture: Option<Ident> = None;
    for selection in value_selections {
        let array = private_ident!("_tsPatternRecordSelectedValues");
        value_arrays.push(array);
        // A temp already holds the value that the test reads.
        if let Expr::Ident(read) = &selection.value {
            value_captures.push(ident_expr(read));
            continue;
        }
        if selection.capture_group.is_some() {
            let capture = group_capture
                .get_or_insert_with(|| {
                    let capture = private_ident!("_tsPatternRecordSelectedValue");
                    let mut replacer =
                        ReadOnce::new(selection.value.clone(), capture.clone(), true);
                    value_test.visit_mut_with(&mut replacer);
                    capture_decls.push(capture.clone());
                    capture
                })
                .clone();
            value_captures.push(selection_capture_projection(
                &capture,
                selection.projection.clone(),
            ));
            continue;
        }
        let capture = private_ident!("_tsPatternRecordSelectedValue");
        let mut replacer = ReadOnce::new(selection.value.clone(), capture.clone(), true);
        value_test.visit_mut_with(&mut replacer);
        if !replacer.found {
            let read = assign_expr(capture.clone(), selection.value.clone());
            // A value built from temps of the test is ready only after it.
            value_test = if temps
                .iter()
                .any(|temp| contains_ident_ref(&selection.value, temp))
            {
                ordered_and(value_test, seq_expr(read, bool_lit(true)))
            } else {
                seq_expr(read, value_test)
            };
        }
        capture_decls.push(capture.clone());
        value_captures.push(ident_expr(&capture));
    }
    let selection = record_selection_object(
        &selected_keys,
        &value_arrays,
        key_selections,
        value_selections,
    );
    // With a nested record, only entries that selected add values.
    let selected_from = match (&group_capture, value_arrays.first()) {
        (Some(_), Some(array)) => array,
        _ => &keys,
    };
    let result = record_result_object(
        ident_expr(&matched),
        selection.clone(),
        bin(
            BinaryOp::NotEqEq,
            member_expr(ident_expr(selected_from), "length"),
            num_lit(0.0),
        ),
    );
    let invalid_result = record_result_object(bool_lit(false), selection, bool_lit(false));
    let push = |array: &Ident, item: Expr| {
        expr_stmt(call_expr(
            member_expr(ident_expr(array), "push"),
            vec![item],
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
                Stmt::Block(block_stmt(vec![push(
                    &selected_keys,
                    ident_expr(coerced_key_ident),
                )])),
                None,
            ));
        }
        if !key_selections.is_empty() {
            collect_selections.push(if_stmt(
                ident_expr(&key_match),
                Stmt::Block(block_stmt(vec![push(
                    &selected_keys,
                    ident_expr(&key_ident),
                )])),
                None,
            ));
        }
    } else if !key_selections.is_empty() {
        collect_selections.push(push(&selected_keys, ident_expr(&key_ident)));
    }
    let value_pushes = value_arrays
        .iter()
        .zip(value_captures)
        .map(|(array, capture)| push(array, capture));
    match &group_capture {
        Some(capture) => collect_selections.push(if_stmt(
            member_expr(ident_expr(capture), "selected"),
            Stmt::Block(block_stmt(value_pushes.collect())),
            None,
        )),
        None => collect_selections.extend(value_pushes),
    }
    let mut entry_stmts = temps.into_iter().map(let_decl).collect::<Vec<_>>();
    entry_stmts.push(const_stmt(
        value_ident,
        computed_member_expr(ident_expr(&record_value), ident_expr(&key_ident)),
    ));
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
    setup_statements.extend(capture_decls.into_iter().map(let_decl));
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

/// Each key selection gets its own array of the matched keys; each value
/// selection collects its own array of values.
fn record_selection_object(
    selected_keys: &Ident,
    selected_values: &[Ident],
    key_selections: &[SelectionBinding],
    value_selections: &[SelectionBinding],
) -> Expr {
    let key_arrays = key_selections.iter().enumerate().map(|(index, selection)| {
        let keys = ident_expr(selected_keys);
        // A handler can change the array of one name, so later names get a copy.
        let array = match index {
            0 => keys,
            _ => call_expr(member_expr(keys, "slice"), vec![]),
        };
        (selection, array)
    });
    let value_arrays = value_selections
        .iter()
        .zip(selected_values)
        .map(|(selection, array)| (selection, ident_expr(array)));
    let mut props = Vec::new();
    for (selection, array) in key_arrays.chain(value_arrays) {
        let Some(name) = &selection.name else {
            // An anonymous selection is the only one.
            return array;
        };
        props.push(PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
            key: selection_prop_name(name),
            value: Box::new(array),
        }))));
    }
    Expr::Object(ObjectLit {
        span: DUMMY_SP,
        props,
    })
}

/// `selected` is false when the record has no keys, so nothing was selected.
fn record_result_object(matched: Expr, selection: Expr, selected: Expr) -> Expr {
    Expr::Object(ObjectLit {
        span: DUMMY_SP,
        props: [
            ("matched", matched),
            ("selection", selection),
            ("selected", selected),
        ]
        .into_iter()
        .map(|(key, value)| {
            PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                key: PropName::Ident(IdentName::new(key.into(), DUMMY_SP)),
                value: Box::new(value),
            })))
        })
        .collect(),
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
    fn declares_temporaries_after_directive_prologues() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
export function run(x: any) { "worklet"; "use strict"; return [match(x).with({ a: { b: P.select() } }, (v) => v).otherwise(() => 0)]; }
class C { constructor(x: any) { "use strict"; this.v = [match(x).with({ a: { b: P.select() } }, (v) => v).otherwise(() => 0)]; } }"#,
        );

        assert!(
            output.contains("{\n    \"worklet\";\n    \"use strict\";\n    let _tsPattern"),
            "{output}"
        );
        assert!(
            output.contains("{\n        \"use strict\";\n        let _tsPattern"),
            "{output}"
        );
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
    fn emits_if_block_for_arrow_body_exhaustive_match() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
export const run = (result: { type: "error" } | { type: "ok"; data: { type: "img"; src: string } } | { type: "ok"; data: { type: "text"; content: string } }) => match(result).with({ type: "error" }, () => "error").with({ type: "ok", data: { type: "text" } }, ({ data }) => data.content).with({ type: "ok", data: { type: "img", src: P.select() } }, (src) => src).exhaustive();"#,
        );

        assert!(output.contains("=>{"), "{output}");
        assert!(
            output.contains("return match(_tsPatternInput).exhaustive()"),
            "{output}"
        );
        assert!(!output.contains("match(result).with"), "{output}");
        assert!(!output.contains("=>()=>"), "{output}");
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
        assert!(
            output.contains("\"data\" in result && ((_tsPatternData = result.data)"),
            "{output}"
        );
        assert!(
            output.contains("_tsPatternResult = _tsPatternSrc"),
            "{output}"
        );
        assert!(!output.contains("match(result).with"), "{output}");
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
    fn passes_effectful_input_once_to_an_arrow_at_module_scope() {
        let output = transform(
            r#"import { match } from "ts-pattern";
const result = match(next()).with("a", () => 1).otherwise(() => 0);"#,
        );

        assert!(!output.contains(".with("), "{output}");
        assert!(output.matches("next()").count() == 1, "{output}");
        assert!(output.contains(")(next())"), "{output}");
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
    fn inlines_multi_use_p_when_predicate_on_identifier() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with(P.when((value) => value > 0 && value < 10), () => "ok").otherwise(() => "other");"#,
        );

        assert!(output.contains("input > 0 && input < 10"), "{output}");
        assert!(!output.contains(")(input)"), "{output}");
    }

    #[test]
    fn inlines_p_when_predicate_call_after_its_argument() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with(P.when((value) => check(value)), () => "ok").otherwise(() => "other");"#,
        );

        assert!(output.contains("check(_tsPatternInput) ?"), "{output}");
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
    fn falls_back_to_runtime_exhaustive_without_iife() {
        let output = transform(
            r#"import { match } from "ts-pattern";
const result = match(input).with("ok", () => true).exhaustive();"#,
        );

        assert!(
            output.contains(r#"input === "ok" ? true : match(input).exhaustive()"#),
            "{output}"
        );
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
        assert!(
            output.contains("(_tsPatternData = input.data), _tsPatternData !== null"),
            "{output}"
        );
        assert!(
            output.contains("(_tsPatternSrc = _tsPatternData.src)"),
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
            output.contains("((_tsPatternValue = input[1]), _tsPatternValue === \"a\" || _tsPatternValue === \"b\")"),
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
const handle = (value: unknown) => value;
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
    fn lowers_capturing_initializers_and_returns_to_statements() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
export function run(input: unknown) {
  const name = match(input).with({ user: { name: P.select() } }, (name) => name).otherwise(() => "");
  return match(name).with(P.string.select(), (value) => value).otherwise(() => "");
}"#,
        );

        assert!(
            output.contains("const name = _tsPatternResult;"),
            "{output}"
        );
        assert!(output.contains("(_tsPatternUser = input.user)"), "{output}");
        assert!(!output.contains(")(undefined"), "{output}");
        assert!(!output.contains("match(name)"), "{output}");
    }

    #[test]
    fn collects_array_item_selections() {
        let output = transform(
            r#"import { match, P } from "ts-pattern";
const result = match(input).with(P.array(P.select()), (value) => value).otherwise(() => []);"#,
        );

        assert!(!output.contains(".with("), "{output}");
        assert!(output.contains(".push("), "{output}");
    }
}
