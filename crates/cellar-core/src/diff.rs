//! Diffing two indexed bundles through a filter, into a machine-readable result.
//!
//! Change detection itself is exact and cheap: [`ModuleEntry::raw_sha256`] is the
//! hash of the bytes as served, so "did this module change" is a string compare and
//! never depends on how the source was re-printed. The expensive part — computing
//! hunks and a similarity ratio — runs only for modules that actually changed and
//! survived the filter.
//!
//! Two things keep the output honest rather than merely small:
//!
//! - A change whose every altered line is transpiler/bundler churn is reported with
//!   `noiseOnly: true` rather than dropped. The caller can ignore those; a caller
//!   who suspects the noise rules are over-broad can still see them.
//! - A module too large to diff within [`DiffOptions::max_diff_bytes`] is reported
//!   with `hunksOmitted` naming the reason, so a missing diff is never mistaken for
//!   an empty one.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

use crate::filter::FilterSet;
use crate::model::{BundleId, ModuleEntry, ModuleIndex, SCHEMA_VERSION};
use crate::shape;

/// Supplies module source text on demand. Implemented by
/// [`BundleHandle`](crate::store::BundleHandle); a test can supply its own.
pub trait SourceLoader {
    fn load(&self, entry: &ModuleEntry) -> Result<String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone)]
pub struct DiffOptions {
    /// Emit unified-diff text per modified module.
    pub include_hunks: bool,
    /// Context lines around each hunk.
    pub context_lines: usize,
    /// Skip hunk/similarity computation for modules larger than this on either
    /// side. A handful of bundle modules are megabytes of generated tables, and
    /// diffing them costs more than the answer is worth.
    pub max_diff_bytes: usize,
    /// Truncate emitted hunk text to this many bytes.
    pub max_hunk_bytes: usize,
    /// Report modules whose only changed lines are noise. When false they are
    /// counted in the summary but left out of `changes`.
    pub include_noise_only: bool,
    /// Classify each changed module by comparing the two programs with their
    /// local names erased, and name the functions that really changed.
    ///
    /// Off by default because it parses both sides of every changed module,
    /// which the sha256 comparison does not. It costs roughly a second per
    /// thousand modules and is the difference between "7,400 modules differ"
    /// and a list you can read.
    pub semantic: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            include_hunks: true,
            context_lines: 3,
            max_diff_bytes: 1 << 20,
            max_hunk_bytes: 64 * 1024,
            include_noise_only: false,
            semantic: false,
        }
    }
}

/// One module's change between two bundles.
// No `Eq`: `similarity` is a float. Comparisons of these are for tests and for
// dedup, where `PartialEq` is what is actually wanted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleChange {
    pub name: String,
    pub kind: ChangeKind,
    /// Path of the source in the old bundle, relative to that bundle's directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_sha256: Option<String>,
    /// Dependencies gained, excluding those matching the filter's `noiseDeps`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps_added: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps_removed: Vec<String>,
    /// True when the only dependency changes were noise ones, which is worth
    /// distinguishing from "no dependency change at all".
    #[serde(default, skip_serializing_if = "is_false")]
    pub deps_noise_only: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports_added: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports_removed: Vec<String>,
    /// Fraction of lines unchanged, 0.0–1.0. Absent when the diff was skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
    /// Every changed line matched the filter's `noiseCode` rules.
    #[serde(default, skip_serializing_if = "is_false")]
    pub noise_only: bool,
    /// What the program did, as opposed to what the text did.
    ///
    /// `noiseOnly` above is a line-level guess: it can recognise a changed line
    /// that *looks* like transpiler output, and nothing more. It cannot see a
    /// rename cascade, which is the dominant form of noise in a minified bundle
    /// — inserting one binding renames every local after it, and each of those
    /// lines looks like real code because it is real code that means exactly
    /// what it meant before. This field is decided by comparing the two modules
    /// with their local names erased, so `renamedOnly` is a proof rather than a
    /// pattern match. Absent when the semantic pass was not requested or the
    /// module did not parse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<shape::Verdict>,
    /// The functions that actually changed, by the name they are exported
    /// under. A module reported as changed with this list empty changed only
    /// outside its functions — a dependency, a top-level constant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions_changed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions_added: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions_removed: Vec<String>,
    /// Functions whose program is unchanged however they were renamed or moved,
    /// and the total. `40/46 kept` is the sentence a reviewer needs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub functions_kept: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub functions_total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<u32>,
    /// Unified diff text, when requested and computed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hunks: Option<String>,
    /// Why hunks are absent, when they are. Never left to inference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hunks_omitted: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    /// Modules in each bundle before filtering.
    pub old_total: u64,
    pub new_total: u64,
    /// Modules considered after filtering.
    pub old_kept: u64,
    pub new_kept: u64,
    pub added: u64,
    pub removed: u64,
    pub modified: u64,
    pub unchanged: u64,
    /// Modified modules whose changes were all noise. Counted even when they are
    /// excluded from `changes`, so the number is never silently zero.
    pub noise_only: u64,
    /// Modified modules whose source diff was skipped for size.
    pub diffs_skipped: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleDiff {
    pub schema_version: String,
    pub old: BundleId,
    pub new: BundleId,
    pub filter: String,
    pub summary: DiffSummary,
    /// Sorted by module name, so two runs produce identical bytes and the output
    /// can be diffed against a previous run.
    pub changes: Vec<ModuleChange>,
}

