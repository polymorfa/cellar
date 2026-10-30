//! Telling a rename from a change.
//!
//! Half the WhatsApp modules in a release differ byte-for-byte, which is ~7,400
//! diffs nobody reads. Most of that is not change at all. Minifiers assign names
//! by position, so inserting one constant renames everything after it:
//!
//! ```text
//! - var … h = "16505361212@c.us", y = /^1313555\d{4}$/, C = 99, b = 4, v = (…)
//! + var … h = "1807055946647698", y = "16505361212@c.us", C = /^1313555\d{4}$/, b = 99, v = 4, S = (…)
//! ```
//!
//! Every local shifted by one. A text diff calls that five changes; it is zero.
//! And buried in the same module were three real ones — a dependency dropped, an
//! argument removed, a gating branch deleted — which is exactly the failure
//! mode: the noise is not merely loud, it hides the signal.
//!
//! # The shape
//!
//! A module is walked to a canonical token stream in which every identifier that
//! *binds within the module* is replaced by the order it was first encountered.
//! Two modules that differ only in the names of their locals produce identical
//! streams, so a rename is invisible by construction rather than by heuristic.
//!
//! Three things are deliberately **not** normalized, because each is a real
//! difference that a looser comparison would swallow:
//!
//! * **Free identifiers.** `babelHelpers`, `Promise`, `Symbol` are not this
//!   module's to rename; if one changes, something changed.
//! * **String and numeric literals.** They are the wire values this whole
//!   project exists to track. `"pin"` becoming `"pin_v1"` is the change.
//! * **Order.** Statement order is semantics, not formatting.
//!
//! Minifier artifacts that *are* pure encoding — `!0` for `true`, `!1` for
//! `false`, `void 0` for `undefined` — are folded, since a minifier that changes
//! its mind about those has not changed the program.

use oxc_allocator::Allocator;
use oxc_ast::{AstKind, ast::*};
use oxc_ast_visit::Visit;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_semantic::{Scoping, SymbolId};
use oxc_span::SourceType;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::diff::SourceLoader;
use crate::model::ModuleEntry;

/// A module's structure, with local names erased.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shape {
    /// FNV-1a of the canonical stream.
    pub hash: String,
    /// How many tokens it took. A cheap size signal for ranking diffs.
    pub tokens: usize,
}

struct Canon<'s> {
    scoping: &'s Scoping,
    out: String,
    tokens: usize,
    /// symbol -> the order it was first seen. A minifier's choice of letter is
    /// not information; the position in the binding order is.
    seen: HashMap<SymbolId, u32>,
    function_span: Option<oxc_span::Span>,
}

impl<'s> Canon<'s> {
    fn new(scoping: &'s Scoping) -> Self {
        Self {
            scoping,
            out: String::new(),
            tokens: 0,
            seen: HashMap::new(),
            function_span: None,
        }
    }

    fn for_function(scoping: &'s Scoping, span: oxc_span::Span) -> Self {
        Self { function_span: Some(span), ..Self::new(scoping) }
    }

    fn push(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\x1f');
        self.tokens += 1;
    }

    fn ident(&mut self, symbol: Option<SymbolId>, name: &str) {
        match symbol {
            // Bound here: emit its ordinal, so `y` and `C` are the same thing
            // when they occupy the same position in the binding order.
            Some(id) => {
                if self.function_span.is_some_and(|span| {
                    let declared = self.scoping.symbol_span(id);
                    declared.start < span.start || declared.end > span.end
                }) {
                    self.push(&format!("capture{}", id.index()));
                    return;
                }
                let next = self.seen.len() as u32;
                let n = *self.seen.entry(id).or_insert(next);
                self.push(&format!("${n}"));
            }
            // Free: `babelHelpers`, `Promise`, a global. Not ours to rename, so
            // a difference in one is a difference in the program.
            None => self.push(&format!("@{name}")),
        }
    }
}

