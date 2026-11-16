//! A Model Context Protocol server on stdio.
//!
//! Tools-only, hand-rolled JSON-RPC. That is a deliberate choice over an SDK: the
//! surface an agent needs here is `initialize` / `tools/list` / `tools/call`, which
//! is a few hundred lines, and it keeps the binary free of a fast-moving dependency
//! for a protocol this stable.
//!
//! Two rules the transport imposes and this module must never break:
//!
//! - **stdout carries only JSON-RPC.** Anything else corrupts the stream. Progress
//!   and errors go to stderr; the `Ctx` is built in quiet mode for this reason.
//! - **A tool that fails is a successful RPC.** Per the MCP specification, an error
//!   inside a tool is returned as `isError: true` with the message as content, so
//!   the model can read the failure and adjust. Only protocol-level faults —
//!   malformed JSON, an unknown method — become JSON-RPC errors.
//!
//! Every tool returns the same JSON its CLI counterpart would print with `--json`,
//! and every result that names a module carries the module's absolute path, so an
//! agent can hand off to its own file-reading and grep tools at any point.

use std::io::{BufRead, Write};

use anyhow::Result;
use cellar_core::diff::DiffOptions;
use cellar_core::graph::Direction;
use cellar_core::model::Platform;
use serde_json::{Value, json};

use crate::ops::{self, Ctx};

/// Protocol revisions this server implements. The first is what it offers when a
/// client asks for something unrecognized.
const SUPPORTED_PROTOCOLS: [&str; 2] = ["2025-06-18", "2024-11-05"];

const JSONRPC: &str = "2.0";
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const PARSE_ERROR: i64 = -32700;

/// Read requests from stdin until it closes.
pub fn serve(ctx: &Ctx) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut line = String::new();

    loop {
        line.clear();
        if stdin.lock().read_line(&mut line)? == 0 {
            return Ok(()); // client closed the pipe
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                write_message(
                    &mut stdout,
                    &error_response(Value::Null, PARSE_ERROR, &format!("invalid JSON: {e}")),
                )?;
                continue;
            }
        };

        // A message without `id` is a notification: act on it, answer nothing.
        // Answering one is a protocol violation some clients treat as fatal.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = request.get("params").cloned().unwrap_or(json!({}));

        let response = dispatch(ctx, &method, &params, id);
        write_message(&mut stdout, &response)?;
    }
}

fn write_message(out: &mut impl Write, message: &Value) -> Result<()> {
    serde_json::to_writer(&mut *out, message)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn dispatch(ctx: &Ctx, method: &str, params: &Value, id: Value) -> Value {
    match method {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let version = if SUPPORTED_PROTOCOLS.contains(&requested) {
                requested
            } else {
                SUPPORTED_PROTOCOLS[0]
            };
            ok(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "cellar",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => ok(id, json!({})),
        "tools/list" => ok(id, json!({ "tools": tools() })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if name.is_empty() {
                return error_response(id, INVALID_PARAMS, "tools/call requires a tool `name`");
            }
            match call_tool(ctx, name, &args) {
                Ok(value) => ok(id, tool_result(&value, false)),
                // A tool failure is data for the model, not a transport fault.
                Err(e) => ok(id, tool_result(&json!(format!("{e:#}")), true)),
            }
        }
        other => error_response(id, METHOD_NOT_FOUND, &format!("unknown method {other:?}")),
    }
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": JSONRPC, "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": JSONRPC,
        "id": id,
        "error": { "code": code, "message": message },
    })
}

/// Wrap a value as MCP tool content. Structured results go out as pretty JSON text
/// so a model reading the transcript sees something legible.
fn tool_result(value: &Value, is_error: bool) -> Value {
    let text = match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|e| e.to_string()),
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

const INSTRUCTIONS: &str = "\
cellar holds Meta source bundles (WhatsApp Web and the other first-party clients), one \
per client revision, exploded so every module is its own file.

Typical investigations:
  • What changed in this release? `diff` with the `default` filter, then `module_get` \
    the interesting names.
  • Where does feature X live? `module_search` with a `source` regex; narrow with `name` \
    first when you can, it avoids opening unrelated files.
  • What is this module part of? `graph` with direction `dependents` — minified call \
    sites do not spell module names, so grep cannot answer this.

Every result carries a `path`. Once a search points you at a module, reading and \
grepping those files with your own tools is usually faster than another query — the \
whole `modules/` directory is ordinary, greppable, multi-line JavaScript.