/// Compute the diff between two indexed bundles.
pub fn diff_bundles(
    old: &ModuleIndex,
    old_src: &dyn SourceLoader,
    new: &ModuleIndex,
    new_src: &dyn SourceLoader,
    filter: &FilterSet,
    opts: &DiffOptions,
) -> Result<BundleDiff> {
    let old_keep = filter.apply(old);
    let new_keep = filter.apply(new);

    let mut summary = DiffSummary {
        old_total: old.len() as u64,
        new_total: new.len() as u64,
        old_kept: old_keep.kept_count() as u64,
        new_kept: new_keep.kept_count() as u64,
        ..Default::default()
    };

    let names: BTreeSet<&String> = old_keep.kept.union(&new_keep.kept).collect();
    let mut changes = Vec::new();

    for name in names {
        let o = old_keep.keeps(name).then(|| old.get(name)).flatten();
        let n = new_keep.keeps(name).then(|| new.get(name)).flatten();

        match (o, n) {
            (None, Some(n)) => {
                summary.added += 1;
                changes.push(ModuleChange {
                    name: name.clone(),
                    kind: ChangeKind::Added,
                    old_file: None,
                    new_file: Some(n.file.clone()),
                    old_sha256: None,
                    new_sha256: Some(n.raw_sha256.clone()),
                    deps_added: retain_signal(&n.deps, &[], filter),
                    deps_removed: vec![],
                    deps_noise_only: false,
                    verdict: None,
                    functions_changed: Vec::new(),
                    functions_added: Vec::new(),
                    functions_removed: Vec::new(),
                    functions_kept: None,
                    functions_total: None,
                    exports_added: n.exports.clone(),
                    exports_removed: vec![],
                    similarity: None,
                    noise_only: false,
                    lines_added: None,
                    lines_removed: None,
                    hunks: None,
                    hunks_omitted: None,
                });
            }
            (Some(o), None) => {
                summary.removed += 1;
                changes.push(ModuleChange {
                    name: name.clone(),
                    kind: ChangeKind::Removed,
                    old_file: Some(o.file.clone()),
                    new_file: None,
                    old_sha256: Some(o.raw_sha256.clone()),
                    new_sha256: None,
                    deps_added: vec![],
                    deps_removed: retain_signal(&o.deps, &[], filter),
                    deps_noise_only: false,
                    verdict: None,
                    functions_changed: Vec::new(),
                    functions_added: Vec::new(),
                    functions_removed: Vec::new(),
                    functions_kept: None,
                    functions_total: None,
                    exports_added: vec![],
                    exports_removed: o.exports.clone(),
                    similarity: None,
                    noise_only: false,
                    lines_added: None,
                    lines_removed: None,
                    hunks: None,
                    hunks_omitted: None,
                });
            }
            (Some(o), Some(n)) => {
                // Compare every definition, not just the primary: see
                // `ModuleEntry::fingerprint`.
                if o.fingerprint() == n.fingerprint() {
                    summary.unchanged += 1;
                    continue;
                }
                summary.modified += 1;
                let mut change = modified_change(name, o, n, filter);

                let too_big = o.stored_len as usize > opts.max_diff_bytes
                    || n.stored_len as usize > opts.max_diff_bytes;
                if too_big {
                    summary.diffs_skipped += 1;
                    change.hunks_omitted = Some(format!(
                        "module exceeds maxDiffBytes ({} bytes); old={} new={}",
                        opts.max_diff_bytes, o.stored_len, n.stored_len
                    ));
                } else {
                    let old_text = old_src.load(o)?;
                    let new_text = new_src.load(n)?;
                    apply_text_diff(&mut change, &old_text, &new_text, filter, opts);
                    if opts.semantic {
                        apply_semantic_diff(&mut change, &old_text, &new_text);
                    }
                }

                // A proof outranks a guess. The line rules can only recognise
                // changed lines that *look* like transpiler output; the shape
                // comparison knows the two programs are the same one, whatever
                // the lines say. Where they disagree, take the proof — that is
                // the whole reason for computing it.
                if change.verdict == Some(shape::Verdict::RenamedOnly) {
                    change.noise_only = true;
                }
                if change.noise_only {
                    summary.noise_only += 1;
                    if !opts.include_noise_only {
                        continue;
                    }
                }
                changes.push(change);
            }
            (None, None) => unreachable!("name came from the union of the two kept sets"),
        }
    }

    Ok(BundleDiff {
        schema_version: SCHEMA_VERSION.to_string(),
        old: old.bundle,
        new: new.bundle,
        filter: filter.name().to_string(),
        summary,
        changes,
    })
}