impl<'a, 's> Visit<'a> for Canon<'s> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        match kind {
            AstKind::BindingIdentifier(b) => {
                let name = b.name.as_str();
                self.ident(b.symbol_id.get(), name);
            }
            AstKind::IdentifierReference(r) => {
                let name = r.name.as_str();
                let symbol = r
                    .reference_id
                    .get()
                    .and_then(|id| self.scoping.get_reference(id).symbol_id());
                self.ident(symbol, name);
            }
            // Literals carry the wire values this project tracks. Never folded.
            AstKind::StringLiteral(s) => self.push(&format!("s{:?}", s.value.as_str())),
            AstKind::NumericLiteral(n) => self.push(&format!("n{}", n.value)),
            AstKind::BigIntLiteral(b) => self.push(&format!("N{}", b.value)),
            AstKind::BooleanLiteral(b) => self.push(&format!("b{}", b.value)),
            AstKind::RegExpLiteral(r) => self.push(&format!("r{}", r.regex)),
            AstKind::NullLiteral(_) => self.push("null"),
            // A property name is part of the shape: `.foo` and `.bar` differ.
            AstKind::IdentifierName(i) => self.push(&format!(".{}", i.name)),
            AstKind::PrivateIdentifier(i) => self.push(&format!("#{}", i.name)),
            // Operators are semantics, and the discriminant does not carry them:
            // `a > b` and `a >= b` are both `BinaryExpression`. Erasing names
            // must not erase meaning.
            AstKind::BinaryExpression(e) => self.push(&format!("op{}", e.operator.as_str())),
            AstKind::LogicalExpression(e) => self.push(&format!("op{}", e.operator.as_str())),
            AstKind::AssignmentExpression(e) => self.push(&format!("op{}", e.operator.as_str())),
            AstKind::UnaryExpression(e) => self.push(&format!("op{}", e.operator.as_str())),
            AstKind::VariableDeclaration(d) => self.push(&format!("decl{:?}", d.kind)),
            AstKind::Function(f) => self.push(&format!("fn{:?}:{}:{}", f.r#type, f.r#async, f.generator)),
            AstKind::ArrowFunctionExpression(f) => self.push(&format!("arrow{}:{}", f.r#async, f.expression)),
            AstKind::TemplateElement(t) => self.push(&format!("template{:?}:{}", t.value.raw, t.tail)),
            AstKind::ObjectProperty(p) => self.push(&format!("prop{:?}:{}:{}", p.kind, p.method, p.computed)),
            AstKind::CallExpression(e) => self.push(&format!("call{}", e.optional)),
            AstKind::StaticMemberExpression(e) => self.push(&format!("member{}", e.optional)),
            AstKind::ComputedMemberExpression(e) => self.push(&format!("computed{}", e.optional)),
            AstKind::PrivateFieldExpression(e) => self.push(&format!("private{}", e.optional)),
            AstKind::MethodDefinition(m) => self.push(&format!("method{:?}:{}:{}", m.kind, m.r#static, m.computed)),
            AstKind::PropertyDefinition(p) => self.push(&format!("field{}:{}", p.r#static, p.computed)),
            AstKind::ForOfStatement(f) => self.push(&format!("forof{}", f.r#await)),
            AstKind::YieldExpression(e) => self.push(&format!("yield{}", e.delegate)),
            AstKind::LabelIdentifier(i) => self.push(&format!("label{}", i.name)),
            AstKind::UpdateExpression(e) => {
                self.push(&format!("op{}{}", e.operator.as_str(), e.prefix))
            }
            other => {
                // The discriminant, not `debug_name()`. That helpfully embeds
                // the identifier — `VariableDeclarator(h)` — so every parent
                // node leaked the very name the normalization exists to erase,
                // and two renamed-but-identical modules never matched.
                self.push(&format!("{:?}", std::mem::discriminant(&other)));
            }
        }
    }

    fn leave_node(&mut self, _: AstKind<'a>) {
        self.push(")");
    }

    /// `!0` and `!1` are how a minifier writes `true` and `false`.
    ///
    /// Folded because a minifier changing its mind between the two encodings has
    /// not changed the program — and it does, between builds.
    fn visit_unary_expression(&mut self, expr: &UnaryExpression<'a>) {
        if expr.operator == UnaryOperator::LogicalNot
            && let Expression::NumericLiteral(n) = &expr.argument
            && (n.value == 0.0 || n.value == 1.0)
        {
            self.push(&format!("b{}", n.value == 0.0));
            // The close token too: `true` costs enter+leave, so a fold that
            // emits only one would differ from the literal it folds to.
            self.push(")");
            return;
        }
        // `void 0` is `undefined`, for the same reason.
        if expr.operator == UnaryOperator::Void
            && let Expression::NumericLiteral(n) = &expr.argument
            && n.value == 0.0
        {
            self.push("@undefined");
            self.push(")");
            return;
        }
        oxc_ast_visit::walk::walk_unary_expression(self, expr);
    }
}

fn fnv1a(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The canonical shape of a module, or `None` if it does not parse.
pub fn shape(src: &str) -> Option<Shape> {
    let alloc = Allocator::default();
    let parsed = Parser::new(&alloc, src, SourceType::cjs()).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return None;
    }
    // Symbol ids are what make a rename invisible, and they only exist once the
    // binder has run.
    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(&parsed.program)
        .semantic;
    let mut c = Canon::new(semantic.scoping());
    c.visit_program(&parsed.program);
    Some(Shape {
        hash: fnv1a(&c.out),
        tokens: c.tokens,
    })
}

/// What a comparison of two revisions of one module concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    /// Byte-identical.
    Same,
    /// Different text, identical shape: renaming and nothing else.
    RenamedOnly,
    /// The program changed.
    Changed,
    /// One side did not parse. Reported, never guessed at.
    Unknown,
}

/// The shape of every function in a module, independently.
///
/// Module-level comparison turned out to be nearly useless on real revisions:
/// every module a release touches contains *something* real, so 7,520 of them
/// come back "changed" and the reviewer is no better off. The unit that reduces
/// the work is the function — a module with sixty functions and one real edit
/// should cost you one function to read, not sixty.
pub fn function_shapes(src: &str) -> Vec<Shape> {
    let alloc = Allocator::default();
    let parsed = Parser::new(&alloc, src, SourceType::cjs()).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return vec![];
    }
    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(&parsed.program)
        .semantic;

    struct Fns<'s> {
        scoping: &'s Scoping,
        out: Vec<Shape>,
    }
    impl<'a, 's> Visit<'a> for Fns<'s> {
        fn visit_function(&mut self, f: &Function<'a>, flags: oxc_semantic::ScopeFlags) {
            let mut c = Canon::for_function(self.scoping, f.span);
            c.visit_function(f, flags);
            self.out.push(Shape {
                hash: fnv1a(&c.out),
                tokens: c.tokens,
            });
            oxc_ast_visit::walk::walk_function(self, f, flags);
        }
        fn visit_arrow_function_expression(&mut self, f: &ArrowFunctionExpression<'a>) {
            let mut c = Canon::for_function(self.scoping, f.span);
            c.visit_arrow_function_expression(f);
            self.out.push(Shape {
                hash: fnv1a(&c.out),
                tokens: c.tokens,
            });
            oxc_ast_visit::walk::walk_arrow_function_expression(self, f);
        }
    }

    let mut v = Fns {
        scoping: semantic.scoping(),
        out: Vec::new(),
    };
    v.visit_program(&parsed.program);
    v.out
}

/// How much of a changed module actually changed.
///
/// Returns (functions whose shape is unchanged, total functions in the new
/// revision). Shapes are matched as a multiset: a function that merely moved is
/// not a change, and moving is most of what a minifier does.
pub fn function_overlap(before: &str, after: &str) -> (usize, usize) {
    use std::collections::HashMap;
    let mut old: HashMap<String, usize> = HashMap::new();
    for s in function_shapes(before) {
        *old.entry(s.hash).or_default() += 1;
    }
    let new = function_shapes(after);
    let mut kept = 0;
    for s in &new {
        if let Some(n) = old.get_mut(&s.hash)
            && *n > 0
        {
            *n -= 1;
            kept += 1;
        }
    }
    (kept, new.len())
}

pub fn compare(before: &str, after: &str) -> Verdict {
    if before == after {
        return Verdict::Same;
    }
    match (shape(before), shape(after)) {
        (Some(a), Some(b)) if a.hash == b.hash => Verdict::RenamedOnly,
        (Some(_), Some(_)) => Verdict::Changed,
        _ => Verdict::Unknown,
    }
}

// --- naming the functions --------------------------------------------------

/// One function, canonically shaped and labelled by whatever survives minification.
///
/// The label deliberately excludes local binding names. A minifier renames
/// `function e(t)` to `function u(n)` between builds, so matching on that would
/// re-introduce exactly the noise the shape exists to remove — and do worse than
/// not matching at all, because it would pair up unrelated functions and report
/// both as changed.
///
/// What survives is the **export name**. A Metro module declares its functions
/// under minified locals and then publishes them — `l.encodeServerErrorReceipt =
/// u` — and that property name is stable across builds because it is the
/// module's interface. So the local is resolved through the export table before
/// it is used as a label, which is the same walk the source viewer's
/// go-to-definition does, in the other direction. Failing that, anything written
/// directly as a property (`{parse: …}`, a class method) names itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Func {
    /// `encodeServerErrorReceipt`, `encode/#2` for a closure nested inside
    /// `encode`, `#7` for something anonymous at the top level.
    pub label: String,
    pub hash: String,
    pub tokens: usize,
    /// How many parameters it declares. Used only to pair up unnamed functions,
    /// where it is the one stable attribute a minifier does not touch.
    pub params: usize,
    /// Byte offsets into the module, so a viewer can jump straight to it.
    pub start: u32,
    pub end: u32,
}

/// Which module-local symbols are published, and under what name.
///
/// `__d("M", [...], function(a, b, …, exports) { … exports.foo = u … })`: the
/// factory parameter resolved from exports/module writes is the exports object, so an assignment to a
/// property of it maps a stable public name onto whatever letter the minifier
/// happened to give the local this build.
fn export_parameter(f: &Function<'_>, scoping: &Scoping) -> Option<SymbolId> {
    let params: Vec<_> = f.params.items.iter().map(|p| match &p.pattern {
        BindingPattern::BindingIdentifier(id) => id.symbol_id.get(), _ => None,
    }).collect();
    struct Writes<'s> { scoping: &'s Scoping, module: Vec<SymbolId>, other: Vec<SymbolId> }
    impl<'a> Visit<'a> for Writes<'_> {
        fn visit_assignment_expression(&mut self, e: &AssignmentExpression<'a>) {
            if let Some(m) = e.left.as_member_expression()
                && let Expression::Identifier(r) = m.object()
                && let Some(id) = r.reference_id.get().and_then(|id| self.scoping.get_reference(id).symbol_id()) {
                if m.static_property_name() == Some("exports") { self.module.push(id) } else { self.other.push(id) }
            }
            oxc_ast_visit::walk::walk_assignment_expression(self,e);
        }
    }
    let mut w = Writes { scoping, module: vec![], other: vec![] };
    if let Some(body) = &f.body { w.visit_function_body(body); }
    f.params.items.iter().find_map(|p| match &p.pattern {
        BindingPattern::BindingIdentifier(id) if id.name == "exports" => id.symbol_id.get(), _=>None,
    }).or_else(|| params.iter().position(|id| id.is_some_and(|id| w.module.contains(&id)))
        .and_then(|i| params.get(i+1).copied().flatten()))
      .or_else(|| params.iter().rev().flatten().find(|id| w.other.contains(id)).copied())
      .or_else(|| params.get(if params.len()==6 {5} else {6}).copied().flatten())
}

fn export_names(program: &Program<'_>, scoping: &Scoping) -> HashMap<SymbolId, String> {
    struct Exports<'s> {
        scoping: &'s Scoping,
        /// The exports object, once the module factory has been recognised.
        obj: Option<SymbolId>,
        out: HashMap<SymbolId, String>,
        depth: u32,
    }

    impl<'s> Exports<'s> {
        fn symbol_of(&self, r: &IdentifierReference) -> Option<SymbolId> {
            r.reference_id
                .get()
                .and_then(|id| self.scoping.get_reference(id).symbol_id())
        }
    }

    impl<'a, 's> Visit<'a> for Exports<'s> {
        fn visit_function(&mut self, f: &Function<'a>, flags: oxc_semantic::ScopeFlags) {
            if self.depth == 0 {
                self.obj = export_parameter(f, self.scoping);
            }
            self.depth += 1;
            oxc_ast_visit::walk::walk_function(self, f, flags);
            self.depth -= 1;
        }

        fn visit_assignment_expression(&mut self, e: &AssignmentExpression<'a>) {
            if let AssignmentTarget::StaticMemberExpression(m) = &e.left
                && let Expression::Identifier(obj) = &m.object
                && self.symbol_of(obj).is_some()
                && self.symbol_of(obj) == self.obj
                && let Expression::Identifier(local) = &e.right
                && let Some(sym) = self.symbol_of(local)
            {
                // First name wins. A local published twice is published under
                // both, and picking either consistently beats picking whichever
                // the walk happened to reach last.
                self.out
                    .entry(sym)
                    .or_insert_with(|| m.property.name.to_string());
            }
            oxc_ast_visit::walk::walk_assignment_expression(self, e);
        }
    }

    let mut v = Exports {
        scoping,
        obj: None,
        out: HashMap::new(),
        depth: 0,
    };
    v.visit_program(program);
    v.out
}

/// Every function in a module, shaped and labelled.
pub fn functions(src: &str) -> Vec<Func> {
    let alloc = Allocator::default();
    let parsed = Parser::new(&alloc, src, SourceType::cjs()).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return vec![];
    }
    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(&parsed.program)
        .semantic;
    let exported = export_names(&parsed.program, semantic.scoping());

