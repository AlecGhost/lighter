use arborium::theme::{Style, Theme};
use arborium_highlight::{Span, spans_to_flat_tokens};

pub(crate) trait Backend {
    const STYLE_END: char;

    fn write_style(output: &mut String, style: &Style) -> usize;
    fn write_text(output: &mut String, text: &str);
}

pub(crate) fn render<B: Backend>(source: &str, spans: Vec<Span>, theme: &Theme) -> String {
    let source = source.trim_end_matches('\n');
    let tokens = spans_to_flat_tokens(source, spans);
    let (mut output, cursor) = tokens.into_iter().fold(
        (String::with_capacity(source.len() * 2), 0),
        |(mut output, cursor), token| {
            let start = token.start as usize;
            let end = token.end as usize;
            write_text::<B>(&mut output, &source[cursor..start]);

            match style_for_tag(theme, token.tag) {
                Some(style) => write_styled_text::<B>(&mut output, &source[start..end], style),
                None => write_text::<B>(&mut output, &source[start..end]),
            }

            (output, end)
        },
    );
    write_text::<B>(&mut output, &source[cursor..]);
    output
}

fn write_text<B: Backend>(output: &mut String, text: &str) {
    if !text.is_empty() {
        B::write_text(output, text);
    }
}

fn write_styled_text<B: Backend>(output: &mut String, text: &str, style: &Style) {
    text.split_inclusive('\n').for_each(|segment| {
        let (line, line_ending) = match segment.strip_suffix("\r\n") {
            Some(line) => (line, "\r\n"),
            None => match segment.strip_suffix('\n') {
                Some(line) => (line, "\n"),
                None => (segment, ""),
            },
        };
        write_styled_line::<B>(output, line, style);
        write_text::<B>(output, line_ending);
    });
}

fn write_styled_line<B: Backend>(output: &mut String, line: &str, style: &Style) {
    let groups = B::write_style(output, style);
    B::write_text(output, line);
    output.extend(std::iter::repeat_n(B::STYLE_END, groups));
}

fn style_for_tag<'a>(theme: &'a Theme, tag: &str) -> Option<&'a Style> {
    arborium_theme::tag_to_name(tag)
        .map(arborium_theme::capture_to_slot)
        .and_then(arborium_theme::slot_to_highlight_index)
        .and_then(|index| theme.style(index))
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::ops::Range;

    use arborium::theme::Theme;
    use arborium_highlight::Span;
    use arborium_theme::ThemeSlot;

    pub(crate) const THEME_SOURCE: &str = r##"
name = "Test theme"
variant = "light"

"keyword" = { fg = "mauve" }
"variable" = { fg = "blue" }
"function" = { fg = "mauve", modifiers = ["bold", "italic", "underline", "strikethrough"] }

[palette]
mauve = "#010203"
blue = "#040506"
"##;

    pub(crate) fn span(range: Range<usize>, slot: ThemeSlot, pattern_index: u32) -> Span {
        Span {
            start: range.start as u32,
            end: range.end as u32,
            capture: slot.name().unwrap().to_owned(),
            pattern_index,
        }
    }

    pub(crate) fn theme() -> Theme {
        Theme::from_toml(THEME_SOURCE).unwrap()
    }
}
