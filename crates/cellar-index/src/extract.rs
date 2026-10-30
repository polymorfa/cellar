//! Reading structure out of one `__d(...)` module with the oxc AST.
//!
//! Everything here is deliberately structural rather than textual. The bundle is
//! minified, so identifiers are single letters and the same three characters mean
//! different things in different modules: a regex for `e\.(\w+)\s*=` finds export
//! assignments in one module and unrelated local writes in the next. Resolving the
//! factory's *parameters* instead makes "assignment onto exports" a question
//! about a binding, and it stays correct whatever the minifier named it — as
//! long as the right parameter is picked, which is not stock Metro's. See
//! [`module_and_exports`].

use oxc_ast::ast::{
    Argument, AssignmentTarget, BindingPattern, Expression, Function, Program, Statement,
};
use oxc_ast_visit::{Visit, walk};
use oxc_semantic::{Scoping, SymbolId, SemanticBuilder};

/// The Metro module-definition function.
pub const DEFINE_FN: &str = "__d";

/// Where `module` and `exports` sit, for a factory of this many parameters.
///
/// Stock Metro passes seven parameters and puts `exports` fifth. This bundle is
/// Meta's own fork and does neither. A small number of its modules ship
/// unminified and spell the signature out, which is where these come from — over
/// one revision, 135 modules name their parameters, in two shapes:
///
/// ```text
/// 133×  (global, require, requireDynamic, requireLazy, module, exports)
///   2×  (global, require, importDefault, importNamespace, requireLazy, module, exports, …)
/// ```
///
/// Anything after `exports` in the long form is a dependency the loader injects
/// directly — the two unminified examples receive `invariant` and `fbt` that way
/// — so `exports` is *not* the last parameter, and treating it as one silently
/// loses the exports of every `.react` module, about a fifth of the bundle.
fn module_and_exports(arity: usize) -> Option<(usize, usize)> {
    match arity {
        6 => Some((4, 5)),
        7.. => Some((5, 6)),
        _ => None,
    }
}

/// What one module declares.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ModuleFacts {
    pub name: String,
    /// Dependency names in declaration order — positional, matching `d[i]`.
    pub deps: Vec<String>,
    /// Names assigned onto the exports object, sorted and deduplicated.
    pub exports: Vec<String>,
    /// Named functions declared at the top level of the factory, sorted.
    pub functions: Vec<String>,
}

/// Pull the facts out of a program that consists of a single `__d(...)` call.
///
/// Returns `None` when the program is not a module definition, which is a real
/// case (bundle bootstrap code) and not an error.
pub fn facts_of_module(program: &Program<'_>) -> Option<ModuleFacts> {
    let semantic = SemanticBuilder::new().build(program).semantic;
    let scoping = semantic.scoping();
    for stmt in &program.body {
        let Statement::ExpressionStatement(es) = stmt else {
            continue;
        };
        let Expression::CallExpression(call) = &es.expression else {
            continue;
        };
        let Expression::Identifier(callee) = &call.callee else {
            continue;
        };
        if callee.name.as_str() != DEFINE_FN || call.arguments.len() < 3 {
            continue;
        }

        let Some(Expression::StringLiteral(name)) = call.arguments[0].as_expression() else {
            continue;
        };

        let mut facts = ModuleFacts {
            name: name.value.to_string(),
            ..Default::default()
        };

        if let Some(Expression::ArrayExpression(arr)) = call.arguments[1].as_expression() {
            for el in &arr.elements {
                if let Some(Expression::StringLiteral(s)) = el.as_expression() {
                    facts.deps.push(s.value.to_string());
                }
            }
        }

        if let Some(factory) = factory_function(&call.arguments[2]) {
            let mut scan = FactoryScan::new(factory, scoping);
            if let Some(body) = &factory.body {
                scan.visit_function_body(body);
                scan.collect_top_level(body);
            }
            facts.exports = sorted_unique(scan.exports);
            facts.functions = sorted_unique(scan.functions);
        }

        return Some(facts);
    }
    None
}

