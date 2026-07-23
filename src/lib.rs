use std::cell::RefCell;
use std::io::Write;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::str::FromStr;

use arborium::advanced::{Span, spans_to_ansi, spans_to_html};
use arborium::theme::Theme;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod config;
pub mod daemon;
mod latex;
pub mod logging;
pub mod lsp;
pub mod theme;

const TREE_SITTER_SPANS_HEADING: &str = "Tree-sitter spans:";
const LSP_SPANS_HEADING: &str = "LSP spans:";

pub type LangName = Rc<str>;

#[derive(Debug, PartialEq, Eq)]
pub struct Input<'a> {
    pub source: &'a str,
    pub path: Option<&'a Path>,
    pub project: Option<&'a Path>,
    pub lang: LangName,
}

/// An inclusive, one-based range of source lines to render.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct LineRange {
    start: usize,
    end: Option<usize>,
}

impl FromStr for LineRange {
    type Err = LineRangeError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let (start, end) = value.split_once(':').ok_or(LineRangeError)?;
        if start.is_empty() && end.is_empty() {
            return Err(LineRangeError);
        }

        let start = parse_line_number(start)?.unwrap_or(1);
        let end = parse_line_number(end)?;

        match end {
            Some(end) if end < start => Err(LineRangeError),
            _ => Ok(Self { start, end }),
        }
    }
}

fn parse_line_number(value: &str) -> std::result::Result<Option<usize>, LineRangeError> {
    match value {
        "" => Ok(None),
        value => value
            .parse::<usize>()
            .ok()
            .filter(|line| *line > 0)
            .map(Some)
            .ok_or(LineRangeError),
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Error)]
#[error("expected a one-based line range: start:end, :end, or start:")]
pub struct LineRangeError;

#[derive(
    Debug, Default, Clone, Copy, Eq, Hash, PartialEq, clap::ValueEnum, Deserialize, Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    #[default]
    Ansi,
    Html,
    /// LaTeX commands intended for an `xcolor`-enabled fvextra `Verbatim`
    /// environment using `commandchars=\\\{\}`.
    ///
    /// The command characters '\', '{' and '}' are escaped with '\'.
    Latex,
}

#[derive(Debug)]
pub struct HighlightOptions {
    pub output: Output,
    pub lsp: bool,
    pub tree_sitter: bool,
    pub theme: Theme,
    pub lines: Option<LineRange>,
}

