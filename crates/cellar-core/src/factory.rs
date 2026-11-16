//! Which parameters of a Metro module factory are `module` and `exports`.
//!
//! Shared by the indexer, which lists a module's exports, and the shape engine,
//! which labels functions by the name they are exported under. The two used to
//! carry separate copies of this decision and drifted; one answer keeps the
//! export list and the function labels describing the same object.

use oxc_ast::ast::{AssignmentExpression, BindingPattern, Expression, Function};
use oxc_ast_visit::{Visit, walk};
use oxc_semantic::{Scoping, SymbolId};

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
fn by_arity(arity: usize) -> Option<(usize, usize)> {
    match arity {
        6 => Some((4, 5)),
        7.. => Some((5, 6)),
        _ => None,
    }
}

/// The first slot `module` can occupy in either convention. Earlier slots are
/// `global`, `require` and loader helpers, and a property written onto one of
/// those (`global.__DEV__ = …`) says nothing about the module's exports.
const FIRST_MODULE_SLOT: usize = 4;

/// The factory's `module` and `exports` bindings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FactoryBindings {
    pub module: Option<SymbolId>,
    pub exports: Option<SymbolId>,
}

fn symbol(scoping: &Scoping, id: &oxc_ast::ast::IdentifierReference<'_>) -> Option<SymbolId> {
    id.reference_id
        .get()
        .and_then(|id| scoping.get_reference(id).symbol_id())
}

/// Which bindings have properties assigned onto them, split by whether the
/// property was `exports`. Resolved through the symbol table, so a nested
/// binding that shadows a factory parameter is a different symbol and never
/// counts for the parameter.
struct Assigned<'s> {
    scoping: &'s Scoping,
    /// `X.exports = …` and `X.exports.y = …` — X is the module object.
    dot_exports: Vec<SymbolId>,
    /// `X.anythingElse = …` — X is a candidate exports object.
    other: Vec<SymbolId>,
}

impl<'a> Visit<'a> for Assigned<'_> {
    fn visit_assignment_expression(&mut self, e: &AssignmentExpression<'a>) {
        if let Some(member) = e.left.as_member_expression()
            && let Some(prop) = member.static_property_name()
        {
            match member.object() {
                Expression::Identifier(obj) if prop == "exports" => {
                    if let Some(id) = symbol(self.scoping, obj) {
                        self.dot_exports.push(id);
                    }
                }
                Expression::Identifier(obj) => {
                    if let Some(id) = symbol(self.scoping, obj) {
                        self.other.push(id);
                    }
                }
                // `X.exports.prop = …`. The object is itself a member
                // expression, so the plain identifier case never sees it, and
                // without this the one form that most clearly identifies the
                // module object was the one form that identified nothing.
                other => {
                    if let Some(inner) = other.as_member_expression()
                        && inner.static_property_name() == Some("exports")
                        && let Expression::Identifier(obj) = inner.object()
                        && let Some(id) = symbol(self.scoping, obj)
                    {
                        self.dot_exports.push(id);
                    }
                }
            }
        }
        walk::walk_assignment_expression(self, e);
    }
}

/// Decide which parameters are `module` and `exports`, in that order of
/// confidence: what the factory calls them, what it does with them, and only
/// then where they sit.
///
/// Position is the weakest of the three and is why this is not simply a pair of
/// constants. Behaviour is a fact about *this* module — the exports object is
/// the one with properties hung off it, and `module` is the one carrying
/// `.exports` — so it stays right across bundler versions and across the two
/// conventions this bundle mixes. Behaviour only counts on slots where `module`
/// or `exports` can sit at all.
pub fn module_and_exports(factory: &Function<'_>, scoping: &Scoping) -> FactoryBindings {
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

    let mut assigned = Assigned {
        scoping,
        dot_exports: vec![],
        other: vec![],
    };
    if let Some(body) = &factory.body {
        assigned.visit_function_body(body);
    }
    let slot = |id: &SymbolId| {
        params
            .iter()
            .position(|p| *p == Some(*id))
            .filter(|&i| i >= FIRST_MODULE_SLOT)
    };

    // `X.exports = …` identifies `module`, never `exports`.
    let by_module = assigned.dot_exports.iter().filter_map(slot).max();
    // Everything else identifies `exports`. The later slot wins, because
    // `module` precedes `exports` in every convention and a module writing to
    // both is exporting through both.
    let by_exports = assigned.other.iter().filter_map(slot).max();

    let (m, e) = by_arity(params.len()).unwrap_or((usize::MAX, usize::MAX));
    FactoryBindings {
        exports: named("exports")
            // `exports` always immediately follows `module`, so knowing one
            // gives the other. This matters because arity cannot separate the
            // six-parameter convention plus an injected dependency from the
            // seven-parameter convention — `GeminiEmojiCategories` is seven
            // parameters of the former, and reading it by arity looks for
            // exports on the injected `fbt` and finds none.
            .or_else(|| by_module.and_then(|i| params.get(i + 1).copied().flatten()))
            .or_else(|| by_exports.and_then(|i| params[i]))
            .or_else(|| params.get(e).copied().flatten()),
        module: named("module")
            .or_else(|| by_module.and_then(|i| params[i]))
            .or_else(|| params.get(m).copied().flatten()),
    }
}