fn sorted_unique(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

fn factory_function<'b, 'a>(arg: &'b Argument<'a>) -> Option<&'b Function<'a>> {
    // `get_inner_expression`, not a bare match: the bundle writes the factory
    // parenthesized — `__d("M", [], (function(…){…}), 98)` — and oxc preserves
    // parentheses in the AST, so matching `FunctionExpression` directly saw a
    // `ParenthesizedExpression` and gave up. Name and deps come from arguments
    // 0 and 1 and were unaffected, so every module still indexed and looked
    // fine; `exports` and `functions` were simply empty for all 187,892 of
    // them, which reads exactly like "this module exports nothing".
    match arg.as_expression()?.get_inner_expression() {
        Expression::FunctionExpression(f) => Some(f),
        _ => None,
    }
}

fn symbol(scoping: &Scoping, id: &oxc_ast::ast::IdentifierReference<'_>) -> Option<SymbolId> {
    id.reference_id.get().and_then(|id| scoping.get_reference(id).symbol_id())
}

/// Which identifiers have properties assigned onto them, split by whether the
/// property was `exports`.
struct Assigned<'s> {
    scoping: &'s Scoping,
    /// `X.exports = …` — X is the module object.
    dot_exports: Vec<SymbolId>,
    /// `X.anythingElse = …` — X is a candidate exports object.
    other: Vec<SymbolId>,
}

impl<'a> Visit<'a> for Assigned<'_> {
    fn visit_assignment_expression(&mut self, e: &oxc_ast::ast::AssignmentExpression<'a>) {
        if let Some(member) = e.left.as_member_expression()
            && let Some(prop) = member.static_property_name()
        {
            match member.object() {
                Expression::Identifier(obj) if prop == "exports" => {
                    if let Some(id) = symbol(self.scoping,obj) { self.dot_exports.push(id) }
                }
                Expression::Identifier(obj) => if let Some(id) = symbol(self.scoping,obj) { self.other.push(id) },
                // `X.exports.prop = …`. The object is itself a member
                // expression, so the plain identifier case never sees it, and
                // without this the one form that most clearly identifies the
                // module object was the one form that identified nothing.
                other => {
                    if let Some(inner) = other.as_member_expression()
                        && inner.static_property_name() == Some("exports")
                        && let Expression::Identifier(obj) = inner.object()
                    {
                        if let Some(id) = symbol(self.scoping,obj) { self.dot_exports.push(id) };
                    }
                }
            }
        }
        walk::walk_assignment_expression(self, e);
    }
}

struct FactoryScan<'s> {
    scoping: &'s Scoping,
    /// The binding the factory receives `exports` as, e.g. `e`.
    exports_binding: Option<SymbolId>,
    /// The binding the factory receives `module` as, e.g. `m`.
    module_binding: Option<SymbolId>,
    exports: Vec<String>,
    functions: Vec<String>,
}

impl<'s> FactoryScan<'s> {
    /// Decide which parameters are `module` and `exports`, in that order of
    /// confidence: what the factory calls them, what it does with them, and only
    /// then where they sit.
    ///
    /// Position is the weakest of the three and is why this is not simply a pair
    /// of constants. Behaviour is a fact about *this* module — the exports
    /// object is the one with properties hung off it, and `module` is the one
    /// carrying `.exports` — so it stays right across bundler versions and
    /// across the two conventions this bundle mixes.
    fn new(factory: &Function<'_>, scoping: &'s Scoping) -> Self {
        let named = |want: &str| {
            factory.params.items.iter().find_map(|p| match &p.pattern {
                BindingPattern::BindingIdentifier(id) if id.name == want => id.symbol_id.get(),
                _ => None,
            })
        };

        let params: Vec<Option<SymbolId>> = factory
            .params
            .items
            .iter()
            .map(|p| match &p.pattern {
                BindingPattern::BindingIdentifier(id) => id.symbol_id.get(),
                _ => None,
            })
            .collect();

        let mut assigned = Assigned { scoping, dot_exports: vec![], other: vec![] };
        if let Some(body) = &factory.body {
            assigned.visit_function_body(body);
        }
        let slot = |id: &SymbolId| params.iter().position(|p| *p == Some(*id));

        // `X.exports = …` identifies `module`, never `exports`.
        let by_module = assigned.dot_exports.iter().filter_map(slot).max();
        // Everything else identifies `exports`. The later slot wins, because
        // `module` precedes `exports` in every convention and a module writing
        // to both is exporting through both.
        let by_exports = assigned.other.iter().filter_map(slot).max();

        let (m, e) =
            module_and_exports(factory.params.items.len()).unwrap_or((usize::MAX, usize::MAX));
        Self {
            scoping,
            exports_binding: named("exports")
                // `exports` always immediately follows `module`, so knowing one
                // gives the other. This matters because arity cannot separate
                // the six-parameter convention plus an injected dependency from
                // the seven-parameter convention — `GeminiEmojiCategories` is
                // seven parameters of the former, and reading it by arity looks
                // for exports on the injected `fbt` and finds none.
                .or_else(|| by_module.and_then(|i| params.get(i + 1).copied().flatten()))
                .or_else(|| by_exports.and_then(|i| params[i]))
                .or_else(|| params.get(e).copied().flatten()),
            module_binding: named("module")
                .or_else(|| by_module.and_then(|i| params[i]))
                .or_else(|| params.get(m).copied().flatten()),
            exports: Vec::new(),
            functions: Vec::new(),
        }
    }