impl Default for HighlightOptions {
    fn default() -> Self {
        Self {
            output: Output::Ansi,
            lsp: true,
            theme: theme::default(),
            tree_sitter: true,
            lines: None,
        }
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Arborium(#[from] arborium::Error),
    #[error(transparent)]
    Server(#[from] lsp::Error),
    #[error("Could not write debug spans: {0}")]
    DebugOutput(#[source] std::io::Error),
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Highlighter {
    registry: Rc<RefCell<lsp::ServerRegistry>>,
    options: HighlightOptions,
    log: logging::LogLevel,
}

impl Highlighter {
    pub fn new(registry: Rc<RefCell<lsp::ServerRegistry>>) -> Self {
        Self::with_options(
            registry,
            HighlightOptions::default(),
            logging::LogLevel::default(),
        )
    }

    pub fn with_options(
        registry: Rc<RefCell<lsp::ServerRegistry>>,
        options: HighlightOptions,
        log: logging::LogLevel,
    ) -> Self {
        Self {
            registry,
            options,
            log,
        }
    }

    pub fn set_options(&mut self, options: HighlightOptions) {
        self.options = options;
    }

    /// Highlight source with tree-sitter syntax and LSP semantic information.
    pub fn highlight(&self, input: Input<'_>) -> Result<String> {
        let tree_sitter_spans = if self.options.tree_sitter {
            // TODO: get injections and add lsp highlighting for injected languages
            arborium::Highlighter::new().highlight_spans(&input.lang, input.source)?
        } else {
            Vec::new()
        };

        let lsp_spans = match self.options.lsp {
            true => {
                let pattern_index = next_pattern_index(&tree_sitter_spans);
                self.registry
                    .try_borrow_mut()
                    .expect("Server registry already borrowed")
                    .get_server(input.lang.clone(), input.project)?
                    .get_semantic_spans(input.source, input.path, pattern_index)?
            }
            false => Vec::new(),
        };

        write_debug_spans(
            &mut std::io::stderr().lock(),
            input.source,
            &tree_sitter_spans,
            &lsp_spans,
            self.log,
        )
        .map_err(Error::DebugOutput)?;

        Ok(render(
            input.source,
            tree_sitter_spans,
            lsp_spans,
            self.options.output,
            &self.options.theme,
            self.options.lines,
        ))
    }
}

fn next_pattern_index(spans: &[Span]) -> u32 {
    spans
        .iter()
        .map(|span| span.pattern_index)
        .max()
        .unwrap_or_default()
        .saturating_add(1)
}

fn render(
    source: &str,
    tree_sitter_spans: Vec<Span>,
    lsp_spans: Vec<Span>,
    output: Output,
    theme: &Theme,
    lines: Option<LineRange>,
) -> String {
    let spans = merge_spans(tree_sitter_spans, lsp_spans);
    let (source, spans) = match lines {
        Some(lines) => select_lines(source, spans, lines),
        None => (source, spans),
    };
    match output {
        Output::Ansi => spans_to_ansi(source, spans, theme),
        Output::Html => spans_to_html(source, spans, &arborium::HtmlFormat::default()),
        Output::Latex => latex::spans_to_latex(source, spans, theme),
    }
}

fn select_lines(source: &str, spans: Vec<Span>, lines: LineRange) -> (&str, Vec<Span>) {
    let selected = lines.byte_range(source);
    let offset = selected.start as u32;
    let end = selected.end as u32;
    let spans = spans
        .into_iter()
        .filter_map(|span| {
            let start = span.start.max(offset);
            let end = span.end.min(end);
            (start < end).then(|| Span {
                start: start - offset,
                end: end - offset,
                ..span
            })
        })
        .collect();

    (&source[selected], spans)
}

impl LineRange {
    fn byte_range(self, source: &str) -> Range<usize> {
        let line_start = |line| {
            std::iter::once(0)
                .chain(source.match_indices('\n').map(|(newline, _)| newline + 1))
                .nth(line - 1)
                .unwrap_or(source.len())
        };
        let start = line_start(self.start);
        let end = self
            .end
            .and_then(|line| line.checked_add(1))
            .map(line_start)
            .unwrap_or(source.len());

        start..end
    }
}

fn merge_spans(mut tree_sitter_spans: Vec<Span>, lsp_spans: Vec<Span>) -> Vec<Span> {
    tree_sitter_spans.extend(lsp_spans);
    tree_sitter_spans
}

fn render_debug_spans(source: &str, tree_sitter_spans: &[Span], lsp_spans: &[Span]) -> String {
    let mut tree_sitter_spans = tree_sitter_spans.to_vec();
    let mut lsp_spans = lsp_spans.to_vec();
    sort_spans_by_position(&mut tree_sitter_spans);
    sort_spans_by_position(&mut lsp_spans);

    let tree_sitter_lines = format_span_lines(source, &tree_sitter_spans);
    let lsp_lines = format_span_lines(source, &lsp_spans);
    format!("{TREE_SITTER_SPANS_HEADING}\n{tree_sitter_lines}\n\n{LSP_SPANS_HEADING}\n{lsp_lines}")
}

fn write_debug_spans(
    writer: &mut impl Write,
    source: &str,
    tree_sitter_spans: &[Span],
    lsp_spans: &[Span],
    log: logging::LogLevel,
) -> std::io::Result<()> {
    if !log.includes(logging::LogLevel::Debug) {
        return Ok(());
    }

    writeln!(
        writer,
        "{}",
        render_debug_spans(source, tree_sitter_spans, lsp_spans)
    )
}

fn sort_spans_by_position(spans: &mut [Span]) {
    spans.sort_by_key(|span| (span.start, span.end));
}

fn format_span_lines(source: &str, spans: &[Span]) -> String {
    spans
        .iter()
        .map(|span| {
            let text = source
                .get(span.start as usize..span.end as usize)
                .unwrap_or_default();
            format!("{span:?} {text:?}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use arborium_theme::ThemeSlot;

    use super::*;

    const SEMANTIC_PATTERN_INDEX: u32 = 42;
    const THEME_SOURCE: &str = r##"
name = "Test theme"
variant = "light"

"keyword" = { fg = "mauve" }

[palette]
mauve = "#010203"
"##;

    #[test]
    fn applies_parsed_theme_colors() {
        let theme = Theme::from_toml(THEME_SOURCE).unwrap();
        let source = "let";
        let spans = vec![Span {
            start: 0,
            end: source.len() as u32,
            capture: ThemeSlot::Keyword.name().unwrap().to_owned(),
            pattern_index: 0,
        }];
        let rendered = render(source, spans, Vec::new(), Output::Ansi, &theme, None);

        assert!(rendered.contains("\u{1b}[38;2;1;2;3m"));
    }

    #[test]
    fn parses_supported_line_range_forms() {
        let ranges = [
            (
                "2:4",
                LineRange {
                    start: 2,
                    end: Some(4),
                },
            ),
            (
                ":4",
                LineRange {
                    start: 1,
                    end: Some(4),
                },
            ),
            (
                "2:",
                LineRange {
                    start: 2,
                    end: None,
                },
            ),
        ];

        ranges.into_iter().for_each(|(value, expected)| {
            assert_eq!(value.parse(), Ok(expected), "failed to parse {value:?}");
        });
    }

    #[test]
    fn rejects_invalid_line_ranges() {
        ["", ":", "1", "0:1", "1:0", "3:2", "a:2", "1:2:3"]
            .into_iter()
            .for_each(|value| {
                assert_eq!(
                    value.parse::<LineRange>(),
                    Err(LineRangeError),
                    "accepted {value:?}"
                );
            });
    }

    #[test]
    fn selects_closed_and_open_line_ranges_at_utf8_boundaries() {
        const SOURCE: &str = "one\nβeta\nthree\nfour";
        let selections = [
            ("2:3", "βeta\nthree\n"),
            (":2", "one\nβeta\n"),
            ("3:", "three\nfour"),
            ("9:", ""),
        ];

        selections.into_iter().for_each(|(range, expected)| {
            let (source, spans) = select_lines(SOURCE, Vec::new(), range.parse().unwrap());
            assert_eq!(source, expected, "selected the wrong source for {range}");
            assert!(spans.is_empty());
        });
    }

    #[test]
    fn clips_and_rebases_spans_to_selected_lines() {
        const SOURCE: &str = "first\nsecond\nthird";
        let spans = vec![
            span(0, 5, "before", 0),
            span(4, 8, "overlap-start", 1),
            span(7, 12, "inside", 2),
            span(11, 15, "overlap-end", 3),
            span(14, 19, "after", 4),
        ];

        let (source, spans) = select_lines(SOURCE, spans, "2:2".parse().unwrap());

        assert_eq!(source, "second\n");
        assert_eq!(
            spans,
            vec![
                span(0, 2, "overlap-start", 1),
                span(1, 6, "inside", 2),
                span(5, 7, "overlap-end", 3),
            ]
        );
    }

    #[test]
    fn renders_only_selected_lines_for_each_output_format() {
        const SOURCE: &str = "first\nsecond\nthird";
        let theme = Theme::from_toml(THEME_SOURCE).unwrap();

        [Output::Ansi, Output::Html, Output::Latex]
            .into_iter()
            .for_each(|output| {
                let rendered = render(
                    SOURCE,
                    Vec::new(),
                    Vec::new(),
                    output,
                    &theme,
                    Some("2:2".parse().unwrap()),
                );
                assert_eq!(rendered, "second");
            });
    }

    #[test]
    fn disabled_lsp_does_not_request_a_server() {
        const SOURCE: &str = "plain source";
        let registry = Rc::new(RefCell::new(lsp::ServerRegistry::default()));
        let highlighter = Highlighter::with_options(
            registry,
            HighlightOptions {
                lsp: false,
                tree_sitter: false,
                ..HighlightOptions::default()
            },
            logging::LogLevel::default(),
        );

        let output = highlighter
            .highlight(Input {
                source: SOURCE,
                path: None,
                project: None,
                lang: LangName::from("no-server"),
            })
            .unwrap();

        assert_eq!(output, SOURCE);
    }

    fn assert_ordered(text: &str, first: &str, second: &str) {
        assert!(
            text.find(first).unwrap() < text.find(second).unwrap(),
            "{first:?} should precede {second:?} in {text:?}"
        );
    }

    #[test]
    fn debug_logging_separates_and_sorts_span_sources() {
        const DEBUG_SOURCE: &str = "let call value";
        let tree_sitter_spans = vec![
            span(9, 14, ThemeSlot::Variable.name().unwrap(), 1),
            span(0, 3, ThemeSlot::Keyword.name().unwrap(), 0),
        ];
        let lsp_spans = vec![
            span(
                9,
                14,
                ThemeSlot::Variable.name().unwrap(),
                SEMANTIC_PATTERN_INDEX,
            ),
            span(
                4,
                8,
                ThemeSlot::Function.name().unwrap(),
                SEMANTIC_PATTERN_INDEX,
            ),
        ];

        let rendered = render_debug_spans(DEBUG_SOURCE, &tree_sitter_spans, &lsp_spans);
        let sections = format!("\n\n{LSP_SPANS_HEADING}\n");
        let (tree_sitter_output, lsp_output) = rendered.split_once(&sections).unwrap();

        assert!(tree_sitter_output.starts_with(TREE_SITTER_SPANS_HEADING));
        assert_ordered(tree_sitter_output, "\"let\"", "\"value\"");
        assert!(!tree_sitter_output.contains("\"call\""));
        assert_ordered(lsp_output, "\"call\"", "\"value\"");
        assert!(!lsp_output.contains("\"let\""));

        let mut info_output = Vec::new();
        write_debug_spans(
            &mut info_output,
            DEBUG_SOURCE,
            &tree_sitter_spans,
            &lsp_spans,
            logging::LogLevel::Info,
        )
        .unwrap();
        let mut debug_output = Vec::new();
        write_debug_spans(
            &mut debug_output,
            DEBUG_SOURCE,
            &tree_sitter_spans,
            &lsp_spans,
            logging::LogLevel::Debug,
        )
        .unwrap();

        assert!(info_output.is_empty());
        assert_eq!(
            String::from_utf8(debug_output).unwrap(),
            format!("{rendered}\n")
        );
    }

    fn span(start: u32, end: u32, capture: &str, pattern_index: u32) -> Span {
        Span {
            start,
            end,
            capture: capture.to_owned(),
            pattern_index,
        }
    }
}