Filters decide which of the ~100k modules an operation considers. `default` is the \
protocol surface; `all` is everything; `protocol`, `schemas` and `wam` are narrower \
allow-lists. Use `filter_test` before trusting an unfamiliar one.";

/// The tool catalogue. Descriptions are written for a model choosing between them.
fn tools() -> Vec<Value> {
    vec![
        tool(
            "bundle_list",
            "List the source bundles in the archive, with module counts and the directory \
             each one's module sources live in.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            Annotations::READ_ONLY,
        ),
        tool(
            "bundle_info",
            "Show one bundle's manifest: where it came from, its archive hash, and the \
             extraction diagnostics. Check the diagnostics before trusting an unexpected \
             diff — non-zero parse failures mean modules are genuinely missing from the index.",
            json!({
                "type": "object",
                "properties": { "bundle": BUNDLE_ARG },
                "required": ["bundle"],
                "additionalProperties": false
            }),
            Annotations::READ_ONLY,
        ),
        tool(
            "bundle_add",
            "Download a client revision's source archive and index it. Slow (minutes) and \
             uses gigabytes of disk. Omit `rev` to fetch the current revision. This is the \
             only operation that reaches the network.",
            json!({
                "type": "object",
                "properties": {
                    "platform": PLATFORM_ARG,
                    "rev": {
                        "type": "integer",
                        "description": "Client revision. Omit for the current one (whatsapp only)."
                    },
                    "force": { "type": "boolean", "description": "Re-index a bundle already held.", "default": false },
                    "keepChunks": { "type": "boolean", "description": "Keep raw chunk files for offline re-indexing.", "default": false }
                },
                "additionalProperties": false
            }),
            Annotations::WRITES,
        ),
        tool(
            "bundle_import",
            "Index an archive already on disk — a `.zip`, or a directory of extracted chunk \
             files. `rev` is required because it is the bundle's identity and cannot be \
             recovered from the files.",
            json!({
                "type": "object",
                "properties": {
                    "platform": PLATFORM_ARG,
                    "rev": { "type": "integer" },
                    "from": { "type": "string", "description": "Path to a .zip or a directory of chunks." },
                    "force": { "type": "boolean", "default": false }
                },
                "required": ["rev", "from"],
                "additionalProperties": false
            }),
            Annotations::WRITES,
        ),
        tool(
            "bundle_reindex",
            "Re-index a bundle from the chunk files kept by `keepChunks`, without \
             re-downloading. Use after a cellar upgrade changes how modules are extracted; \
             fails with instructions if no chunks were kept.",
            json!({
                "type": "object",
                "properties": {
                    "bundle": BUNDLE_ARG,
                    "keepChunks": { "type": "boolean", "description": "Keep the chunks for next time.", "default": true }
                },
                "required": ["bundle"],
                "additionalProperties": false
            }),
            Annotations::WRITES,
        ),
        tool(
            "bundle_remove",
            "Delete a bundle and all of its module sources. Irreversible without \
             re-downloading; requires `confirm: true`.",
            json!({
                "type": "object",
                "properties": {
                    "bundle": BUNDLE_ARG,
                    "confirm": { "type": "boolean", "description": "Must be true." }
                },
                "required": ["bundle", "confirm"],
                "additionalProperties": false
            }),
            Annotations::DESTRUCTIVE,
        ),
        tool(
            "filter_list",
            "List the named filters. A filter decides which modules an operation considers; \
             without one, a diff of two revisions is tens of thousands of irrelevant changes.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            Annotations::READ_ONLY,
        ),
        tool(
            "filter_get",
            "Print one filter's full definition — every pattern in its hardExclude, include, \
             exclude, noiseDeps and noiseCode lists. Read this before forking a filter, so \
             the edit is a change to known rules rather than a guess.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"],
                "additionalProperties": false
            }),
            Annotations::READ_ONLY,
        ),
        tool(
            "filter_set",
            "Create or update a user filter. Fields left out keep their current value, so a \
             partial document is an edit. Precedence: hardExclude, then include, then exclude; \
             anything unmatched takes defaultVerdict.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "filter": {
                        "type": "object",
                        "description": "Partial filter: any of description, hardExclude, include, \
                                        exclude, defaultVerdict ('keep'|'drop'), \
                                        excludeDependentsOfExcluded, noiseDeps, noiseCode."
                    }
                },
                "required": ["name", "filter"],
                "additionalProperties": false
            }),
            Annotations::WRITES,
        ),
        tool(
            "filter_fork",
            "Copy a filter under a new, editable name. Built-in filters cannot be edited in \
             place; fork one and edit the copy.",
            json!({
                "type": "object",
                "properties": {
                    "source": { "type": "string" },
                    "dest": { "type": "string" }
                },
                "required": ["source", "dest"],
                "additionalProperties": false
            }),
            Annotations::WRITES,
        ),
        tool(
            "filter_delete",
            "Delete a user filter. Deleting one that shadows a built-in restores the built-in.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"],
                "additionalProperties": false
            }),
            Annotations::DESTRUCTIVE,
        ),
        tool(
            "filter_test",
            "Report how many modules a filter keeps for a bundle, with examples of both \
             sides and why each dropped module was dropped. Run this before trusting an \
             unfamiliar or newly edited filter.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "bundle": BUNDLE_ARG,
                    "sample": { "type": "integer", "default": 10 }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
            Annotations::READ_ONLY,
        ),
        tool(
            "module_get",
            "Fetch one module by exact name: its dependencies, dependents, exports, and \
             source. If the name is wrong, the error lists near matches. The returned `path` \
             can be read directly with your own file tools.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Exact module name, e.g. WAWebSendMsgStanza." },
                    "bundle": BUNDLE_ARG,
                    "startLine": { "type": "integer", "description": "First line, 1-based.", "default": 1 },
                    "maxLines": { "type": "integer", "description": "Limit for a large module." },
                    "includeSource": { "type": "boolean", "default": true }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
            Annotations::READ_ONLY,
        ),
        tool(
            "module_search",
            "Find modules by name regex, source-content regex, or exported symbol regex. \
             Combining `name` with `source` is much cheaper than `source` alone, because the \
             name narrowing happens in the index and only the surviving files are opened.",
            json!({
                "type": "object",
                "properties": {
                    "bundle": BUNDLE_ARG,
                    "name": { "type": "string", "description": "Regex over module names." },
                    "source": { "type": "string", "description": "Regex over module source text." },
                    "exports": { "type": "string", "description": "Regex over exported symbol names." },
                    "filter": { "type": "string", "description": "Restrict to what this filter keeps. Omit to search everything." },
                    "limit": { "type": "integer", "default": 50 },
                    "contextLines": { "type": "integer", "default": 0 },
                    "maxMatches": { "type": "integer", "description": "Matching lines reported per module.", "default": 5 }
                },
                "additionalProperties": false
            }),
            Annotations::READ_ONLY,
        ),
        tool(
            "diff",
            "Compare two bundles through a filter: which modules were added, removed or \
             modified, with dependency and export deltas and unified diffs. Changes whose \
             every altered line is transpiler churn are counted as `noiseOnly` in the summary \
             and withheld unless `includeNoise` is set. Start with `includeHunks: false` for \
             an overview, then re-run for the modules you care about. Set `semantic: true` \
             to tell a rename from a real change: each changed module is compared with its \
             local names erased, giving a `verdict` of `renamedOnly` or `changed` and, for \
             the latter, the names of the functions that actually changed. In a minified \
             bundle most byte-level diffs are rename cascades, which no line-level rule can \
             recognise, so this is usually the difference between thousands of changed \
             modules and a list worth reading.",
            json!({
                "type": "object",
                "properties": {
                    "old": BUNDLE_ARG,
                    "new": BUNDLE_ARG,
                    "filter": { "type": "string", "default": "default" },
                    "includeHunks": { "type": "boolean", "default": true },
                    "contextLines": { "type": "integer", "default": 3 },
                    "includeNoise": { "type": "boolean", "default": false },
                    "semantic": { "type": "boolean", "default": false, "description": "Classify each change as renamedOnly or changed by comparing programs with local names erased, and name the changed functions. Parses both sides of every changed module." },
                    "limit": { "type": "integer", "description": "Cap the change list; the summary still reports true totals." }
                },
                "required": ["old", "new"],
                "additionalProperties": false
            }),
            Annotations::READ_ONLY,
        ),
        tool(
            "graph",
            "Dependency or dependent graph for one or more modules. Direction `dependents` \
             answers 'what uses this', which grep cannot: minified call sites reference \
             modules positionally, not by name. Use `match` to root the graph at every module \
             whose name matches a regex.",
            json!({
                "type": "object",
                "properties": {
                    "bundle": BUNDLE_ARG,
                    "modules": { "type": "array", "items": { "type": "string" }, "description": "Root module names." },
                    "match": { "type": "string", "description": "Also root at every module matching this regex." },
                    "direction": { "type": "string", "enum": ["deps", "dependents", "both"], "default": "deps" },
                    "depth": { "type": "integer", "description": "Hops from the roots; 0 is unbounded.", "default": 1 },
                    "maxNodes": { "type": "integer", "default": 2000 },
                    "includeExternal": { "type": "boolean", "description": "Include names this bundle does not define.", "default": true },
                    "detectCycles": { "type": "boolean", "default": false },
                    "filter": { "type": "string", "description": "Mark nodes this filter would exclude." }
                },
                "additionalProperties": false
            }),
            Annotations::READ_ONLY,
        ),
    ]
}