    /// Top-level declarations of the factory body — the module's own functions,
    /// as opposed to every closure nested anywhere inside it.
    fn collect_top_level(&mut self, body: &oxc_ast::ast::FunctionBody<'_>) {
        for stmt in &body.statements {
            match stmt {
                Statement::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        self.functions.push(id.name.to_string());
                    }
                }
                Statement::VariableDeclaration(decl) => {
                    for d in &decl.declarations {
                        let BindingPattern::BindingIdentifier(id) = &d.id else {
                            continue;
                        };
                        let is_callable = matches!(
                            d.init,
                            Some(Expression::FunctionExpression(_))
                                | Some(Expression::ArrowFunctionExpression(_))
                                | Some(Expression::ClassExpression(_))
                        );
                        if is_callable {
                            self.functions.push(id.name.to_string());
                        }
                    }
                }
                Statement::ClassDeclaration(c) => {
                    if let Some(id) = &c.id {
                        self.functions.push(id.name.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    /// `<exports>.name = …` → `name`; `<module>.exports = …` → `default`;
    /// `<module>.exports.name = …` → `name`.
    fn record_export_target(&mut self, target: &AssignmentTarget<'_>) {
        let Some(member) = target.as_member_expression() else {
            return;
        };
        let Some(prop) = member.static_property_name() else {
            return;
        };

        // `<exports>.prop = …`
        if let Expression::Identifier(obj) = member.object()
            && symbol(self.scoping,obj).is_some() && symbol(self.scoping,obj) == self.exports_binding
        {
            self.exports.push(prop.to_string());
            return;
        }

        // `<module>.exports = …`
        if let Expression::Identifier(obj) = member.object()
            && symbol(self.scoping,obj).is_some() && symbol(self.scoping,obj) == self.module_binding
            && prop == "exports"
        {
            self.exports.push("default".to_string());
            return;
        }

        // `<module>.exports.prop = …`
        if let Some(inner) = member.object().as_member_expression()
            && inner.static_property_name() == Some("exports")
            && let Expression::Identifier(obj) = inner.object()
            && symbol(self.scoping,obj).is_some() && symbol(self.scoping,obj) == self.module_binding
        {
            self.exports.push(prop.to_string());
        }
    }
}

impl<'a> Visit<'a> for FactoryScan<'_> {
    fn visit_assignment_expression(&mut self, expr: &oxc_ast::ast::AssignmentExpression<'a>) {
        self.record_export_target(&expr.left);
        walk::walk_assignment_expression(self, expr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    fn facts(src: &str) -> ModuleFacts {
        let alloc = Allocator::default();
        let ret = Parser::new(&alloc, src, SourceType::cjs()).parse();
        facts_of_module(&ret.program).expect("a module definition")
    }

    #[test]
    fn block_shadow_and_captured_exports_are_distinct() {
        let f=facts("__d('M',[],function(g,r,i,n,l,m,e){ {let e={};e.tmp=1} function f(){e.real=1} })");
        assert_eq!(f.exports,vec!["real"]);
    }
    #[test]
    fn nested_parameter_shadow_does_not_select_exports() {
        let f=facts("__d('M',[],function(g,r,i,n,l,m,e){function f(e){e.tmp=1}m.real=1})");
        assert_eq!(f.exports,vec!["real"]);
    }
    #[test]
    fn reads_name_and_positional_deps() {
        let f = facts(r#"__d("WAWebFoo",["A","B"],function(g,r,i,a,m,e,d){},1);"#);
        assert_eq!(f.name, "WAWebFoo");
        assert_eq!(f.deps, ["A", "B"], "dependency order is positional");
    }

    #[test]
    fn exports_are_resolved_through_the_factory_binding_not_a_letter() {
        // The minifier named exports `q` here; a regex keyed on `e.` finds nothing,
        // and worse, would pick up the unrelated `e.detail` write.
        let f = facts(
            r#"__d("M",[],function(g,r,i,a,mod,q,d){
                 function handler(e){ e.detail = 1; }
                 q.sendStanza = handler;
                 q.parse = function(){};
               },1);"#,
        );
        assert_eq!(f.exports, ["parse", "sendStanza"]);
    }

    /// The shape the bundle actually ships, which no fixture here had.
    ///
    /// Two things differ from a hand-written example and each was enough on its
    /// own to return nothing: the factory is *parenthesized*, and `exports` is
    /// the seventh parameter rather than Metro's fifth. Every one of the 187,892
    /// modules in a real bundle indexed with an empty `exports` list, which
    /// reads exactly like "this module exports nothing".
    #[test]
    fn the_real_bundle_shape_is_read() {
        let f = facts(
            r#"__d("WAWebCryptoMediaRetry", ["WABinary"], (function(t, n, r, o, a, i, l) {
                 function u(t) { return t; }
                 l.encryptServerErrorReceipt = u, l.ENC_IV_SIZE = 12;
               }), 98);"#,
        );
        assert_eq!(f.name, "WAWebCryptoMediaRetry");
        assert_eq!(f.exports, ["ENC_IV_SIZE", "encryptServerErrorReceipt"]);
        assert_eq!(f.functions, ["u"], "the factory body was reached at all");
    }

    /// A `.react` module: the loader injects dependencies *after* `exports`, so
    /// the last parameter is `fbt` and taking it as exports finds nothing. About
    /// a fifth of the bundle looks like this.
    #[test]
    fn injected_dependencies_after_exports_do_not_displace_it() {
        let f = facts(
            r#"__d("M.react", [], (function(t, n, r, o, a, i, l, s) {
                 l.Component = function() { return s.render(); };
               }), 98);"#,
        );
        assert_eq!(f.exports, ["Component"]);
    }

    /// Seven parameters, but the *six*-parameter convention plus one injected
    /// dependency — which arity alone cannot distinguish from the seven-parameter
    /// convention. `module.exports` settles it, and `exports` follows `module`.
    /// Taken from `GeminiEmojiCategories`.
    #[test]
    fn six_parameters_plus_an_injected_dependency_is_not_the_long_convention() {
        let f = facts(
            r#"__d("M", ["fbt"], (function(t, n, r, o, a, i, l) {
                 a.exports = { name: l._("") };
                 i.helper = 1;
               }), 98);"#,
        );
        // `a` is module, so `i` is exports — not `l`, which is the injected fbt.
        assert_eq!(f.exports, ["default", "helper"]);
    }

    /// The other convention: six parameters, `exports` fifth. Both shapes appear
    /// in one bundle, so neither may be assumed.
    #[test]
    fn the_six_parameter_convention_is_read_too() {
        let f =
            facts(r#"__d("M", [], (function(t, n, r, o, a, i) { i.CollectionName = 1; }), 98);"#);
        assert_eq!(f.exports, ["CollectionName"]);
    }

    #[test]
    fn module_exports_forms_are_recognised() {
        let f = facts(r#"__d("M",[],function(g,r,i,a,m,e,d){ m.exports = {}; },1);"#);
        assert_eq!(f.exports, ["default"]);

        let f = facts(r#"__d("M",[],function(g,r,i,a,m,e,d){ m.exports.parse = 1; },1);"#);
        assert_eq!(f.exports, ["parse"]);
    }

    #[test]
    fn functions_are_top_level_only() {
        let f = facts(
            r#"__d("M",[],function(g,r,i,a,m,e,d){
                 function outer(){ function inner(){} }
                 var arrow = () => {};
                 var notAFunction = 42;
                 class Thing {}
               },1);"#,
        );
        assert_eq!(
            f.functions,
            ["Thing", "arrow", "outer"],
            "nested `inner` is not the module's own surface"
        );
    }

    #[test]
    fn a_non_module_program_is_not_an_error() {
        let alloc = Allocator::default();
        let ret = Parser::new(&alloc, "window.__bootstrap = 1;", SourceType::cjs()).parse();
        assert!(facts_of_module(&ret.program).is_none());
    }

    #[test]
    fn a_factory_with_too_few_params_still_yields_name_and_deps() {
        let f = facts(r#"__d("M",["A"],function(){},1);"#);
        assert_eq!(f.name, "M");
        assert_eq!(f.deps, ["A"]);
        assert!(f.exports.is_empty());
    }
}
