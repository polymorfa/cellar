//! `cellar` — an archive of Meta source bundles you can look things up in.
//!
//! Every command that produces data can produce it as JSON (`--json`), because the
//! primary caller is a coding agent. The human-readable rendering is a view of the
//! same values, never a separate query.

mod mcp;
mod ops;
mod render;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use cellar_core::diff::DiffOptions;
use cellar_core::graph::Direction;
use cellar_core::model::Platform;
use cellar_core::store::Store;
use clap::{Args, Parser, Subcommand, ValueEnum};

use ops::Ctx;

/// Where the full documentation lives.
///
/// Every command's help ends with a link into this site. `--help` is where someone
/// is already stuck, so it is the one place a pointer to the docs earns its screen
/// space.
const DOCS: &str = "https://cellar.mintlify.site";

/// Help footer for a subcommand, deep-linked to its own page.
///
/// A macro rather than a function because clap's derive needs a `&'static str` it
/// can bake in at compile time, and `concat!` only takes literals.
macro_rules! docs_for {
    ($page:literal) => {
        concat!("Documentation: https://cellar.mintlify.site/", $page)
    };
}

#[derive(Parser)]
#[command(
    name = "cellar",
    version,
    about = "Download, browse, diff and graph Meta source bundles (WhatsApp Web and friends).",
    long_about = "cellar keeps an archive of Meta first-party source bundles, one directory per \
                  client revision, with every module written to its own file so that ordinary \
                  file tools work on it directly.\n\n\
                  Start with `cellar bundle add --rev latest`, then `cellar module search`, \
                  `cellar diff` and `cellar graph`. `cellar mcp` serves the same operations to \
                  an agent over MCP.",
    after_help = "Documentation: https://cellar.mintlify.site"
)]
struct Cli {
    /// Archive root. Defaults to $CELLAR_HOME, then ~/.cellar.
    #[arg(long, global = true, env = "CELLAR_HOME")]
    root: Option<PathBuf>,

    /// Suppress progress output on stderr.
    #[arg(long, short, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage stored source bundles.
    #[command(subcommand, after_help = docs_for!("bundles"))]
    Bundle(BundleCmd),
    /// Manage named module filters.
    #[command(subcommand, after_help = docs_for!("filters"))]
    Filter(FilterCmd),
    /// Look up modules and their source.
    #[command(subcommand, after_help = docs_for!("search"))]
    Module(ModuleCmd),
    /// Search module sources by regex (shorthand for `module search --source`).
    #[command(after_help = docs_for!("search"))]
    Grep(GrepArgs),
    /// Compare two bundles.
    #[command(after_help = docs_for!("diff"))]
    Diff(DiffArgs),
    /// Show the dependency or dependent graph of one or more modules.
    #[command(after_help = docs_for!("graph"))]
    Graph(GraphArgs),
    /// Serve these operations to an agent over MCP on stdio.
    #[command(after_help = docs_for!("agents"))]
    Mcp,
    /// Print where the archive lives.
    Paths,
}