    struct Funcs<'s> {
        scoping: &'s Scoping,
        /// Local symbol -> the name it is published under.
        exported: HashMap<SymbolId, String>,
        out: Vec<Func>,
        /// Labels of the functions we are currently inside.
        path: Vec<String>,
        /// Set by a parent immediately before descending into its function
        /// value, consumed by that function. `take()` is what keeps a stale
        /// hint from being picked up by an unrelated sibling.
        hint: Option<String>,
        anon: u32,
    }

    impl<'s> Funcs<'s> {
        fn record(
            &mut self,
            span: oxc_span::Span,
            params: usize,
            named: Option<String>,
            canon: &Canon<'s>,
        ) -> String {
            let label = match named.or_else(|| self.hint.take()) {
                Some(n) => n,
                None => {
                    self.anon += 1;
                    format!("#{}", self.anon)
                }
            };
            // Only named ancestors qualify a label. Every function in a Metro
            // module is nested inside the module factory, which is itself
            // unnamed, so including anonymous ancestors would prefix `#1/` onto
            // the entire module and hang every label off an ordinal that shifts
            // whenever an earlier closure is added.
            let named_path: Vec<&str> = self
                .path
                .iter()
                .filter(|p| !p.starts_with('#'))
                .map(String::as_str)
                .collect();
            let full = if named_path.is_empty() {
                label.clone()
            } else {
                format!("{}/{}", named_path.join("/"), label)
            };
            self.out.push(Func {
                label: full,
                hash: fnv1a(&canon.out),
                tokens: canon.tokens,
                params,
                start: span.start,
                end: span.end,
            });
            label
        }
    }

    fn key_name(k: &PropertyKey) -> Option<String> {
        match k {
            PropertyKey::StaticIdentifier(i) => Some(i.name.to_string()),
            PropertyKey::StringLiteral(s) => Some(s.value.to_string()),
            _ => None,
        }
    }

    impl<'a, 's> Visit<'a> for Funcs<'s> {
        fn visit_function(&mut self, f: &Function<'a>, flags: oxc_semantic::ScopeFlags) {
            let mut c = Canon::for_function(self.scoping, f.span);
            c.visit_function(f, flags);
            // `function u(t)` says nothing; `exports.encodeServerErrorReceipt =
            // u` says everything, and it is the same `u`.
            let named =
                f.id.as_ref()
                    .and_then(|id| id.symbol_id.get())
                    .and_then(|s| self.exported.get(&s).cloned());
            let label = self.record(f.span, f.params.items.len(), named, &c);
            self.path.push(label);
            oxc_ast_visit::walk::walk_function(self, f, flags);
            self.path.pop();
        }

        fn visit_arrow_function_expression(&mut self, f: &ArrowFunctionExpression<'a>) {
            let mut c = Canon::for_function(self.scoping, f.span);
            c.visit_arrow_function_expression(f);
            let label = self.record(f.span, f.params.items.len(), None, &c);
            self.path.push(label);
            oxc_ast_visit::walk::walk_arrow_function_expression(self, f);
            self.path.pop();
        }

        /// `{parse: function(){…}}` — the key names the value.
        fn visit_object_property(&mut self, p: &ObjectProperty<'a>) {
            if matches!(
                p.value,
                Expression::FunctionExpression(_) | Expression::ArrowFunctionExpression(_)
            ) {
                self.hint = key_name(&p.key);
            }
            oxc_ast_visit::walk::walk_object_property(self, p);
        }

        /// `l.assertTag = function(){…}` — the export surface, and the one name
        /// in a minified module that means the same thing next release.
        fn visit_assignment_expression(&mut self, e: &AssignmentExpression<'a>) {
            if matches!(
                e.right,
                Expression::FunctionExpression(_) | Expression::ArrowFunctionExpression(_)
            ) && let AssignmentTarget::StaticMemberExpression(m) = &e.left
            {
                self.hint = Some(m.property.name.to_string());
            }
            oxc_ast_visit::walk::walk_assignment_expression(self, e);
        }

        fn visit_method_definition(&mut self, m: &MethodDefinition<'a>) {
            self.hint = key_name(&m.key);
            oxc_ast_visit::walk::walk_method_definition(self, m);
        }

        /// `var u = function(){…}`, where `u` is later published. Same trick as
        /// a function declaration, different syntax for it.
        fn visit_variable_declarator(&mut self, d: &VariableDeclarator<'a>) {
            if matches!(
                d.init,
                Some(Expression::FunctionExpression(_))
                    | Some(Expression::ArrowFunctionExpression(_))
            ) && let BindingPattern::BindingIdentifier(id) = &d.id
                && let Some(name) = id.symbol_id.get().and_then(|s| self.exported.get(&s))
            {
                self.hint = Some(name.clone());
            }
            oxc_ast_visit::walk::walk_variable_declarator(self, d);
        }

        fn visit_property_definition(&mut self, p: &PropertyDefinition<'a>) {
            if matches!(
                p.value,
                Some(Expression::FunctionExpression(_))
                    | Some(Expression::ArrowFunctionExpression(_))
            ) {
                self.hint = key_name(&p.key);
            }
            oxc_ast_visit::walk::walk_property_definition(self, p);
        }
    }

    let mut v = Funcs {
        scoping: semantic.scoping(),
        exported,
        out: Vec::new(),
        path: Vec::new(),
        hint: None,
        anon: 0,
    };
    v.visit_program(&parsed.program);
    v.out
}