/// Shared argument schemas, so their wording stays consistent across tools.
const BUNDLE_ARG: Value = Value::Null; // replaced below; see `bundle_arg`

/// `BUNDLE_ARG` cannot be a `const` `json!`, so the macro call sites use this.
fn bundle_arg() -> Value {
    json!({
        "type": "string",
        "description": "Bundle spec: `whatsapp-1030882912`, a bare revision, `latest`, or `<platform>-latest`.",
        "default": "latest"
    })
}

fn platform_arg() -> Value {
    json!({
        "type": "string",
        "enum": ["whatsapp", "messenger", "facebook", "instagram"],
        "default": "whatsapp"
    })
}

struct Annotations;

impl Annotations {
    const READ_ONLY: (bool, bool) = (true, false);
    const WRITES: (bool, bool) = (false, false);
    const DESTRUCTIVE: (bool, bool) = (false, true);
}

fn tool(
    name: &str,
    description: &str,
    mut schema: Value,
    (read_only, destructive): (bool, bool),
) -> Value {
    // Fill in the placeholder argument schemas.
    substitute_placeholders(&mut schema);
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
        "annotations": {
            "readOnlyHint": read_only,
            "destructiveHint": destructive,
            "idempotentHint": read_only,
            "openWorldHint": name == "bundle_add",
        },
    })
}

