# Lighter

Lighter is a command-line source-code highlighter.
It combines tree-sitter syntax highlighting,
provided by the amazing [Arborium](https://github.com/bearcove/arborium) project,
with semantic tokens from language servers,
then writes the result as terminal colours, HTML, LaTeX, or Typst.

Arborium "gets you 90% of the way", is easy to use and can highlight a website dynamically.
Lighter is for the perfectionists among us,
who want the same quality of code highlighting as in an IDE.
The trade-off is a lack of portability–it doesn't run in the browser,
thus can't highlight code client-side.
Instead, it generates code statically,
based on information from locally installed language servers.

See the [live highlighter comparison](https://alecghost.github.io/lighter/) for
an interactive comparison of Highlight.js, Arborium, and Lighter output.

## Features

- Syntax highlighting for the languages supported by Arborium
- Semantic highlighting through the Language Server Protocol (LSP)
- ANSI, HTML, LaTeX, and Typst output
- Built-in and custom themes
- Line selection without losing whole-file semantic context
- A background daemon that keeps language servers warm between invocations

## Installation

Lighter is currently installed from source.
It requires a recent stable Rust toolchain.

```sh
git clone https://github.com/AlecGhost/lighter.git
cd lighter
cargo install --path .
```

Language servers are separate programs.
Install the server for the language whose semantic highlighting you want.

## Quick start

If the matching language server is installed,
expose the project directory as its workspace:

```sh
lighter --project . src/main.rs
```

When input comes from standard input, specify the language:

```sh
printf 'fn main() {}\n' | lighter --lang rust
```

## Usage

```text
lighter [OPTIONS] [FILE]
lighter daemon <COMMAND>
```

`FILE` is optional. Lighter infers the language from a file's extension. With
standard input, `--lang` is required.

The most useful options are:

| Option | Purpose |
| --- | --- |
| `-p, --project <DIR>` | Use a directory as the language server workspace |
| `-f, --format <FORMAT>` | Select `ansi`, `html`, `latex`, or `typst` |
| `-l, --lang <LANG>` | Set the language instead of detecting it |
| `--lines <RANGE>` | Render an inclusive, one-based line range |
| `--theme <THEME>` | Select a built-in theme |
| `--custom-theme <FILE>` | Load an Arborium TOML theme |
| `-c, --config <FILE>` | Load a Lighter TOML configuration file |

Run `lighter --help` for the complete interface and the available built-in
themes.

### Select lines

Ranges are inclusive and one-based:

```sh
lighter --lines 10:20 src/main.rs
lighter --lines :20 src/main.rs
lighter --lines 10: src/main.rs
```

Lighter still analyzes the complete source before selecting the requested
lines. This lets a language server use declarations and imports outside the
rendered range.

### Output formats

`ansi` is the default and emits terminal colour escape sequences.

```sh
lighter  src/main.rs
```

`html` emits an HTML fragment using Arborium custom elements such as `<a-k>`
and `<a-f>`. It does not emit a complete page or CSS, so the embedding page
must style those elements.
It is recommended to link to Arborium's [base.css](https://cdn.jsdelivr.net/npm/@arborium/arborium@2.18.1/dist/themes/base.css)
and one of its [theme files](https://www.jsdelivr.com/package/npm/@arborium/arborium?tab=files&path=dist%2Fthemes),
e.g. [catppuccin-mocha.css](https://cdn.jsdelivr.net/npm/@arborium/arborium@2.18.1/dist/themes/catppuccin-mocha.css).

```sh
lighter --format html src/main.rs > snippet.html
```

`latex` emits commands for an `fvextra` `Verbatim` environment with `xcolor`
enabled and `commandchars=\\\{\}`:

```sh
lighter --format latex src/main.rs > snippet.tex
```

A minimal wrapper looks like this:

```latex
\documentclass{article}
\usepackage{xcolor}
\usepackage{fvextra}
\begin{document}
\begin{Verbatim}[commandchars=\\\{\}]
\input{snippet.tex}
\end{Verbatim}
\end{document}
```

I hacked together a [package](./latex/) that invokes lighter automatically for you,
with a similar interface as minted.

`typst` emits a Typst `block` composed from styled `raw` elements:

```sh
lighter --format typst src/main.rs > snippet.typ
```

The fragment can be included directly:

```typst
#include "snippet.typ"
```

## Semantic highlighting

Lighter enables both tree-sitter and LSP highlighting by default. It lazily
starts a language server the first time a language is used. The server must be
available on `PATH` and support full-document semantic tokens.

Built-in server commands are provided for:

| Languages | Command |
| --- | --- |
| Rust | `rust-analyzer` |
| Python | `basedpyright-langserver --stdio` |
| JavaScript, JSX, TypeScript, TSX | `typescript-language-server --stdio` |
| C, C++ | `clangd` |
| Go | `gopls` |
| Java | `jdtls` |
| Lua | `lua-language-server` |
| Zig | `zls` |
| Ruby | `ruby-lsp` |
| Kotlin | `kotlin-lsp` |
| Swift | `sourcekit-lsp` |
| Haskell | `haskell-language-server-wrapper --lsp` |
| OCaml | `ocamllsp` |
| Dart | `dart language-server` |

If no server is configured, its executable is missing, or it does not provide
semantic tokens, Lighter reports an error.

Pass the source file whenever possible so the language server sees its real
path. For standard input, Lighter creates a temporary document. Use
`--project <DIR>` when the server needs project configuration, dependencies, or
other workspace files.

## Configuration

If you want to supply your own servers or add custom capture mappings, use
`--config`:

```sh
lighter --config lighter.toml src/main.rs
```

A configuration can choose a theme, add or replace language-server commands,
provide server settings, and map LSP token names to Arborium captures:

```toml
theme = "Tokyo Night"

[servers]
gleam = "gleam lsp"

[servers.go]
command = "gopls"
config = { gopls = { semanticTokens = true } }

[captures]
parameter = "variable.parameter"

[captures.rust]
const = "constant"
decorator = "constant"
```

A custom theme can be selected relative to the configuration file:

```toml
theme = { path = "theme.toml" }
```

## Daemon

Starting a daemon keeps language-server processes alive, avoiding their startup
cost on every invocation:

```sh
lighter daemon spawn
lighter --project . src/main.rs
lighter daemon kill
```

Normal invocations discover and use the daemon automatically. Startup options
set its defaults:

```sh
lighter daemon spawn \
  --config lighter.toml \
  --theme "Tokyo Night" \
  --format ansi
```

Per-invocation `--format`, `--theme`, and `--custom-theme` options
override the daemon defaults. Passing `--config`
updates the active server and capture configuration for that daemon session.

## Troubleshooting

**`No language server available for …`**

The language has no built-in or configured server. Add an entry under
`[servers]`, or pass `--no-lsp`.

**`Failed to start server for …`**

Install the configured language server and make sure its executable is on
`PATH`.

**The language cannot be detected**

Use an explicit language:

```sh
lighter --lang rust path/to/source
```

**The language server does not understand the project**

Pass its workspace directory:

```sh
lighter --project path/to/project path/to/project/src/main.rs
```
