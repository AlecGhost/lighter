use std::fmt::Write;

use arborium::theme::{Style, Theme};
use arborium_highlight::Span;

use crate::styled::{self, Backend};

const COMMAND_CHARACTER: char = '\\';

pub(crate) fn spans_to_latex(source: &str, spans: Vec<Span>, theme: &Theme) -> String {
    styled::render::<Latex>(source, spans, theme)
}

struct Latex;

impl Backend for Latex {
    const STYLE_END: char = '}';

    fn write_style(output: &mut String, style: &Style) -> usize {
        let mut groups = 0;
        if let Some(color) = style.fg {
            output.push(COMMAND_CHARACTER);
            output.push_str("textcolor[HTML]{");
            write!(output, "{:02X}{:02X}{:02X}", color.r, color.g, color.b)
                .expect("writing to a String cannot fail");
            output.push_str("}{");
            groups += 1;
        }

        [
            (style.modifiers.bold, "textbf{"),
            (style.modifiers.italic, "textit{"),
            (style.modifiers.underline, "underline{"),
            (style.modifiers.strikethrough, "sout{"),
        ]
        .into_iter()
        .filter(|(enabled, _)| *enabled)
        .for_each(|(_, latex_command)| {
            output.push(COMMAND_CHARACTER);
            output.push_str(latex_command);
            groups += 1;
        });
        groups
    }

    fn write_text(output: &mut String, text: &str) {
        text.chars().for_each(|character| match character {
            COMMAND_CHARACTER | '{' | '}' => {
                output.push(COMMAND_CHARACTER);
                output.push(character);
            }
            character => output.push(character),
        });
    }
}

#[cfg(test)]
mod tests {
    use arborium_theme::ThemeSlot;

    use super::*;
    use crate::styled::test_support::{span, theme};

    fn render(source: &str, spans: Vec<Span>) -> String {
        spans_to_latex(source, spans, &theme())
    }

    #[test]
    fn renders_flat_tokens_and_unstyled_gaps_from_source() {
        const SOURCE: &str = "let value";
        const EXPECTED: &str = r"\textcolor[HTML]{010203}{let} \textcolor[HTML]{040506}{value}";
        let spans = vec![
            span(0..3, ThemeSlot::Keyword, 0),
            span(4..SOURCE.len(), ThemeSlot::Variable, 0),
        ];

        assert_eq!(render(SOURCE, spans), EXPECTED);
    }

    #[test]
    fn resolves_equal_span_ranges_by_pattern_priority() {
        const SOURCE: &str = "let";
        const EXPECTED: &str = r"\textcolor[HTML]{010203}{let}";
        let spans = vec![
            span(0..SOURCE.len(), ThemeSlot::Variable, 0),
            span(0..SOURCE.len(), ThemeSlot::Keyword, 1),
        ];

        assert_eq!(render(SOURCE, spans), EXPECTED);
    }

    #[test]
    fn escapes_fancyverb_command_characters_and_preserves_other_text() {
        const SOURCE: &str = r#"\{}<>#%$&_~^"'😀"#;
        const EXPECTED: &str = r##"\textcolor[HTML]{010203}{\\\{\}<>#%$&_~^"'😀}"##;
        let spans = vec![span(0..SOURCE.len(), ThemeSlot::Keyword, 0)];

        assert_eq!(render(SOURCE, spans), EXPECTED);
    }

    #[test]
    fn renders_all_text_modifiers() {
        const SOURCE: &str = "call";
        const EXPECTED: &str =
            r"\textcolor[HTML]{010203}{\textbf{\textit{\underline{\sout{call}}}}}";
        let spans = vec![span(0..SOURCE.len(), ThemeSlot::Function, 0)];

        assert_eq!(render(SOURCE, spans), EXPECTED);
    }
}
