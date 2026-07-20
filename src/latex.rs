use std::fmt::Write;

use arborium::theme::{Style, Theme};
use arborium_highlight::{Span, spans_to_flat_tokens};

const COMMAND_CHARACTER: char = '\\';

pub(crate) fn spans_to_latex(source: &str, spans: Vec<Span>, theme: &Theme) -> String {
    let source = source.trim_end_matches('\n');
    let tokens = spans_to_flat_tokens(source, spans);
    let (mut output, cursor) = tokens.into_iter().fold(
        (String::with_capacity(source.len() * 2), 0),
        |(mut output, cursor), token| {
            let start = token.start as usize;
            let end = token.end as usize;
            write_latex_text(&mut output, &source[cursor..start]);

            let groups = style_for_tag(theme, token.tag)
                .map_or(0, |style| write_latex_style(&mut output, style));
            write_latex_text(&mut output, &source[start..end]);
            output.extend(std::iter::repeat_n('}', groups));

            (output, end)
        },
    );
    write_latex_text(&mut output, &source[cursor..]);
    output
}

fn style_for_tag<'a>(theme: &'a Theme, tag: &str) -> Option<&'a Style> {
    arborium_theme::tag_to_name(tag)
        .map(arborium_theme::capture_to_slot)
        .and_then(arborium_theme::slot_to_highlight_index)
        .and_then(|index| theme.style(index))
}

fn write_latex_style(output: &mut String, style: &Style) -> usize {
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

fn write_latex_text(output: &mut String, text: &str) {
    text.chars().for_each(|character| match character {
        COMMAND_CHARACTER | '{' | '}' => {
            output.push(COMMAND_CHARACTER);
            output.push(character);
        }
        character => output.push(character),
    });
}

#[cfg(test)]
mod tests {
    use arborium_theme::ThemeSlot;

    use super::*;

    const THEME_SOURCE: &str = r##"
name = "Test theme"
variant = "light"

"keyword" = { fg = "mauve" }
"variable" = { fg = "blue" }

[palette]
mauve = "#010203"
blue = "#040506"
"##;

    fn span(range: std::ops::Range<usize>, slot: ThemeSlot, pattern_index: u32) -> Span {
        Span {
            start: range.start as u32,
            end: range.end as u32,
            capture: slot.name().unwrap().to_owned(),
            pattern_index,
        }
    }

    fn render(source: &str, spans: Vec<Span>) -> String {
        spans_to_latex(source, spans, &Theme::from_toml(THEME_SOURCE).unwrap())
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
}
