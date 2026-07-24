# lighter LaTeX package

This LuaLaTeX package highlights code with [`lighter`](https://github.com/AlecGhost/lighter)
and typesets its LaTeX response through `fvextra`'s `Verbatim`.
The result are code blocks like `minted`, but with accurate syntactic and semantic highlighting.

```tex
\usepackage{lighter/lighter}

\begin{lighter}{python}
def hello(name: str) -> str:
    return f"Hello, {name}!"
\end{lighter}

\inputlighter{python}{path/to/file.py}
```

Set startup options when loading the package. A built-in theme uses `theme`,
while a theme TOML file uses `custom-theme`; these options are mutually
exclusive. `config` selects a lighter configuration file.
Theme options are provided by [`arborium`](https://arborium.bearcove.eu/).
If you want to supply your own theme, I suggest modifying one of [these](https://github.com/bearcove/arborium/tree/main/crates/arborium-theme/themes).

```tex
\usepackage[
  theme={Catppuccin Latte},
  config={path/to/lighter.toml}
]{lighter/lighter}

% Or load a custom theme:
\usepackage[custom-theme={path/to/theme.toml}]{lighter/lighter}
```

Both block forms accept an optional inclusive, one-based line range. Either
endpoint may be omitted:

```tex
\begin{lighter}[2:3]{python}
excluded = "setup"
first_included = 1
second_included = 2
\end{lighter}

\inputlighter[:20]{python}{path/to/file.py}
\inputlighter[10:]{python}{path/to/file.py}
```

Use inline highlighting with either a braced or delimiter-style verbatim
argument:

```tex
\inlinelighter{python}{get_data() -> str}
\inlinelighter{python}|dict[str, int]|
```
