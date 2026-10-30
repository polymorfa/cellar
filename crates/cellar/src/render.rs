//! Human-readable renderings of the JSON that [`crate::ops`] returns.
//!
//! These are views, not queries: every renderer takes the same value the `--json`
//! form would have printed, so the two can never drift apart or disagree.

use anyhow::Result;
use serde_json::Value;

/// Print `value` as JSON, or hand it to `renderer`.
pub fn emit(value: &Value, json: bool, renderer: fn(&Value)) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        renderer(value);
    }
    Ok(())
}

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn n(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map_or(&[], |a| a)
}

/// Bytes as a short human-scale string.
fn bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = b as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{b} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn bundle_added(v: &Value) {
    let m = &v["manifest"];
    println!("indexed {}", s(v, "bundle"));
    println!("  modules:  {}", n(m, "modulesIndexed"));
    println!(
        "  sources:  {} ({})",
        s(v, "modulesDir"),
        bytes(n(m, "modulesBytes"))
    );
    diagnostics_lines(&m["diagnostics"], "  ");
}

pub fn bundle_list(v: &Value) {
    let bundles = arr(v, "bundles");
    if bundles.is_empty() {
        println!("no bundles yet — `cellar bundle add --rev latest`");
        return;
    }
    println!("{:<24} {:>9}  {:<26} DIR", "BUNDLE", "MODULES", "INDEXED");
    for b in bundles {
        println!(
            "{:<24} {:>9}  {:<26} {}",
            s(b, "bundle"),
            n(b, "modules"),
            s(b, "indexedAt"),
            s(b, "dir")
        );
    }
}

pub fn bundle_info(v: &Value) {
    let m = &v["manifest"];
    println!("{}", s(v, "bundle"));
    println!("  dir:        {}", s(v, "dir"));
    println!(
        "  modules:    {} ({})",
        n(m, "modulesIndexed"),
        bytes(n(m, "modulesBytes"))
    );
    println!("  indexed at: {}", s(m, "indexedAt"));
    println!("  form:       {}", s(m, "sourceForm"));
    if let Some(url) = m.get("sourceUrl").and_then(Value::as_str) {
        println!("  source:     {url}");
    }
    if let Some(sha) = m.get("archiveSha256").and_then(Value::as_str) {
        println!("  archive:    sha256:{sha} ({})", bytes(n(m, "archiveLen")));
    }
    println!("  diagnostics:");
    diagnostics_lines(&m["diagnostics"], "    ");
}

/// Extraction counters. Printed in full rather than only when non-zero: an
/// all-zero block is the evidence that the extraction was clean, and hiding it
/// would make "clean" and "not reported" look the same.
fn diagnostics_lines(d: &Value, indent: &str) {
    println!(
        "{indent}chunks read {} | skipped {} | parse failures {}",
        n(d, "chunksRead"),
        n(d, "chunksSkipped"),
        n(d, "chunkParseFailures")
    );
    println!(
        "{indent}codegen fallbacks {} | renamed files {} | unresolved deps {}",
        n(d, "codegenFailures"),
        n(d, "renamedFiles"),
        n(d, "unresolvedDepsTotal")
    );
    let with_variants = d
        .get("modulesWithVariants")
        .and_then(Value::as_object)
        .map_or(0, |m| m.len());
    if with_variants > 0 {
        println!("{indent}modules shipping more than one definition: {with_variants}");
    }
}

pub fn filter_list(v: &Value) {
    println!(
        "{:<14} {:<8} {:<8} {:>4} {:>4} {:>4}  DESCRIPTION",
        "NAME", "SOURCE", "DEFAULT", "HARD", "INC", "EXC"
    );
    for f in arr(v, "filters") {
        let desc = s(f, "description");
        let desc: String = desc.split_whitespace().collect::<Vec<_>>().join(" ");
        let desc = if desc.len() > 60 {
            format!("{}…", &desc[..59])
        } else {
            desc
        };
        println!(
            "{:<14} {:<8} {:<8} {:>4} {:>4} {:>4}  {}",
            s(f, "name"),
            if f["builtin"].as_bool().unwrap_or(false) {
                "builtin"
            } else {
                "user"
            },
            s(f, "defaultVerdict"),
            n(f, "hardExclude"),
            n(f, "include"),
            n(f, "exclude"),
            desc
        );
    }
    println!("\nuser filters live in {}", s(v, "dir"));
}