#[derive(Subcommand)]
enum BundleCmd {
    /// Download a revision's archive and index it.
    Add(BundleAddArgs),
    /// Index an already-downloaded archive or extracted directory.
    Import(BundleImportArgs),
    /// List stored bundles.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one bundle's manifest, including extraction diagnostics.
    Info {
        /// Bundle spec: `whatsapp-1030882912`, a bare revision, `latest`, or
        /// `<platform>-latest`.
        bundle: String,
        #[arg(long)]
        json: bool,
    },
    /// Re-index a bundle from the chunk files kept by `--keep-chunks`.
    Reindex {
        bundle: String,
        #[arg(long)]
        no_pretty: bool,
        /// Keep the chunk files afterwards, so it can be re-indexed again.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        keep_chunks: bool,
    },
    /// Delete a bundle and its module sources.
    Rm {
        bundle: String,
        /// Required, because this deletes a directory that took minutes to build.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args)]
struct BundleAddArgs {
    #[arg(long, default_value = "whatsapp")]
    platform: Platform,
    /// Client revision, or `latest` to discover the current one.
    #[arg(long, default_value = "latest")]
    rev: String,
    /// Store module sources exactly as served: one very long line each.
    #[arg(long)]
    no_pretty: bool,
    /// Re-index a bundle already in the archive.
    #[arg(long)]
    force: bool,
    /// Keep the extracted chunk files, so the bundle can be re-indexed offline.
    #[arg(long)]
    keep_chunks: bool,
}

#[derive(Args)]
struct BundleImportArgs {
    #[arg(long, default_value = "whatsapp")]
    platform: Platform,
    /// Client revision this archive belongs to. Required: it is the bundle's
    /// identity and cannot be recovered from the files.
    #[arg(long)]
    rev: u64,
    /// A `.zip` archive, or a directory of already-extracted chunk files.
    #[arg(long)]
    from: PathBuf,
    #[arg(long)]
    no_pretty: bool,
    #[arg(long)]
    force: bool,
    #[arg(long)]
    keep_chunks: bool,
}

#[derive(Subcommand)]
enum FilterCmd {
    /// List available filters.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Print one filter's full definition.
    Show { name: String },
    /// Create or update a filter from a JSON document.
    Set {
        name: String,
        /// JSON file, or `-` for stdin. Fields left out keep their current value.
        #[arg(long, default_value = "-")]
        from: PathBuf,
    },
    /// Copy a filter (usually a built-in) under a new, editable name.
    Fork { source: String, dest: String },
    /// Delete a user filter.
    Rm { name: String },
    /// Report what a filter keeps and drops for a bundle.
    Test {
        name: String,
        #[arg(long, default_value = "latest")]
        bundle: String,
        /// How many example modules to show on each side.
        #[arg(long, default_value_t = 10)]
        sample: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ModuleCmd {
    /// Print one module's metadata and source.
    Show {
        name: String,
        #[arg(long, default_value = "latest")]
        bundle: String,
        /// First line to print, 1-based.
        #[arg(long, default_value_t = 1)]
        start: u32,
        /// Maximum lines to print.
        #[arg(long)]
        lines: Option<u32>,
        /// Print only the file path, for piping into other tools.
        #[arg(long)]
        path_only: bool,
        #[arg(long)]
        json: bool,
    },
    /// Find modules by name, source content or exported symbol.
    Search(SearchArgs),
}

#[derive(Args)]
struct SearchArgs {
    /// Regex over module names.
    #[arg(long)]
    name: Option<String>,
    /// Regex over module source.
    #[arg(long)]
    source: Option<String>,
    /// Regex over exported symbol names.
    #[arg(long)]
    exports: Option<String>,
    #[arg(long, default_value = "latest")]
    bundle: String,
    /// Restrict to what this filter keeps. Omit to search everything.
    #[arg(long)]
    filter: Option<String>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    /// Lines of context around each source match.
    #[arg(long, short = 'C', default_value_t = 0)]
    context: usize,
    /// Maximum matching lines reported per module.
    #[arg(long, default_value_t = 5)]
    max_matches: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct GrepArgs {
    /// Regex to search module sources for.
    pattern: String,
    #[arg(long, default_value = "latest")]
    bundle: String,
    /// Narrow by module name first, which avoids opening unrelated files.
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    filter: Option<String>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    #[arg(long, short = 'C', default_value_t = 0)]
    context: usize,
    #[arg(long, default_value_t = 5)]
    max_matches: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct DiffArgs {
    /// Older bundle. Defaults to the second-newest stored for the platform.
    old: Option<String>,
    /// Newer bundle. Defaults to the newest stored for the platform.
    new: Option<String>,
    #[arg(long, default_value = "whatsapp")]
    platform: Platform,
    #[arg(long, default_value = "default")]
    filter: String,
    #[arg(long, value_enum, default_value_t = DiffFormat::Text)]
    format: DiffFormat,
    /// Omit unified-diff text, leaving only which modules changed and how.
    #[arg(long)]
    no_hunks: bool,
    /// Include modules whose only changes are transpiler churn.
    #[arg(long)]
    include_noise: bool,
    /// Tell a rename from a real change, and name the functions that changed.
    ///
    /// Compares each changed module with its local names erased. A minifier
    /// names by position, so inserting one binding renames every local after it
    /// — the dominant form of noise in a bundle, and one no line-level rule can
    /// recognise, because renamed code looks exactly like code.
    #[arg(long)]
    semantic: bool,
    /// Context lines in each hunk.
    #[arg(long, short = 'C', default_value_t = 3)]
    context: usize,
    /// Truncate the change list. The summary still reports true totals.
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum DiffFormat {
    /// Human-readable summary.
    Text,
    /// One JSON document.
    Json,
    /// One JSON change per line, for streaming.
    Ndjson,
    /// Markdown report.
    Md,
}

#[derive(Args)]
struct GraphArgs {
    /// Root module names.
    modules: Vec<String>,
    /// Add every module whose name matches this regex as a root — the "graph for a
    /// group of modules" case.
    #[arg(long = "match")]
    match_pattern: Option<String>,
    #[arg(long, default_value = "latest")]
    bundle: String,
    #[arg(long, value_enum, default_value_t = GraphDirection::Deps)]
    direction: GraphDirection,
    /// Hops from the roots. 0 means unbounded.
    #[arg(long, default_value_t = 1)]
    depth: usize,
    #[arg(long, default_value_t = 2000)]
    max_nodes: usize,
    /// Drop edges to modules this bundle does not define.
    #[arg(long)]
    no_external: bool,
    /// Report dependency cycles in the collected subgraph.
    #[arg(long)]
    cycles: bool,
    /// Mark nodes this filter would exclude.
    #[arg(long)]
    filter: Option<String>,
    #[arg(long, value_enum, default_value_t = GraphFormat::Text)]
    format: GraphFormat,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum GraphDirection {
    /// What the roots depend on.
    Deps,
    /// What depends on the roots.
    Dependents,
    /// Both.
    Both,
}

impl From<GraphDirection> for Direction {
    fn from(d: GraphDirection) -> Self {
        match d {
            GraphDirection::Deps => Direction::Deps,
            GraphDirection::Dependents => Direction::Dependents,
            GraphDirection::Both => Direction::Both,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum GraphFormat {
    Text,
    Json,
    Dot,
    Mermaid,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("cellar: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.root.as_deref().map(ops::expand_home);
    let store = Store::open(root)?;
    // The MCP server speaks JSON-RPC on stdout, so progress must never go there.
    let quiet = cli.quiet || matches!(cli.command, Command::Mcp);
    let ctx = Ctx::new(store, quiet);

    match cli.command {
        Command::Paths => {
            println!("root:    {}", ctx.store.root().display());
            println!("bundles: {}", ctx.store.bundles_dir().display());
            println!("filters: {}", ctx.store.filters_dir().display());
            println!("docs:    {DOCS}");
            Ok(())
        }
        Command::Mcp => mcp::serve(&ctx),
        Command::Bundle(cmd) => bundle(&ctx, cmd),
        Command::Filter(cmd) => filter(&ctx, cmd),
        Command::Module(cmd) => module(&ctx, cmd),
        Command::Grep(args) => {
            let value = ctx.module_search(
                &args.bundle,
                args.name.as_deref(),
                Some(&args.pattern),
                None,
                args.filter.as_deref(),
                args.limit,
                args.context,
                args.max_matches,
            )?;
            render::emit(&value, args.json, render::search)
        }
        Command::Diff(args) => diff(&ctx, args),
        Command::Graph(args) => graph(&ctx, args),
    }
}

fn bundle(ctx: &Ctx, cmd: BundleCmd) -> Result<()> {
    match cmd {
        BundleCmd::Add(a) => {
            let rev = parse_rev(&a.rev)?;
            let value =
                ctx.bundle_add(a.platform, rev, None, !a.no_pretty, a.force, a.keep_chunks)?;
            render::emit(&value, false, render::bundle_added)
        }
        BundleCmd::Import(a) => {
            let from = ops::expand_home(&a.from);
            if !from.exists() {
                bail!("{} does not exist", from.display());
            }
            let value = ctx.bundle_add(
                a.platform,
                Some(a.rev),
                Some(&from),
                !a.no_pretty,
                a.force,
                a.keep_chunks,
            )?;
            render::emit(&value, false, render::bundle_added)
        }
        BundleCmd::Reindex {
            bundle,
            no_pretty,
            keep_chunks,
        } => {
            let value = ctx.bundle_reindex(&bundle, !no_pretty, keep_chunks)?;
            render::emit(&value, false, render::bundle_added)
        }
        BundleCmd::List { json } => render::emit(&ctx.bundle_list()?, json, render::bundle_list),
        BundleCmd::Info { bundle, json } => {
            render::emit(&ctx.bundle_info(&bundle)?, json, render::bundle_info)
        }
        BundleCmd::Rm { bundle, yes } => {
            if !yes {
                bail!("refusing to delete {bundle} without --yes");
            }
            let value = ctx.bundle_remove(&bundle)?;
            println!("removed {}", value["removed"].as_str().unwrap_or(&bundle));
            Ok(())
        }
    }
}

/// `latest` or a number.
fn parse_rev(spec: &str) -> Result<Option<u64>> {
    if spec.eq_ignore_ascii_case("latest") {
        return Ok(None);
    }
    spec.parse()
        .map(Some)
        .with_context(|| format!("--rev {spec:?} is neither a revision number nor `latest`"))
}

fn filter(ctx: &Ctx, cmd: FilterCmd) -> Result<()> {
    match cmd {
        FilterCmd::List { json } => render::emit(&ctx.filter_list()?, json, render::filter_list),
        FilterCmd::Show { name } => {
            println!("{}", serde_json::to_string_pretty(&ctx.filter_get(&name)?)?);
            Ok(())
        }
        FilterCmd::Set { name, from } => {
            let text = ops::read_json_arg(&ops::expand_home(&from))?;
            let patch: ops::FilterPatch = serde_json::from_str(&text)
                .context("the filter document is not valid JSON for a filter")?;
            // Patch over the existing filter when there is one, so a partial
            // document is an edit rather than a silent reset of every other field.
            let base = ctx
                .store
                .get_filter(&name)
                .unwrap_or_else(|_| ops::blank_filter(&name));
            let value = ctx.filter_put(patch.apply(base), Some(&name))?;
            println!(
                "saved {} to {}",
                value["saved"].as_str().unwrap_or(&name),
                value["path"].as_str().unwrap_or_default()
            );
            Ok(())
        }
        FilterCmd::Fork { source, dest } => {
            let value = ctx.filter_fork(&source, &dest)?;
            println!(
                "forked {source} -> {} ({})",
                dest,
                value["path"].as_str().unwrap_or_default()
            );
            Ok(())
        }
        FilterCmd::Rm { name } => {
            ctx.filter_delete(&name)?;
            println!("deleted {name}");
            Ok(())
        }
        FilterCmd::Test {
            name,
            bundle,
            sample,
            json,
        } => render::emit(
            &ctx.filter_test(&name, &bundle, sample)?,
            json,
            render::filter_test,
        ),
    }
}

fn module(ctx: &Ctx, cmd: ModuleCmd) -> Result<()> {
    match cmd {
        ModuleCmd::Show {
            name,
            bundle,
            start,
            lines,
            path_only,
            json,
        } => {
            let value = ctx.module_get(&bundle, &name, start, lines, !path_only)?;
            if path_only {
                println!("{}", value["path"].as_str().unwrap_or_default());
                return Ok(());
            }
            render::emit(&value, json, render::module_show)
        }
        ModuleCmd::Search(a) => {
            let value = ctx.module_search(
                &a.bundle,
                a.name.as_deref(),
                a.source.as_deref(),
                a.exports.as_deref(),
                a.filter.as_deref(),
                a.limit,
                a.context,
                a.max_matches,
            )?;
            render::emit(&value, a.json, render::search)
        }
    }
}

fn diff(ctx: &Ctx, args: DiffArgs) -> Result<()> {
    let (old, new) = match (args.old, args.new) {
        (Some(o), Some(n)) => (o, n),
        (Some(o), None) => {
            // One operand means "from this one to the newest".
            let newest = ctx.store.latest(args.platform)?.to_string();
            (o, newest)
        }
        (None, _) => ctx.default_diff_pair(args.platform)?,
    };

    let opts = DiffOptions {
        include_hunks: !args.no_hunks,
        context_lines: args.context,
        include_noise_only: args.include_noise,
        semantic: args.semantic,
        ..DiffOptions::default()
    };
    let value = ctx.diff(&old, &new, Some(&args.filter), &opts, args.limit)?;

    match args.format {
        DiffFormat::Json => println!("{}", serde_json::to_string_pretty(&value)?),
        DiffFormat::Ndjson => render::diff_ndjson(&value)?,
        DiffFormat::Md => render::diff_markdown(&value),
        DiffFormat::Text => render::diff_text(&value),
    }
    Ok(())
}

fn graph(ctx: &Ctx, args: GraphArgs) -> Result<()> {
    let value = ctx.graph(
        &args.bundle,
        &args.modules,
        args.match_pattern.as_deref(),
        args.direction.into(),
        (args.depth > 0).then_some(args.depth),
        args.max_nodes,
        !args.no_external,
        args.cycles,
        args.filter.as_deref(),
    )?;

    match args.format {
        GraphFormat::Json => println!("{}", serde_json::to_string_pretty(&value)?),
        GraphFormat::Dot | GraphFormat::Mermaid => {
            let g: cellar_core::graph::DepGraph = serde_json::from_value(value)?;
            let text = if args.format == GraphFormat::Dot {
                cellar_core::graph::to_dot(&g)
            } else {
                cellar_core::graph::to_mermaid(&g)
            };
            print!("{text}");
        }
        GraphFormat::Text => render::graph_text(&value),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn rev_accepts_latest_and_numbers_only() {
        assert_eq!(parse_rev("latest").unwrap(), None);
        assert_eq!(parse_rev("LATEST").unwrap(), None);
        assert_eq!(parse_rev("1030882912").unwrap(), Some(1030882912));
        assert!(parse_rev("v3").is_err());
    }

    #[test]
    fn removing_a_bundle_requires_confirmation() {
        let cli = Cli::try_parse_from(["cellar", "bundle", "rm", "whatsapp-1"]).unwrap();
        let Command::Bundle(BundleCmd::Rm { yes, .. }) = cli.command else {
            panic!("expected bundle rm");
        };
        assert!(!yes, "the guard must default to off");
    }

    #[test]
    fn diff_operands_are_optional() {
        let cli = Cli::try_parse_from(["cellar", "diff"]).unwrap();
        let Command::Diff(a) = cli.command else {
            panic!("expected diff");
        };
        assert!(a.old.is_none() && a.new.is_none());
        assert_eq!(a.filter, "default");
    }

    #[test]
    fn graph_accepts_names_and_a_pattern_together() {
        let cli = Cli::try_parse_from([
            "cellar",
            "graph",
            "WAWebFoo",
            "--match",
            "^WAWebSendMsg",
            "--depth",
            "2",
        ])
        .unwrap();
        let Command::Graph(a) = cli.command else {
            panic!("expected graph");
        };
        assert_eq!(a.modules, ["WAWebFoo"]);
        assert_eq!(a.match_pattern.as_deref(), Some("^WAWebSendMsg"));
        assert_eq!(a.depth, 2);
    }
}