/// A function that exists in both revisions under one name, and differs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FnChange {
    pub label: String,
    pub before: Func,
    pub after: Func,
}

/// What actually happened to a module's functions between two revisions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FnDiff {
    /// Same name, different program. The thing you want to read.
    pub changed: Vec<FnChange>,
    pub added: Vec<Func>,
    pub removed: Vec<Func>,
    /// Functions whose shape is unchanged, however they moved or were renamed.
    pub kept: usize,
    pub total: usize,
}

impl FnDiff {
    /// Whether anything real happened here.
    pub fn is_change(&self) -> bool {
        !self.changed.is_empty() || !self.added.is_empty() || !self.removed.is_empty()
    }
}

/// Match a module's functions across two revisions, by shape and then by name.
///
/// Shape first, as a multiset: a function that is the same program has not
/// changed no matter where it moved to or what its locals became, and moving is
/// most of what a minifier does. Only what is left over is matched by name,
/// which is why an unchanged function sharing a label with a changed one never
/// confuses the pairing.
pub fn diff_functions(before: &str, after: &str) -> FnDiff {
    let old = functions(before);
    let new = functions(after);

    let mut pool: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in old.iter().enumerate() {
        pool.entry(f.hash.as_str()).or_default().push(i);
    }

    let mut old_used = vec![false; old.len()];
    let mut leftover_new: Vec<&Func> = Vec::new();
    let mut d = FnDiff {
        total: new.len(),
        ..Default::default()
    };

    for f in &new {
        match pool.get_mut(f.hash.as_str()).and_then(|v| {
            let slot = v.iter().position(|&i| old[i].label == f.label).unwrap_or(v.len().saturating_sub(1));
            if v.is_empty() { None } else { Some(v.remove(slot)) }
        }) {
            Some(i) => {
                old_used[i] = true;
                d.kept += 1;
            }
            None => leftover_new.push(f),
        }
    }

    let mut leftover_old: HashMap<&str, Vec<&Func>> = HashMap::new();
    for (i, f) in old.iter().enumerate() {
        if !old_used[i] {
            leftover_old.entry(f.label.as_str()).or_default().push(f);
        }
    }

    let is_named = |f: &Func| !f.label.rsplit('/').next().unwrap_or("#").starts_with('#');

    let mut unnamed_new: Vec<&Func> = Vec::new();
    for f in leftover_new {
        // An anonymous label is a position, not a name: `#4` in one revision and
        // `#4` in the next are each the fourth unnamed function, which two
        // insertions earlier makes meaningless. Pairing on it directly would
        // manufacture changes out of nothing.
        if !is_named(f) {
            unnamed_new.push(f);
            continue;
        }
        match leftover_old.get_mut(f.label.as_str()).and_then(|v| v.pop()) {
            Some(prev) => d.changed.push(FnChange {
                label: f.label.clone(),
                before: prev.clone(),
                after: f.clone(),
            }),
            None => d.added.push(f.clone()),
        }
    }

    // What is left is unnamed on both sides. Reporting each as one addition
    // *and* one removal doubles the count and reads as far more churn than
    // happened — the module factory itself is unnamed, so every module that
    // changed at all was showing up twice. Pair them on the two things a
    // minifier leaves alone: which named function they sit inside, and how many
    // parameters they take. That is not identity and cannot be; it is enough to
    // stop one edit from being counted as two, and the label stays `#n` so
    // nobody mistakes the pairing for a name.
    let mut by_shape: HashMap<(&str, usize), Vec<&Func>> = HashMap::new();
    for rest in leftover_old.values() {
        for f in rest.iter().filter(|f| !is_named(f)) {
            let parent = f.label.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            by_shape.entry((parent, f.params)).or_default().push(f);
        }
    }
    for f in unnamed_new {
        let parent = f.label.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        match by_shape.get_mut(&(parent, f.params)).and_then(|v| v.pop()) {
            Some(prev) => d.changed.push(FnChange {
                label: f.label.clone(),
                before: prev.clone(),
                after: f.clone(),
            }),
            None => d.added.push(f.clone()),
        }
    }
    for rest in leftover_old.values() {
        for f in rest.iter().filter(|f| is_named(f)) {
            d.removed.push((*f).clone());
        }
    }
    for rest in by_shape.into_values() {
        for f in rest {
            d.removed.push(f.clone());
        }
    }

    narrow(&mut d);
    d.total = d.kept + d.changed.len() + d.added.len();

    // Biggest first: a 400-token function that changed is the release, a
    // 6-token one is a constant someone bumped.
    d.changed.sort_by_key(|c| std::cmp::Reverse(c.after.tokens));
    d.added.sort_by_key(|f| std::cmp::Reverse(f.tokens));
    d.removed.sort_by_key(|f| std::cmp::Reverse(f.tokens));
    d
}