pub fn filter_test(v: &Value) {
    let total = n(v, "total");
    let kept = n(v, "kept");
    let pct = if total > 0 {
        (kept as f64 / total as f64) * 100.0
    } else {
        0.0
    };
    println!(
        "filter `{}` on {}: keeps {kept} of {total} modules ({pct:.1}%)",
        s(v, "filter"),
        s(v, "bundle")
    );
    if let Some(breakdown) = v.get("dropBreakdown").and_then(Value::as_object) {
        for (reason, count) in breakdown {
            println!("  dropped by {reason}: {count}");
        }
    }
    println!("\nkept, for example:");
    for m in arr(v, "keptSample") {
        println!("  {}", m.as_str().unwrap_or_default());
    }
    println!("\ndropped, for example:");
    for d in arr(v, "droppedSample") {
        println!("  {:<50} {}", s(d, "module"), d["why"]);
    }
}

pub fn module_show(v: &Value) {
    let m = &v["module"];
    println!("{}", s(m, "name"));
    println!("  path:      {}", s(v, "path"));
    println!("  bundle:    {}", s(v, "bundle"));
    println!("  sha256:    {}", s(m, "rawSha256"));
    println!(
        "  size:      {} raw / {} stored",
        bytes(n(m, "rawLen")),
        bytes(n(m, "storedLen"))
    );
    println!("  deps:      {}", join(arr(m, "deps"), 12));
    println!("  dependents: {}", join(arr(m, "dependents"), 12));
    println!("  exports:   {}", join(arr(m, "exports"), 12));
    if let Some(src) = v.get("source").and_then(Value::as_str) {
        let start = n(v, "startLine").max(1);
        println!("\n--- source ---");
        for (i, line) in src.lines().enumerate() {
            println!("{:>6} │ {line}", start as usize + i);
        }
    }
}

fn join(values: &[Value], max: usize) -> String {
    if values.is_empty() {
        return "(none)".into();
    }
    let shown: Vec<&str> = values.iter().take(max).filter_map(Value::as_str).collect();
    if values.len() > max {
        format!("{} … (+{} more)", shown.join(", "), values.len() - max)
    } else {
        shown.join(", ")
    }
}

pub fn search(v: &Value) {
    let matched = n(v, "matched");
    println!(
        "{matched} module(s) matched of {} considered{}",
        n(v, "candidates"),
        if v["truncated"].as_bool().unwrap_or(false) {
            format!(" (showing {})", arr(v, "hits").len())
        } else {
            String::new()
        }
    );
    for hit in arr(v, "hits") {
        println!("\n{}  {}", s(hit, "name"), s(hit, "path"));
        let matches = arr(hit, "matches");
        if let Some(count) = hit.get("matchCount").and_then(Value::as_u64)
            && count as usize > matches.len()
        {
            println!("  ({count} matching lines, showing {})", matches.len());
        }
        for m in matches {
            for b in arr(m, "before") {
                println!("        │ {}", b.as_str().unwrap_or_default());
            }
            println!("  {:>5} │ {}", n(m, "line"), s(m, "text"));
            for a in arr(m, "after") {
                println!("        │ {}", a.as_str().unwrap_or_default());
            }
        }
    }
    let unreadable = arr(v, "unreadable");
    if !unreadable.is_empty() {
        println!("\n{} module(s) could not be read:", unreadable.len());
        for u in unreadable {
            println!("  {}", u.as_str().unwrap_or_default());
        }
    }
}

