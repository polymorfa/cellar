//! Run the module extractor over real bundle files and report coverage.
//!
//! Exists because the unit fixtures were all hand-written and all wrong in the
//! same two ways — unparenthesized factories, stock Metro's parameter order —
//! so they passed while every real module returned nothing. A synthetic test
//! proves the code does what its author expected; this proves it does what the
//! bundle needs.

use std::env;
use std::fs;

fn main() {
    let (mut total, mut with_exports, mut with_functions) = (0usize, 0usize, 0usize);

    for path in env::args().skip(1) {
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let alloc = oxc_allocator::Allocator::default();
        let parsed = oxc_parser::Parser::new(&alloc, &src, oxc_span::SourceType::cjs()).parse();
        let Some(facts) = cellar_index::extract::facts_of_module(&parsed.program) else {
            continue;
        };
        total += 1;
        if !facts.exports.is_empty() {
            with_exports += 1;
        }
        if !facts.functions.is_empty() {
            with_functions += 1;
        }
    }

    let pct = |n: usize| {
        n.checked_mul(100)
            .and_then(|x| x.checked_div(total))
            .unwrap_or(0)
    };
    println!("{total} modules");
    println!("  with exports:   {with_exports} ({}%)", pct(with_exports));
    println!(
        "  with functions: {with_functions} ({}%)",
        pct(with_functions)
    );
}