/// Keep only the innermost thing that changed.
///
/// A function's shape includes its body, so editing one line inside a closure
/// marks that closure, the function around it, and the module factory around
/// that — every module reports its own factory as changed, on top of whatever
/// really happened. Reading a chain of ancestors tells you nothing the innermost
/// entry did not already say.
///
/// Containment comes from spans rather than labels, because an unnamed ancestor
/// is deliberately stripped out of its descendants' labels and cannot be
/// recovered from them. The two directions are resolved separately: `changed`
/// and `added` carry offsets into the new revision, `removed` into the old, and
/// comparing across the two would be comparing coordinates from different files.
fn narrow(d: &mut FnDiff) {
    let new_spans: Vec<_> = d.changed.iter().map(|c| (c.after.start, c.after.end))
        .chain(d.added.iter().map(|f| (f.start, f.end))).collect();
    let old_spans: Vec<_> = d.changed.iter().map(|c| (c.before.start, c.before.end))
        .chain(d.removed.iter().map(|f| (f.start, f.end))).collect();
    let contains = |s, e, spans: &[(u32, u32)]| spans.iter().any(|&(a,b)| (a,b) != (s,e) && s <= a && b <= e);
    let previous = d.changed.len();
    d.changed.retain(|c| !contains(c.after.start,c.after.end,&new_spans)
        && !contains(c.before.start,c.before.end,&old_spans));
    // Ancestors narrowed out of the report are structural overlap, not extra edits.
    d.kept += previous - d.changed.len();
    // Whole additions/removals retain their outer API, not only its callbacks.
    let added: Vec<_> = d.added.iter().map(|f| (f.start,f.end)).collect();
    d.added.retain(|f| !added.iter().any(|&(a,b)| (a,b)!=(f.start,f.end) && a<=f.start && f.end<=b));
    let removed: Vec<_> = d.removed.iter().map(|f| (f.start,f.end)).collect();
    d.removed.retain(|f| !removed.iter().any(|&(a,b)| (a,b)!=(f.start,f.end) && a<=f.start && f.end<=b));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the label: it names the one function that changed,
    /// while the minifier renames every local around it and moves the others.
    #[test]
    fn the_changed_function_is_named() {
        let before = r#"var e = {};
            e.encode = function(t, n) { return t + n };
            e.decode = function(r) { return r * 2 };
            e.check = function(o) { return o > 4 };"#;
        // Every local renamed, order shuffled, and `check` given a real edit.
        let after = r#"var u = {};
            u.decode = function(a) { return a * 2 };
            u.check = function(s) { return s > 5 };
            u.encode = function(l, c) { return l + c };"#;

        let d = diff_functions(before, after);
        assert_eq!(d.kept, 2, "encode and decode moved but did not change");
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].label, "check");
        assert!(d.added.is_empty() && d.removed.is_empty());
    }

    /// The real shape of a Metro module: functions declared under minified
    /// locals, published at the bottom. The local is noise and the export name
    /// is the contract, so the label has to come from the second, not the first.
    #[test]
    fn a_function_is_labelled_by_the_name_it_is_exported_under() {
        let before = r#"__d("M", ["dep"], (function(t, n, r, o, a, i, l) {
            function u(t) { return t + 1 }
            function c(e) { return e * 2 }
            l.encodeReceipt = u;
            l.decodeReceipt = c;
        }), 98)"#;
        // Same module, every local renamed by the minifier, and `decodeReceipt`
        // given a real edit.
        let after = r#"__d("M", ["dep"], (function(e, s, d, f, g, h, j) {
            function q(y) { return y + 1 }
            function w(z) { return z * 3 }
            j.encodeReceipt = q;
            j.decodeReceipt = w;
        }), 98)"#;

        let labels: Vec<_> = functions(before).into_iter().map(|f| f.label).collect();
        assert!(labels.contains(&"encodeReceipt".to_string()), "{labels:?}");

        let d = diff_functions(before, after);
        assert_eq!(d.changed.len(), 1, "one real edit: {:?}", d.changed);
        assert_eq!(d.changed[0].label, "decodeReceipt");
        assert!(
            d.added.is_empty() && d.removed.is_empty(),
            "+{:?} -{:?}",
            d.added,
            d.removed
        );
    }

    /// The module factory is itself unnamed. Reporting it as one addition and
    /// one removal doubled the count for every module that changed at all.
    #[test]
    fn the_unnamed_module_factory_is_one_change_not_two() {
        let before = r#"__d("M", ["dep"], (function(t, n, r, o, a, i, l) { l.x = 1 }), 98)"#;
        let after = r#"__d("M", ["dep"], (function(t, n, r, o, a, i, l) { l.x = 2 }), 98)"#;
        let d = diff_functions(before, after);
        assert_eq!(d.changed.len(), 1, "{d:?}");
        assert!(d.added.is_empty() && d.removed.is_empty(), "{d:?}");
    }

    /// A closure inside a named function inherits the path, so a change deep
    /// inside `encode` is still reported as being in `encode`.
    #[test]
    fn nested_closures_carry_the_path() {
        let src =
            r#"var e = {}; e.encode = function(t) { return t.map(function(n) { return n + 1 }) };"#;
        let labels: Vec<_> = functions(src).into_iter().map(|f| f.label).collect();
        assert!(labels.contains(&"encode".to_string()), "{labels:?}");
        assert!(
            labels.iter().any(|l| l.starts_with("encode/")),
            "{labels:?}"
        );
    }

    /// Anonymous functions are numbered by position, and a position is not a
    /// name. Pairing on it would turn one insertion into a wall of changes.
    #[test]
    fn anonymous_functions_are_never_paired_by_position() {
        let before = r#"f(function(a) { return a }, function(b) { return b * 2 });"#;
        // One function inserted at the front: every ordinal after it shifts.
        let after = r#"f(function(z) { return z + 9 }, function(a) { return a }, function(b) { return b * 2 });"#;
        let d = diff_functions(before, after);
        assert_eq!(d.kept, 2, "the two originals are unchanged");
        assert_eq!(d.changed.len(), 0, "an ordinal shift is not a change");
        assert_eq!(d.added.len(), 1);
    }

    /// A minifier inserting one binding renames everything after it. This is the
    /// exact cascade seen in `WAWebWid` between two real revisions.
    #[test]
    fn a_rename_cascade_is_not_a_change() {
        let before = r#"var h = "a", y = /x/, C = 99, b = 4; function f(){ return C + b }"#;
        let after = r#"var y = "a", C = /x/, b = 99, v = 4; function g(){ return b + v }"#;
        assert_eq!(compare(before, after), Verdict::RenamedOnly);
    }

    /// The same cascade, plus one real edit buried in it.
    #[test]
    fn a_change_hiding_inside_a_rename_is_still_found() {
        let before = r#"var h = "a", y = /x/, C = 99, b = 4; function f(){ return C + b }"#;
        let after = r#"var y = "a", C = /x/, b = 99, v = 5; function g(){ return b + v }"#;
        assert_eq!(compare(before, after), Verdict::Changed);
    }

    /// Every category of edit that must survive normalization.
    #[test]
    fn real_edits_are_all_detected() {
        let base = r#"function f(a, b) { if (a > b) { return g(a, "pin"); } return 0 }"#;
        for (what, edited) in [
            (
                "a literal",
                r#"function f(a, b) { if (a > b) { return g(a, "pin_v1"); } return 0 }"#,
            ),
            (
                "an operator",
                r#"function f(a, b) { if (a >= b) { return g(a, "pin"); } return 0 }"#,
            ),
            (
                "an argument",
                r#"function f(a, b) { if (a > b) { return g(a); } return 0 }"#,
            ),
            (
                "a callee",
                r#"function f(a, b) { if (a > b) { return h(a, "pin"); } return 0 }"#,
            ),
            (
                "a statement",
                r#"function f(a, b) { if (a > b) { return g(a, "pin"); } }"#,
            ),
            (
                "a property",
                r#"function f(a, b) { if (a > b) { return g.x(a, "pin"); } return 0 }"#,
            ),
            (
                "order",
                r#"function f(a, b) { if (a > b) { return g("pin", a); } return 0 }"#,
            ),
        ] {
            assert_eq!(compare(base, edited), Verdict::Changed, "missed: {what}");
        }
    }

    /// Renaming a *parameter* is still only a rename.
    #[test]
    fn parameters_and_nested_scopes_normalize() {
        let before = r#"function f(e, t) { return function (n) { return e + t + n } }"#;
        let after = r#"function q(a, b) { return function (c) { return a + b + c } }"#;
        assert_eq!(compare(before, after), Verdict::RenamedOnly);
    }

    /// A global is not ours to rename, so a different one is a different program.
    #[test]
    fn free_identifiers_are_never_normalized() {
        let before = r#"function f() { return Promise.resolve(1) }"#;
        let after = r#"function f() { return Symbol.resolve(1) }"#;
        assert_eq!(compare(before, after), Verdict::Changed);
    }

    /// `!0`/`!1` are how a minifier spells booleans, and it changes its mind.
    #[test]
    fn boolean_encodings_fold() {
        assert_eq!(
            compare("var a = !0, b = !1;", "var a = true, b = false;"),
            Verdict::RenamedOnly
        );
        // But a flipped boolean is a real change.
        assert_eq!(compare("var a = !0;", "var a = !1;"), Verdict::Changed);
    }
}

