//! Core model for `cellar`: the on-disk layout of a source-bundle archive, the
//! module index that describes one bundle, the named filter engine, and the diff
//! and dependency-graph queries that run over them.
//!
//! No network and no JS parsing live here — `cellar-fetch` downloads bundles and
//! `cellar-index` turns them into a [`ModuleIndex`]. This crate is what both of
//! those, the CLI and the MCP server agree on.
//!
//! # Design
//!
//! - **Deterministic.** Every emitted list is sorted and every map is a
//!   [`BTreeMap`](std::collections::BTreeMap), so re-indexing the same bundle
//!   produces byte-identical JSON and a diff of two runs is empty.
//! - **Diagnostics, not silence.** Anything the indexer sees but cannot resolve is
//!   counted in [`Diagnostics`] rather than dropped, so "this bundle has no such
//!   module" and "we failed to extract it" never look alike.
//! - **Paths in every answer.** Query results carry the on-disk path of the module
//!   source, because the caller (usually an agent) has its own file-reading and
//!   grep tools and should be able to take over from any result.

pub mod builtin;
pub mod diff;
pub mod filter;
pub mod graph;
pub mod model;
pub mod naming;
pub mod search;
pub mod shape;
pub mod store;

pub use diff::{BundleDiff, ChangeKind, DiffOptions, ModuleChange, SourceLoader, diff_bundles};
pub use filter::{Filter, FilterDecision, FilterSet, Verdict};
pub use graph::{DepGraph, Direction, GraphOptions};
pub use model::{
    BundleId, BundleManifest, Diagnostics, ModuleEntry, ModuleIndex, Platform, SCHEMA_VERSION,
    SourceForm,
};
pub use naming::FileNamer;
pub use search::{Query, SearchResult};
pub use store::{BundleHandle, Store};