pub fn diff_text(v: &Value) {
    let sum = &v["summary"];
    println!(
        "{} -> {}  (filter `{}`)",
        s(v, "old"),
        s(v, "new"),
        s(v, "filter")
    );
    println!(
        "  considered {} of {} modules (old) / {} of {} (new)",
        n(sum, "oldKept"),
        n(sum, "oldTotal"),
        n(sum, "newKept"),
        n(sum, "newTotal")
    );
    println!(
        "  added {} | removed {} | modified {} | unchanged {}",
        n(sum, "added"),
        n(sum, "removed"),
        n(sum, "modified"),
        n(sum, "unchanged")
    );
    println!(
        "  suppressed as noise: {} | diffs skipped for size: {}",
        n(sum, "noiseOnly"),
        n(sum, "diffsSkipped")
    );

    if v["changesTruncated"].as_bool().unwrap_or(false) {
        println!(
            "\nshowing {} of {} changes",
            arr(v, "changes").len(),
            n(v, "changesTotal")
        );
    }

    for c in arr(v, "changes") {
        println!("\n[{}] {}", s(c, "kind"), s(c, "name"));
        let deps_added = arr(c, "depsAdded");
        let deps_removed = arr(c, "depsRemoved");
        if !deps_added.is_empty() {
            println!("  + deps: {}", join(deps_added, 20));
        }
        if !deps_removed.is_empty() {
            println!("  - deps: {}", join(deps_removed, 20));
        }
        if !arr(c, "exportsAdded").is_empty() {
            println!("  + exports: {}", join(arr(c, "exportsAdded"), 20));
        }
        if !arr(c, "exportsRemoved").is_empty() {
            println!("  - exports: {}", join(arr(c, "exportsRemoved"), 20));
        }
        if let Some(sim) = c.get("similarity").and_then(Value::as_f64) {
            println!(
                "  {:.1}% similar, +{} -{} lines",
                sim * 100.0,
                n(c, "linesAdded"),
                n(c, "linesRemoved")
            );
        }
        // The line counts above measure the text; this measures the program.
        // Printed second so the two read as claim and correction where they
        // disagree, which on a minified bundle is most of the time.
        if let Some(verdict) = c.get("verdict").and_then(Value::as_str) {
            match verdict {
                "renamedOnly" => {
                    println!("  renamed only — the text differs, the program does not")
                }
                "unknown" => {
                    println!("  unparseable, so no verdict is offered rather than guessed")
                }
                _ => {
                    if let Some(total) = c.get("functionsTotal").and_then(Value::as_u64) {
                        let kept = c.get("functionsKept").and_then(Value::as_u64).unwrap_or(0);
                        println!(
                            "  {} of {total} localized functions changed",
                            total.saturating_sub(kept)
                        );
                    }
                    for (label, key) in [
                        ("changed", "functionsChanged"),
                        ("added", "functionsAdded"),
                        ("removed", "functionsRemoved"),
                    ] {
                        let fns = arr(c, key);
                        if !fns.is_empty() {
                            println!("    {label}: {}", join(fns, 8));
                        }
                    }
                }
            }
        }
        if let Some(path) = c.get("newFile").and_then(Value::as_str) {
            println!("  new: {}/{}", s(v, "newDir"), path);
        }
        if let Some(why) = c.get("hunksOmitted").and_then(Value::as_str) {
            println!("  (no diff shown: {why})");
        }
        if let Some(h) = c.get("hunks").and_then(Value::as_str) {
            for line in h.lines() {
                println!("  {line}");
            }
        }
    }
}

/// One JSON object per line: the summary first, then one per change.
pub fn diff_ndjson(v: &Value) -> Result<()> {
    let mut head = v.clone();
    head["changes"] = Value::Array(vec![]);
    println!("{}", serde_json::to_string(&head)?);
    for c in arr(v, "changes") {
        println!("{}", serde_json::to_string(c)?);
    }
    Ok(())
}