/// Prove the classifier on real code, rather than on examples chosen to pass.
///
/// Hand-written cases test what the author thought of. This takes modules out of
/// the actual bundle and mutates them two ways, where the right answer is known
/// by construction:
///
/// * **renames** — every local given a fresh name. The program is identical, so
///   anything but `RenamedOnly` is a false alarm: noise the engine failed to
///   absorb, which is the failure that makes a release unreviewable.
/// * **edits** — one literal, operator, or argument altered. The program is
///   different, so anything but `Changed` is a miss: a real change reported as
///   noise, which is the failure that ships a bug.
///
/// The two error rates are not symmetric in cost and are returned separately.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FuzzReport {
    /// Renamed mutants correctly absorbed.
    pub renames_absorbed: u32,
    /// Renamed mutants reported as changes. Noise the engine failed to absorb,
    /// which is the failure that makes a release unreviewable.
    pub false_alarms: u32,
    /// Edited mutants correctly caught.
    pub edits_caught: u32,
    /// Edited mutants reported as noise. The failure that ships a bug.
    pub misses: u32,
    /// Mutations that found nothing to change in a module and were skipped.
    pub skipped: u32,
    pub modules: u32,
}

/// The two error rates are not symmetric in cost and are returned separately.
pub fn fuzz(src: &dyn SourceLoader, entries: &[ModuleEntry], limit: usize) -> FuzzReport {
    use regex::Regex;
    let bundle = src;

    // Rename every binding, using the symbol table.
    //
    // The first two attempts did this textually and both were wrong in the same
    // way: `\b[a-z]\b` cannot tell a binding from a property key or a character
    // inside a regex, so it produced mutants that really *were* different
    // programs and then blamed the classifier for saying so. Distinguishing
    // those two things is the entire problem — a harness that cannot do it is
    // not measuring the engine, it is measuring itself.
    //
    // `rename_locals` renames exactly the identifiers that resolve to a symbol
    // declared in the module, so the result is α-equivalent by construction and
    // any verdict other than `RenamedOnly` is a genuine miss.
    type Mutation = (&'static str, fn(&str) -> Option<String>);
    let edits: [Mutation; 3] = [
        ("literal", |s| {
            let re = Regex::new(r#""([A-Za-z_][A-Za-z0-9_]{3,})""#).ok()?;
            let c = re.captures(s)?;
            Some(s.replacen(&format!("\"{}\"", &c[1]), &format!("\"{}X\"", &c[1]), 1))
        }),
        ("operator", |s| {
            s.contains(" === ").then(|| s.replacen(" === ", " !== ", 1))
        }),
        ("number", |s| {
            let re = Regex::new(r"\b([1-9][0-9]{1,4})\b").ok()?;
            let c = re.captures(s)?;
            let n: u64 = c[1].parse().ok()?;
            Some(s.replacen(&c[1], &(n + 1).to_string(), 1))
        }),
    ];

    let (mut rn_ok, mut rn_bad) = (0u32, 0u32);
    let (mut ed_ok, mut ed_bad, mut ed_skip) = (0u32, 0u32, 0u32);

    let mut modules = 0u32;
    for e in entries.iter().take(limit) {
        let Ok(src) = bundle.load(e) else { continue };
        modules += 1;
        if shape(&src).is_none() {
            continue;
        }

        if let Some(renamed) = rename_locals(&src) {
            match compare(&src, &renamed) {
                Verdict::RenamedOnly | Verdict::Same => rn_ok += 1,
                _ => rn_bad += 1,
            }
        }

        for (_, edit) in &edits {
            match edit(&src) {
                Some(m) if m != src => match compare(&src, &m) {
                    Verdict::Changed => ed_ok += 1,
                    _ => ed_bad += 1,
                },
                _ => ed_skip += 1,
            }
        }
    }

    FuzzReport {
        renames_absorbed: rn_ok,
        false_alarms: rn_bad,
        edits_caught: ed_ok,
        misses: ed_bad,
        skipped: ed_skip,
        modules,
    }
}

/// An α-rename of every module-local binding, done through the symbol table.
///
/// Every identifier that resolves to a symbol declared in this module becomes
/// `q<n>`; everything else — property names, globals, string and regex bodies —
/// is left exactly as it was. The result is a different text and the same
/// program, by construction rather than by care.
///
/// Public because it is the only honest way to generate a known-rename: doing it
/// textually cannot tell `obj.a` from `var a`.
pub fn rename_locals(src: &str) -> Option<String> {
    let alloc = Allocator::default();
    let parsed = Parser::new(&alloc, src, SourceType::cjs()).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return None;
    }
    let semantic = SemanticBuilder::new()
        .with_build_nodes(true)
        .build(&parsed.program)
        .semantic;

    struct Spans<'s> {
        scoping: &'s Scoping,
        hits: Vec<(u32, u32, SymbolId)>,
    }
    impl<'a, 's> Visit<'a> for Spans<'s> {
        fn enter_node(&mut self, kind: AstKind<'a>) {
            match kind {
                AstKind::BindingIdentifier(b) => {
                    if let Some(id) = b.symbol_id.get() {
                        self.hits.push((b.span.start, b.span.end, id));
                    }
                }
                AstKind::IdentifierReference(r) => {
                    if let Some(id) = r
                        .reference_id
                        .get()
                        .and_then(|i| self.scoping.get_reference(i).symbol_id())
                    {
                        self.hits.push((r.span.start, r.span.end, id));
                    }
                }
                _ => {}
            }
        }
    }

    let mut v = Spans {
        scoping: semantic.scoping(),
        hits: Vec::new(),
    };
    v.visit_program(&parsed.program);
    struct Shorthand { spans: Vec<oxc_span::Span> }
    impl<'a> Visit<'a> for Shorthand {
        fn visit_object_property(&mut self, p: &ObjectProperty<'a>) {
            if p.shorthand { self.spans.push(p.span); }
            oxc_ast_visit::walk::walk_object_property(self,p);
        }
        fn visit_binding_property(&mut self, p: &BindingProperty<'a>) {
            if p.shorthand { self.spans.push(p.span); }
            oxc_ast_visit::walk::walk_binding_property(self,p);
        }
    }
    let mut short = Shorthand { spans: vec![] }; short.visit_program(&parsed.program);
    let excluded: std::collections::HashSet<_> = v.hits.iter().filter(|(a,b,_)|
        short.spans.iter().any(|s| s.start<=*a && *b<=s.end)).map(|(_,_,id)| *id).collect();
    v.hits.retain(|(_,_,id)| !excluded.contains(id));
    if v.hits.is_empty() {
        return None;
    }

    // Spliced back to front so earlier offsets stay valid.
    v.hits.sort_by_key(|(start, ..)| std::cmp::Reverse(*start));
    let mut out = src.to_string();
    for (start, end, id) in v.hits {
        out.replace_range(start as usize..end as usize, &format!("q{}", id.index()));
    }
    Some(out)
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    #[test]
    fn behavior_fields_are_not_renames() {
        for (a,b) in [
            ("var a=1", "const a=1"),
            ("function a(){return 1}", "async function a(){return 1}"),
            ("function a(){return 1}", "function* a(){return 1}"),
            ("var a=`old`", "var a=`new`"),
            ("a.b()", "a?.b()"),
            ("var a={get x(){return 1}}", "var a={x(){return 1}}"),
        ] { assert_eq!(compare(a,b),Verdict::Changed,"{a} -> {b}"); }
    }
    #[test]
    fn captures_and_duplicate_shapes_keep_the_edited_export() {
        let a="__d('M',[],function(g,r,d,l,m,e){var a=r('A'),b=r('B');function x(){return a()}function y(){return a()}e.A=x;e.B=y})";
        let b=a.replace("function y(){return a()}","function y(){return b()}");
        let d=diff_functions(a,&b);assert_eq!(d.changed.len(),1);assert_eq!(d.changed[0].label,"B");
        assert!(d.added.is_empty() && d.removed.is_empty());assert_eq!(d.total-d.kept,1);
    }
    #[test]
    fn injected_parameter_is_not_exports() {
        let a="__d('M',['fbt'],function(g,r,d,l,m,e,fbt){function x(){return 1}m.exports={};e.real=x;})";
        let d=diff_functions(a,&a.replace("return 1","return 2"));assert_eq!(d.changed[0].label,"real");
    }
    #[test]
    fn outer_additions_and_nested_deletions_are_localized() {
        let d=diff_functions("", "var e={};e.encode=function(){return [1].map(function(x){return x})}");
        assert_eq!(d.added.len(),1);assert_eq!(d.added[0].label,"encode");
        let a="var e={};e.encode=function(){function inner(){return 1}return 2}";
        let b="var e={};e.encode=function(){return 2}";
        let d=diff_functions(a,b);assert!(d.changed.is_empty());assert_eq!(d.removed.len(),1);
    }
    #[test]
    fn parser_diagnostics_and_undefined_are_honest() {
        assert_eq!(compare("var a=;", "var b=;"), Verdict::Unknown);
        assert_eq!(compare("var a=void 0", "var b=undefined"),Verdict::RenamedOnly);
        assert_eq!(compare("function f(undefined){return void 0}", "function f(undefined){return undefined}"),Verdict::Changed);
    }
    #[test]
    fn fuzz_does_not_rename_shorthand_keys() {
        let a="var a=1,b=2;use({a},b)";let b=rename_locals(a).unwrap();
        assert!(b.contains("{a}"));assert_eq!(compare(a,&b),Verdict::RenamedOnly);
        let a="var {x}=obj;use(x)";assert!(rename_locals(a).is_none());
    }
}