/// Entries of `from` that are absent from `to` and are not noise dependencies.
fn retain_signal(from: &[String], to: &[String], filter: &FilterSet) -> Vec<String> {
    let to: BTreeSet<&str> = to.iter().map(String::as_str).collect();
    let mut out: Vec<String> = from
        .iter()
        .filter(|d| !to.contains(d.as_str()) && !filter.is_noise_dep(d))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

fn diff_lists(from: &[String], to: &[String]) -> Vec<String> {
    let to: BTreeSet<&str> = to.iter().map(String::as_str).collect();
    let mut out: Vec<String> = from
        .iter()
        .filter(|d| !to.contains(d.as_str()))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

fn modified_change(
    name: &str,
    o: &ModuleEntry,
    n: &ModuleEntry,
    filter: &FilterSet,
) -> ModuleChange {
    let deps_added = retain_signal(&n.deps, &o.deps, filter);
    let deps_removed = retain_signal(&o.deps, &n.deps, filter);
    // Whether anything changed at all before noise filtering, so that "no dep
    // change" and "only noise deps changed" stay distinguishable.
    let raw_changed =
        !diff_lists(&n.deps, &o.deps).is_empty() || !diff_lists(&o.deps, &n.deps).is_empty();

    ModuleChange {
        name: name.to_string(),
        kind: ChangeKind::Modified,
        old_file: Some(o.file.clone()),
        new_file: Some(n.file.clone()),
        old_sha256: Some(o.raw_sha256.clone()),
        new_sha256: Some(n.raw_sha256.clone()),
        deps_noise_only: raw_changed && deps_added.is_empty() && deps_removed.is_empty(),
        deps_added,
        deps_removed,
        exports_added: diff_lists(&n.exports, &o.exports),
        exports_removed: diff_lists(&o.exports, &n.exports),
        similarity: None,
        noise_only: false,
        verdict: None,
        functions_changed: Vec::new(),
        functions_added: Vec::new(),
        functions_removed: Vec::new(),
        functions_kept: None,
        functions_total: None,
        lines_added: None,
        lines_removed: None,
        hunks: None,
        hunks_omitted: None,
    }
}

/// Decide what the *program* did, and name the functions it did it in.
///
/// The text diff above answers "which lines are different", which in a minified
/// bundle is mostly a question about the minifier. This answers "is this a
/// different program", and when it is, "which functions" — the two things a
/// reviewer actually wants and neither of which a line diff can supply.
fn apply_semantic_diff(change: &mut ModuleChange, old_text: &str, new_text: &str) {
    let verdict = shape::compare(old_text, new_text);
    change.verdict = Some(verdict);
    // Nothing to localise: either the program is the same, or one side did not
    // parse and any function list would be invented rather than measured.
    if !matches!(verdict, shape::Verdict::Changed) {
        return;
    }

    let d = shape::diff_functions(old_text, new_text);
    change.functions_changed = d.changed.iter().map(|c| c.label.clone()).collect();
    change.functions_added = d.added.iter().map(|f| f.label.clone()).collect();
    change.functions_removed = d.removed.iter().map(|f| f.label.clone()).collect();
    change.functions_kept = Some(d.kept as u32);
    change.functions_total = Some(d.total as u32);
}

fn apply_text_diff(
    change: &mut ModuleChange,
    old_text: &str,
    new_text: &str,
    filter: &FilterSet,
    opts: &DiffOptions,
) {
    let diff = TextDiff::from_lines(old_text, new_text);

    let mut added = 0u32;
    let mut removed = 0u32;
    let mut signal_lines = 0u32;
    for change_op in diff.iter_all_changes() {
        let tag = change_op.tag();
        if tag == ChangeTag::Equal {
            continue;
        }
        match tag {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => unreachable!(),
        }
        if !filter.is_noise_line(change_op.value().trim_end()) {
            signal_lines += 1;
        }
    }

    change.lines_added = Some(added);
    change.lines_removed = Some(removed);
    change.similarity = Some(diff.ratio());
    // A module can differ byte-wise while every *line* is equal (the raw bytes
    // changed but re-printing normalized it away). That is noise by definition.
    change.noise_only = signal_lines == 0;

    if opts.include_hunks && !change.noise_only {
        let mut text = diff
            .unified_diff()
            .context_radius(opts.context_lines)
            .header("old", "new")
            .to_string();
        if text.len() > opts.max_hunk_bytes {
            let mut end = opts.max_hunk_bytes;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("\n… [truncated: exceeded maxHunkBytes]\n");
            change.hunks_omitted = Some("truncated to maxHunkBytes".to_string());
        }
        change.hunks = Some(text);
    }
}

/// Group a diff's changes by a caller-supplied key, for a quick overview of where
/// change is concentrated. Used by the CLI's summary output.
pub fn group_by<'a, F>(diff: &'a BundleDiff, key: F) -> BTreeMap<String, Vec<&'a ModuleChange>>
where
    F: Fn(&'a ModuleChange) -> String,
{
    let mut out: BTreeMap<String, Vec<&ModuleChange>> = BTreeMap::new();
    for c in &diff.changes {
        out.entry(key(c)).or_default().push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Filter, Verdict};
    use crate::model::{Platform, SourceForm};

    struct MapLoader(BTreeMap<String, String>);

    impl SourceLoader for MapLoader {
        fn load(&self, entry: &ModuleEntry) -> Result<String> {
            Ok(self.0.get(&entry.name).cloned().unwrap_or_default())
        }
    }

    fn entry(name: &str, sha: &str, deps: &[&str], len: u64) -> ModuleEntry {
        ModuleEntry {
            name: name.to_string(),
            file: format!("modules/{name}.js"),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            dependents: vec![],
            exports: vec![],
            functions: vec![],
            raw_sha256: sha.to_string(),
            raw_len: len,
            stored_len: len,
            chunk: "c.js".to_string(),
            form: SourceForm::Pretty,
            variants: vec![],
        }
    }

    fn keep_all() -> FilterSet {
        FilterSet::compile(Filter {
            name: "all".into(),
            description: String::new(),
            hard_exclude: vec![],
            include: vec![],
            exclude: vec![],
            default_verdict: Verdict::Keep,
            exclude_dependents_of_excluded: false,
            noise_deps: vec!["^React$".into()],
            noise_code: vec!["babelHelpers".into()],
            builtin: false,
        })
        .unwrap()
    }

    fn idx(rev: u64, entries: Vec<ModuleEntry>) -> ModuleIndex {
        ModuleIndex::new(BundleId::new(Platform::Whatsapp, rev), entries)
    }

    fn loader(pairs: &[(&str, &str)]) -> MapLoader {
        MapLoader(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn classifies_added_removed_modified_and_unchanged() {
        let old = idx(
            1,
            vec![
                entry("Same", "aaa", &[], 10),
                entry("Gone", "bbb", &[], 10),
                entry("Changed", "ccc", &[], 10),
            ],
        );
        let new = idx(
            2,
            vec![
                entry("Same", "aaa", &[], 10),
                entry("New", "ddd", &[], 10),
                entry("Changed", "eee", &[], 10),
            ],
        );
        let ol = loader(&[("Changed", "let a = 1;\n")]);
        let nl = loader(&[("Changed", "let a = 2;\n")]);

        let d = diff_bundles(&old, &ol, &new, &nl, &keep_all(), &DiffOptions::default()).unwrap();
        assert_eq!(d.summary.added, 1);
        assert_eq!(d.summary.removed, 1);
        assert_eq!(d.summary.modified, 1);
        assert_eq!(d.summary.unchanged, 1);

        let names: Vec<&str> = d.changes.iter().map(|c| c.name.as_str()).collect();
        // Sorted by name, and `Same` is absent because it did not change.
        assert_eq!(names, ["Changed", "Gone", "New"]);
    }

    #[test]
    fn noise_only_changes_are_counted_but_withheld_by_default() {
        let old = idx(1, vec![entry("M", "aaa", &[], 40)]);
        let new = idx(2, vec![entry("M", "bbb", &[], 40)]);
        let ol = loader(&[("M", "keep();\nbabelHelpers.extends(a);\n")]);
        let nl = loader(&[("M", "keep();\nbabelHelpers.extends(b);\n")]);

        let opts = DiffOptions::default();
        let d = diff_bundles(&old, &ol, &new, &nl, &keep_all(), &opts).unwrap();
        assert_eq!(d.summary.modified, 1);
        assert_eq!(d.summary.noise_only, 1);
        assert!(d.changes.is_empty(), "noise is withheld, not lost");

        let opts = DiffOptions {
            include_noise_only: true,
            ..DiffOptions::default()
        };
        let d = diff_bundles(&old, &ol, &new, &nl, &keep_all(), &opts).unwrap();
        assert_eq!(d.changes.len(), 1);
        assert!(d.changes[0].noise_only);
    }

    #[test]
    fn real_change_alongside_noise_is_not_noise_only() {
        let old = idx(1, vec![entry("M", "aaa", &[], 60)]);
        let new = idx(2, vec![entry("M", "bbb", &[], 60)]);
        let ol = loader(&[("M", "babelHelpers.extends(a);\nsendStanza(1);\n")]);
        let nl = loader(&[("M", "babelHelpers.extends(b);\nsendStanza(2);\n")]);
        let d = diff_bundles(&old, &ol, &new, &nl, &keep_all(), &DiffOptions::default()).unwrap();
        assert_eq!(d.changes.len(), 1);
        assert!(!d.changes[0].noise_only);
        assert!(d.changes[0].hunks.as_ref().unwrap().contains("sendStanza"));
    }

    /// The case the line rules cannot reach. Every changed line here is real
    /// code that reads as real code; only erasing the names shows the two
    /// programs are the same one.
    #[test]
    fn a_rename_cascade_is_noise_the_line_rules_cannot_see() {
        let old = idx(1, vec![entry("M", "aaa", &[], 90)]);
        let new = idx(2, vec![entry("M", "bbb", &[], 90)]);
        let ol = loader(&[("M", "var h = 1, y = 2;\nfunction f() { return h + y }\n")]);
        let nl = loader(&[("M", "var y = 1, C = 2;\nfunction g() { return y + C }\n")]);

        // Without the semantic pass this is three changed lines of ordinary
        // code, and the line rules are right to call it a change on what they
        // can see.
        let d = diff_bundles(&old, &ol, &new, &nl, &keep_all(), &DiffOptions::default()).unwrap();
        assert_eq!(d.changes.len(), 1);
        assert!(!d.changes[0].noise_only);

        let opts = DiffOptions {
            semantic: true,
            include_noise_only: true,
            ..Default::default()
        };
        let d = diff_bundles(&old, &ol, &new, &nl, &keep_all(), &opts).unwrap();
        assert_eq!(d.changes[0].verdict, Some(shape::Verdict::RenamedOnly));
        assert!(d.changes[0].noise_only, "a proof outranks the line rules");
        assert_eq!(d.summary.noise_only, 1);
    }

    /// And it must not swallow a real edit hiding inside that same cascade.
    #[test]
    fn a_real_change_inside_a_cascade_is_named() {
        let old = idx(1, vec![entry("M", "aaa", &[], 90)]);
        let new = idx(2, vec![entry("M", "bbb", &[], 90)]);
        // A real module shape: the factory's last parameter is the exports
        // object, and that is what makes `check` a name rather than a letter.
        let ol = loader(&[(
            "M",
            "__d(\"M\", [], (function(t, n, r, o, a, i, l) {\n\
             \x20 var h = 1;\n  function f(t) { return t > 4 }\n  l.check = f;\n}), 98)\n",
        )]);
        let nl = loader(&[(
            "M",
            "__d(\"M\", [], (function(e, s, d, f, g, h, j) {\n\
             \x20 var y = 1;\n  function q(n) { return n > 5 }\n  j.check = q;\n}), 98)\n",
        )]);

        let opts = DiffOptions {
            semantic: true,
            ..Default::default()
        };
        let d = diff_bundles(&old, &ol, &new, &nl, &keep_all(), &opts).unwrap();
        let c = &d.changes[0];
        assert_eq!(c.verdict, Some(shape::Verdict::Changed));
        assert!(!c.noise_only);
        assert_eq!(
            c.functions_changed,
            vec!["check"],
            "named, not just counted"
        );
    }

    #[test]
    fn noise_deps_are_excluded_but_the_fact_of_change_is_kept() {
        let old = idx(1, vec![entry("M", "aaa", &["React"], 10)]);
        let new = idx(2, vec![entry("M", "bbb", &[], 10)]);
        let ol = loader(&[("M", "a\n")]);
        let nl = loader(&[("M", "b\n")]);
        let d = diff_bundles(&old, &ol, &new, &nl, &keep_all(), &DiffOptions::default()).unwrap();
        let c = &d.changes[0];
        assert!(c.deps_removed.is_empty(), "React is a noise dep");
        assert!(c.deps_noise_only, "but the dep list really did change");
    }

    #[test]
    fn oversized_modules_say_why_the_diff_is_missing() {
        let old = idx(1, vec![entry("Big", "aaa", &[], 5_000_000)]);
        let new = idx(2, vec![entry("Big", "bbb", &[], 5_000_000)]);
        let empty = loader(&[]);
        let d = diff_bundles(
            &old,
            &empty,
            &new,
            &empty,
            &keep_all(),
            &DiffOptions::default(),
        )
        .unwrap();
        assert_eq!(d.summary.diffs_skipped, 1);
        let c = &d.changes[0];
        assert!(c.hunks.is_none());
        assert!(c.hunks_omitted.as_ref().unwrap().contains("maxDiffBytes"));
    }

    #[test]
    fn output_is_deterministic() {
        let old = idx(1, vec![entry("B", "a", &[], 5), entry("A", "a", &[], 5)]);
        let new = idx(2, vec![entry("A", "z", &[], 5), entry("B", "z", &[], 5)]);
        let ol = loader(&[("A", "1\n"), ("B", "1\n")]);
        let nl = loader(&[("A", "2\n"), ("B", "2\n")]);
        let run = || {
            serde_json::to_string(
                &diff_bundles(&old, &ol, &new, &nl, &keep_all(), &DiffOptions::default()).unwrap(),
            )
            .unwrap()
        };
        assert_eq!(run(), run());
    }
}