pub fn diff_markdown(v: &Value) {
    let sum = &v["summary"];
    println!("# {} → {}\n", s(v, "old"), s(v, "new"));
    println!("Filter: `{}`\n", s(v, "filter"));
    println!("| metric | count |");
    println!("| --- | --- |");
    for (label, key) in [
        ("Added", "added"),
        ("Removed", "removed"),
        ("Modified", "modified"),
        ("Unchanged", "unchanged"),
        ("Noise-only (suppressed)", "noiseOnly"),
        ("Diffs skipped for size", "diffsSkipped"),
    ] {
        println!("| {label} | {} |", n(sum, key));
    }

    for kind in ["added", "removed", "modified"] {
        let group: Vec<&Value> = arr(v, "changes")
            .iter()
            .filter(|c| s(c, "kind") == kind)
            .collect();
        if group.is_empty() {
            continue;
        }
        println!("\n## {} ({})\n", kind, group.len());
        for c in group {
            println!("### `{}`\n", s(c, "name"));
            if !arr(c, "depsAdded").is_empty() {
                println!("- deps added: {}", join(arr(c, "depsAdded"), 20));
            }
            if !arr(c, "depsRemoved").is_empty() {
                println!("- deps removed: {}", join(arr(c, "depsRemoved"), 20));
            }
            if !arr(c, "exportsAdded").is_empty() {
                println!("- exports added: {}", join(arr(c, "exportsAdded"), 20));
            }
            if let Some(verdict) = c.get("verdict").and_then(Value::as_str) {
                println!("- semantic verdict: `{verdict}`");
                if let Some(total) = c.get("functionsTotal").and_then(Value::as_u64) {
                    let kept = c.get("functionsKept").and_then(Value::as_u64).unwrap_or(0);
                    println!("- {} of {total} localized functions changed", total.saturating_sub(kept));
                }
                for (label,key) in [("changed","functionsChanged"),("added","functionsAdded"),("removed","functionsRemoved")] {
                    if !arr(c,key).is_empty() { println!("- {label}: {}",join(arr(c,key),20)); }
                }
            }
            if let Some(h) = c.get("hunks").and_then(Value::as_str) {
                println!("\n```diff\n{h}\n```");
            }
            println!();
        }
    }
}

pub fn graph_text(v: &Value) {
    let nodes = arr(v, "nodes");
    let edges = arr(v, "edges");
    println!(
        "{} nodes, {} edges  (bundle {}, direction {})",
        nodes.len(),
        edges.len(),
        s(v, "bundle"),
        s(v, "direction")
    );
    if v["truncated"].as_bool().unwrap_or(false) {
        println!("  TRUNCATED at maxNodes — raise --max-nodes or lower --depth");
    }
    let missing = arr(v, "missingRoots");
    if !missing.is_empty() {
        println!("  roots not in this bundle: {}", join(missing, 20));
    }

    println!(
        "\n{:<6} {:<7} {:>5} {:>5}  MODULE",
        "DEPTH", "STATE", "DEPS", "USED"
    );
    for node in nodes {
        let state = if !node["present"].as_bool().unwrap_or(true) {
            "external"
        } else if !node["inFilter"].as_bool().unwrap_or(true) {
            "filtered"
        } else {
            "ok"
        };
        println!(
            "{:<6} {:<7} {:>5} {:>5}  {}",
            n(node, "depth"),
            state,
            n(node, "depCount"),
            n(node, "dependentCount"),
            s(node, "name")
        );
    }

    let cycles = arr(v, "cycles");
    if !cycles.is_empty() {
        println!("\n{} cycle(s):", cycles.len());
        for c in cycles {
            let path: Vec<&str> = c
                .as_array()
                .map_or(vec![], |a| a.iter().filter_map(Value::as_str).collect());
            println!("  {}", path.join(" -> "));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn byte_formatting_scales() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(2048), "2.0 KiB");
        assert_eq!(bytes(1024 * 1024 * 3), "3.0 MiB");
    }

    #[test]
    fn join_reports_how_many_it_hid() {
        let values: Vec<Value> = (0..30).map(|i| json!(format!("m{i}"))).collect();
        let out = join(&values, 5);
        assert!(out.contains("+25 more"), "{out}");
        assert_eq!(join(&[], 5), "(none)");
    }

    #[test]
    fn accessors_tolerate_missing_keys() {
        let v = json!({});
        assert_eq!(s(&v, "nope"), "");
        assert_eq!(n(&v, "nope"), 0);
        assert!(arr(&v, "nope").is_empty());
    }

    #[test]
    fn ndjson_emits_a_header_then_one_line_per_change() {
        // The header must not carry the changes as well, or a consumer reading
        // line-by-line would process every change twice.
        let v = json!({
            "summary": {"added": 1},
            "changes": [{"name": "A"}, {"name": "B"}],
        });
        let mut head = v.clone();
        head["changes"] = Value::Array(vec![]);
        assert_eq!(head["changes"].as_array().unwrap().len(), 0);
        assert_eq!(arr(&v, "changes").len(), 2);
    }
}