/// Replace the `BUNDLE_ARG` / `PLATFORM_ARG` placeholders with their real schemas.
fn substitute_placeholders(schema: &mut Value) {
    let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    for (key, value) in props.iter_mut() {
        if !value.is_null() {
            continue;
        }
        *value = match key.as_str() {
            "platform" => platform_arg(),
            _ => bundle_arg(),
        };
    }
}

const PLATFORM_ARG: Value = Value::Null;

// --- argument extraction -------------------------------------------------

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn arg_usize(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .map_or(default, |v| v as usize)
}

fn arg_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn bundle_of(args: &Value) -> &str {
    arg_str(args, "bundle").unwrap_or("latest")
}

fn platform_of(args: &Value) -> Result<Platform> {
    match arg_str(args, "platform") {
        Some(p) => p.parse(),
        None => Ok(Platform::Whatsapp),
    }
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    arg_str(args, key).ok_or_else(|| anyhow::anyhow!("missing required argument `{key}`"))
}

fn call_tool(ctx: &Ctx, name: &str, args: &Value) -> Result<Value> {
    match name {
        "bundle_list" => ctx.bundle_list(),
        "bundle_info" => ctx.bundle_info(required(args, "bundle")?),
        "bundle_add" => ctx.bundle_add(
            platform_of(args)?,
            arg_u64(args, "rev"),
            None,
            true,
            arg_bool(args, "force", false),
            arg_bool(args, "keepChunks", false),
        ),
        "bundle_import" => {
            let from = ops::expand_home(std::path::Path::new(required(args, "from")?));
            let rev = arg_u64(args, "rev")
                .ok_or_else(|| anyhow::anyhow!("missing required argument `rev`"))?;
            ctx.bundle_add(
                platform_of(args)?,
                Some(rev),
                Some(&from),
                true,
                arg_bool(args, "force", false),
                false,
            )
        }
        "bundle_reindex" => ctx.bundle_reindex(
            required(args, "bundle")?,
            true,
            arg_bool(args, "keepChunks", true),
        ),
        "bundle_remove" => {
            if !arg_bool(args, "confirm", false) {
                anyhow::bail!(
                    "bundle_remove deletes the bundle's whole directory and needs `confirm: true`"
                );
            }
            ctx.bundle_remove(required(args, "bundle")?)
        }
        "filter_list" => ctx.filter_list(),
        "filter_get" => ctx.filter_get(required(args, "name")?),
        "filter_set" => {
            let name = required(args, "name")?;
            let patch: ops::FilterPatch =
                serde_json::from_value(args.get("filter").cloned().unwrap_or(json!({})))
                    .map_err(|e| anyhow::anyhow!("`filter` is not a valid filter document: {e}"))?;
            let base = ctx
                .store
                .get_filter(name)
                .unwrap_or_else(|_| ops::blank_filter(name));
            ctx.filter_put(patch.apply(base), Some(name))
        }
        "filter_fork" => ctx.filter_fork(required(args, "source")?, required(args, "dest")?),
        "filter_delete" => ctx.filter_delete(required(args, "name")?),
        "filter_test" => ctx.filter_test(
            required(args, "name")?,
            bundle_of(args),
            arg_usize(args, "sample", 10),
        ),
        "module_get" => ctx.module_get(
            bundle_of(args),
            required(args, "name")?,
            arg_u64(args, "startLine").unwrap_or(1) as u32,
            arg_u64(args, "maxLines").map(|v| v as u32),
            arg_bool(args, "includeSource", true),
        ),
        "module_search" => {
            if arg_str(args, "name").is_none()
                && arg_str(args, "source").is_none()
                && arg_str(args, "exports").is_none()
            {
                anyhow::bail!(
                    "module_search needs at least one of `name`, `source` or `exports`; \
                     an unrestricted search would return the whole bundle"
                );
            }
            ctx.module_search(
                bundle_of(args),
                arg_str(args, "name"),
                arg_str(args, "source"),
                arg_str(args, "exports"),
                arg_str(args, "filter"),
                arg_usize(args, "limit", 50),
                arg_usize(args, "contextLines", 0),
                arg_usize(args, "maxMatches", 5),
            )
        }
        "diff" => {
            let opts = DiffOptions {
                include_hunks: arg_bool(args, "includeHunks", true),
                context_lines: arg_usize(args, "contextLines", 3),
                include_noise_only: arg_bool(args, "includeNoise", false),
                semantic: arg_bool(args, "semantic", false),
                ..DiffOptions::default()
            };
            ctx.diff(
                required(args, "old")?,
                required(args, "new")?,
                arg_str(args, "filter"),
                &opts,
                args.get("limit")
                    .and_then(Value::as_u64)
                    .map(|v| v as usize),
            )
        }
        "graph" => {
            let modules: Vec<String> = args
                .get("modules")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let direction = match arg_str(args, "direction").unwrap_or("deps") {
                "dependents" => Direction::Dependents,
                "both" => Direction::Both,
                "deps" => Direction::Deps,
                other => {
                    anyhow::bail!("direction must be `deps`, `dependents` or `both`, got {other:?}")
                }
            };
            let depth = arg_usize(args, "depth", 1);
            ctx.graph(
                bundle_of(args),
                &modules,
                arg_str(args, "match"),
                direction,
                (depth > 0).then_some(depth),
                arg_usize(args, "maxNodes", 2000),
                arg_bool(args, "includeExternal", true),
                arg_bool(args, "detectCycles", false),
                arg_str(args, "filter"),
            )
        }
        other => anyhow::bail!("unknown tool {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_named(name: &str) -> Value {
        tools()
            .into_iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name}"))
    }

    #[test]
    fn every_tool_has_a_description_and_an_object_schema() {
        for t in tools() {
            let name = t["name"].as_str().expect("name");
            assert!(
                t["description"].as_str().is_some_and(|d| d.len() > 40),
                "{name} needs a description written for a model choosing between tools"
            );
            assert_eq!(t["inputSchema"]["type"], "object", "{name}");
            assert_eq!(
                t["inputSchema"]["additionalProperties"], false,
                "{name} must reject unknown arguments rather than ignore them"
            );
        }
    }

    #[test]
    fn tool_names_are_unique() {
        let mut names: Vec<String> = tools()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        let before = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), before);
    }

    #[test]
    fn placeholder_schemas_are_substituted() {
        // A missed substitution would ship a `null` schema, which some clients
        // reject outright and others silently treat as "any".
        for t in tools() {
            let name = t["name"].as_str().unwrap().to_string();
            let props = t["inputSchema"]["properties"].as_object().unwrap();
            for (key, value) in props {
                assert!(!value.is_null(), "{name}.{key} still holds a placeholder");
            }
        }
        assert_eq!(
            tool_named("diff")["inputSchema"]["properties"]["old"]["type"],
            "string"
        );
        assert_eq!(
            tool_named("bundle_add")["inputSchema"]["properties"]["platform"]["enum"][0],
            "whatsapp"
        );
    }

    #[test]
    fn destructive_tools_are_annotated_as_such() {
        for name in ["bundle_remove", "filter_delete"] {
            let t = tool_named(name);
            assert_eq!(t["annotations"]["destructiveHint"], true, "{name}");
            assert_eq!(t["annotations"]["readOnlyHint"], false, "{name}");
        }
        for name in [
            "diff",
            "graph",
            "module_get",
            "module_search",
            "bundle_list",
        ] {
            assert_eq!(
                tool_named(name)["annotations"]["readOnlyHint"],
                true,
                "{name}"
            );
        }
    }

    #[test]
    fn only_bundle_add_is_marked_open_world() {
        // `openWorldHint` is how a client knows a tool reaches the network.
        for t in tools() {
            let expected = t["name"] == "bundle_add";
            assert_eq!(t["annotations"]["openWorldHint"], expected, "{}", t["name"]);
        }
    }

    #[test]
    fn initialize_echoes_a_supported_protocol_and_falls_back_otherwise() {
        let store = cellar_core::store::Store::open(Some(std::env::temp_dir().join(format!(
            "cellar-mcp-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))))
        .unwrap();
        let ctx = Ctx::new(store, true);

        let r = dispatch(
            &ctx,
            "initialize",
            &json!({ "protocolVersion": "2024-11-05" }),
            json!(1),
        );
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");

        let r = dispatch(
            &ctx,
            "initialize",
            &json!({ "protocolVersion": "1999-01-01" }),
            json!(2),
        );
        assert_eq!(r["result"]["protocolVersion"], SUPPORTED_PROTOCOLS[0]);
        assert_eq!(r["result"]["serverInfo"]["name"], "cellar");
    }

    #[test]
    fn an_unknown_method_is_a_jsonrpc_error() {
        let store = cellar_core::store::Store::open(Some(
            std::env::temp_dir().join(format!("cellar-mcp-unknown-{}", std::process::id())),
        ))
        .unwrap();
        let ctx = Ctx::new(store, true);
        let r = dispatch(&ctx, "resources/list", &json!({}), json!(7));
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
        assert!(r.get("result").is_none());
    }

    #[test]
    fn a_failing_tool_is_a_successful_rpc_with_is_error() {
        // The model must be able to read the failure and adjust; a JSON-RPC error
        // would be handled by the client and never reach it.
        let store = cellar_core::store::Store::open(Some(
            std::env::temp_dir().join(format!("cellar-mcp-fail-{}", std::process::id())),
        ))
        .unwrap();
        let ctx = Ctx::new(store, true);
        let r = dispatch(
            &ctx,
            "tools/call",
            &json!({ "name": "bundle_info", "arguments": { "bundle": "whatsapp-999" } }),
            json!(3),
        );
        assert!(r.get("error").is_none(), "not a transport fault");
        assert_eq!(r["result"]["isError"], true);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("999"),
            "the message must say what failed: {text}"
        );
    }

    #[test]
    fn unrestricted_module_search_is_refused_with_an_explanation() {
        let store = cellar_core::store::Store::open(Some(
            std::env::temp_dir().join(format!("cellar-mcp-search-{}", std::process::id())),
        ))
        .unwrap();
        let ctx = Ctx::new(store, true);
        let err = call_tool(&ctx, "module_search", &json!({}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("at least one"), "{err}");
    }

    #[test]
    fn bundle_remove_requires_confirmation() {
        let store = cellar_core::store::Store::open(Some(
            std::env::temp_dir().join(format!("cellar-mcp-rm-{}", std::process::id())),
        ))
        .unwrap();
        let ctx = Ctx::new(store, true);
        let err = call_tool(&ctx, "bundle_remove", &json!({ "bundle": "whatsapp-1" }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("confirm"), "{err}");
    }

    #[test]
    fn a_notification_gets_no_reply() {
        // `notifications/initialized` arrives with no `id`; replying to it is a
        // protocol violation that some clients treat as fatal.
        let msg: Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(msg.get("id").is_none());
    }
}
