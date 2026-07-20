use std::io::Write;
use std::path::Path;
use std::rc::Rc;

use arborium::advanced::{Span, spans_to_ansi, spans_to_html};
use arborium::theme::Theme;
use thiserror::Error;

pub mod logging;
pub mod lsp;

const TREE_SITTER_SPANS_HEADING: &str = "Tree-sitter spans:";
const LSP_SPANS_HEADING: &str = "LSP spans:";

pub type LangName = Rc<str>;

#[derive(Debug, Default, Clone, Copy, clap::ValueEnum)]
pub enum Output {
    #[default]
    Ansi,
    Html,
}

pub struct HighlightOptions {
    pub output: Output,
    pub tree_sitter: bool,
    pub theme: Theme,
    pub log: logging::LogLevel,
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

/// Highlight source with tree-sitter syntax and LSP semantic information.
pub fn highlight(
    input: &str,
    path: Option<&Path>,
    lang: LangName,
    registry: &mut lsp::ServerRegistry,
    options: &HighlightOptions,
) -> Result<String> {
    let tree_sitter_spans = if options.tree_sitter {
        // TODO: get injections and add lsp highlighting for injected languages
        arborium::Highlighter::new().highlight_spans(&lang, input)?
    } else {
        Vec::new()
    };

    let pattern_index = next_pattern_index(&tree_sitter_spans);
    let lsp_spans =
        registry
            .get_server(lang.clone())?
            .get_semantic_spans(input, path, pattern_index)?;

    write_debug_spans(
        &mut std::io::stderr().lock(),
        input,
        &tree_sitter_spans,
        &lsp_spans,
        options.log,
    )
    .map_err(Error::DebugOutput)?;

    Ok(render(
        input,
        tree_sitter_spans,
        lsp_spans,
        options.output,
        &options.theme,
    ))
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
) -> String {
    let spans = merge_spans(tree_sitter_spans, lsp_spans);
    match output {
        Output::Ansi => spans_to_ansi(source, spans, theme),
        Output::Html => spans_to_html(source, spans, &arborium::HtmlFormat::default()),
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

        let rendered = render(source, spans, Vec::new(), Output::Ansi, &theme);

        assert!(rendered.contains("\u{1b}[38;2;1;2;3m"));
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
