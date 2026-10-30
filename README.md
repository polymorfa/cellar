# cellar

[![Ler em Português](https://img.shields.io/badge/Ler%20em-Portugu%C3%AAs-009c3b?style=for-the-badge)](README_BR.md)

> Este documento também está disponível em [português](README_BR.md).

A searchable archive of Meta's shipped client source. Download a WhatsApp Web
revision, diff it against another, read any module, and trace what depends on what.

WhatsApp Web ships as tens of thousands of minified JavaScript chunks. Each chunk
packs many `__d("ModuleName", [deps], factory, id)` definitions onto a single line.
`cellar` downloads a client revision, parses it with the [oxc](https://oxc.rs) AST,
and writes one file per module, named after the module. The result is a directory
that `grep -r`, your editor, and your coding agent all work on directly.

## Features

- Download and manage source bundles for any past client revision.
- Diff two revisions through a named filter, in JSON, NDJSON, Markdown or text.
- Search modules by name, source text, or exported symbol.
- Print any module's source, dependencies, dependents and exports.
- Build dependency and dependent graphs, as JSON, DOT or Mermaid.
- Serve all of the above to a coding agent over MCP.
- Supports `whatsapp`, `messenger`, `facebook` and `instagram`.

## Requirements

- A recent stable Rust toolchain (1.95 or newer).
- [`just`](https://github.com/casey/just), which runs the install and CI recipes.
- Around 1.3 GB of disk per indexed revision.

## Installation

Install `just` first:

```bash
brew install just        # macOS
cargo install just       # anywhere Rust works
```

Other options, including Debian, Fedora, Arch, Nix, Scoop and a prebuilt binary,
are in [`just`'s install guide](https://github.com/casey/just#installation).

Then build cellar:

```bash
git clone https://github.com/polymorfa/cellar
cd cellar
just install
```

That puts `cellar` at `~/.cargo/bin/cellar`. Make sure `~/.cargo/bin` is on your
`PATH`.

## Documentation

Full documentation, including a worked example that uncovers an unannounced feature
in four commands, is at **[cellar.mintlify.site](https://cellar.mintlify.site)**.

| Page | What it covers |
| --- | --- |
| [AI assistants](https://cellar.mintlify.site/agents) | MCP setup for Claude Code and Codex, and the sixteen tools |
| [A real example](https://cellar.mintlify.site/walkthrough) | Finding passkey device linking before it shipped |
| [Managing versions](https://cellar.mintlify.site/bundles) | Download, import, inspect and remove releases |
| [Finding code](https://cellar.mintlify.site/search) | Search by name, source or exported symbol |
| [Comparing releases](https://cellar.mintlify.site/diff) | Diffs as text, JSON, NDJSON or Markdown |
| [Graphs](https://cellar.mintlify.site/graph) | Dependency graphs as Mermaid, Graphviz or JSON |
| [Filters](https://cellar.mintlify.site/filters) | Cutting 187,000 modules down to what matters |

Every command also links to its own page from `--help`.

## Project layout

| Crate | Role |
| --- | --- |
| `cellar-core` | Store layout, index model, filter engine, diff, graphs. No I/O beyond the filesystem. |
| `cellar-index` | oxc bundle parsing into a module index. |
| `cellar-fetch` | Revision discovery, download and unpacking. The only crate that reaches the network. |
| `cellar` | CLI and MCP server, both built on one shared operations layer. |

## Development

```bash
just ci        # fmt-check, clippy, tests, skill check
```

No test may reach the network. CI enforces this.

## Credits

- [wa-spec](https://github.com/vinikjkkj/wa-spec) by vini, which extracts WhatsApp Web
  protocol bindings from the same bundles daily.
- [whatspec](https://github.com/oxidezap/whatspec) by João Lucas, which extracts a
  typed protocol IR from the same bundles and shaped the design here.
- ProtoCocktail's `wa-diff-analyzer`, my earlier attempt at this problem, whose module
  filter ruleset is ported here as the `default` filter.
- [WARDEN](https://warden-re.io), my own reverse-engineering project, which inspired
  this one.
- [meta-code-verify](https://github.com/facebookincubator/meta-code-verify), Meta's
  own source-integrity extension, which documents the `btarchive` endpoint.

## Get support

If you'd like business to enterprise-level support from Rajeh, you can book a video
chat. Book a 1 hour time slot by contacting him on Discord or pre-ordering
[here](https://purpshell.dev/book). The earlier you pre-order the better, as his
time slots usually fill up very quickly.

If you are a business, we encourage you to contribute back to the development costs
of the project. You can do so by booking meetings or sponsoring below. All support
is welcome from businesses of all sizes.

## Sponsor

If you'd like to financially support this project, you can do so
[here](https://purpshell.dev/sponsor).

## Disclaimer

> [!CAUTION]
> This project is not affiliated, associated, authorized, endorsed by, or in any way
> officially connected with WhatsApp or any of its subsidiaries or its affiliates.
> The official WhatsApp website can be found at whatsapp.com. "WhatsApp" as well as
> related names, marks, emblems and images are registered trademarks of their
> respective owners.
>
> `cellar` reads publicly served client bundles for interoperability research. The
> maintainers do not condone the use of this project in practices that violate the
> Terms of Service of WhatsApp, and call upon the personal responsibility of its
> users to use it fairly.

## License

Copyright (c) 2026 Rajeh Taher
Copyright (c) 2026 Polymorfa (Softium LLC)

Licensed under the MIT License. See [LICENSE](LICENSE) for the full text.
