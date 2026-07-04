//! Terminal hyperlink metadata carried beside visible Ratatui lines.
//!
//! OSC 8 bytes must not participate in layout measurement. This module keeps
//! destinations separate from text so wrapping and height calculations stay
//! based on visible columns.

use std::ops::Range;

use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalHyperlink {
    pub columns: Range<usize>,
    pub destination: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HyperlinkLine {
    pub line: Line<'static>,
    pub hyperlinks: Vec<TerminalHyperlink>,
}

impl HyperlinkLine {
    pub fn new(line: Line<'static>) -> Self {
        Self {
            line,
            hyperlinks: Vec::new(),
        }
    }

    pub fn width(&self) -> usize {
        self.line.width()
    }

    pub fn push_span(&mut self, span: Span<'static>, destination: Option<&str>) {
        let start = self.width();
        let end = start + span.content.width();
        self.line.push_span(span);
        if end > start
            && let Some(destination) = destination
        {
            self.hyperlinks.push(TerminalHyperlink {
                columns: start..end,
                destination: destination.to_string(),
            });
        }
    }
}

impl From<Line<'static>> for HyperlinkLine {
    fn from(line: Line<'static>) -> Self {
        Self::new(line)
    }
}

impl From<&'static str> for HyperlinkLine {
    fn from(text: &'static str) -> Self {
        Self::new(Line::from(text))
    }
}

impl From<String> for HyperlinkLine {
    fn from(text: String) -> Self {
        Self::new(Line::from(text))
    }
}

pub fn visible_lines(lines: Vec<HyperlinkLine>) -> Vec<Line<'static>> {
    lines.into_iter().map(|line| line.line).collect()
}

pub fn plain_hyperlink_lines(lines: Vec<Line<'static>>) -> Vec<HyperlinkLine> {
    lines.into_iter().map(HyperlinkLine::new).collect()
}

pub fn prefix_hyperlink_lines(
    lines: Vec<HyperlinkLine>,
    initial_prefix: Span<'static>,
    subsequent_prefix: Span<'static>,
) -> Vec<HyperlinkLine> {
    lines
        .into_iter()
        .enumerate()
        .map(|(index, mut line)| {
            let prefix = if index == 0 {
                initial_prefix.clone()
            } else {
                subsequent_prefix.clone()
            };
            let shift = prefix.content.width();
            let mut spans = Vec::with_capacity(line.line.spans.len() + 1);
            spans.push(prefix);
            spans.extend(line.line.spans);
            line.line = Line::from(spans).style(line.line.style);
            for hyperlink in &mut line.hyperlinks {
                hyperlink.columns = hyperlink.columns.start + shift..hyperlink.columns.end + shift;
            }
            line
        })
        .collect()
}

pub fn annotate_web_urls(lines: Vec<Line<'static>>) -> Vec<HyperlinkLine> {
    lines.into_iter().map(annotate_web_urls_in_line).collect()
}

pub fn annotate_web_urls_in_line(line: Line<'static>) -> HyperlinkLine {
    let text = line_text(&line);
    let mut out = HyperlinkLine::new(line);
    out.hyperlinks = web_links_in_text(&text);
    out
}

fn web_links_in_text(text: &str) -> Vec<TerminalHyperlink> {
    let mut links = Vec::new();
    let mut byte = 0usize;
    while byte < text.len() {
        let rest = &text[byte..];
        let Some(offset) = rest.find("http://").or_else(|| rest.find("https://")) else {
            break;
        };
        let start_byte = byte + offset;
        let url_tail = &text[start_byte..];
        let url_len = url_tail
            .find(|ch: char| ch.is_whitespace() || matches!(ch, ')' | ']' | '}' | '"' | '\''))
            .unwrap_or(url_tail.len());
        let end_byte = start_byte + url_len;
        if end_byte > start_byte {
            links.push(TerminalHyperlink {
                columns: byte_column(text, start_byte)..byte_column(text, end_byte),
                destination: text[start_byte..end_byte].to_string(),
            });
        }
        byte = end_byte;
    }
    links
}

fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn byte_column(text: &str, byte: usize) -> usize {
    text[..byte].chars().map(|ch| ch.width().unwrap_or(0)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotates_urls_without_changing_visible_text() {
        let line = Line::from("see https://example.com now");
        let linked = annotate_web_urls_in_line(line);

        assert_eq!(line_text(&linked.line), "see https://example.com now");
        assert_eq!(linked.hyperlinks.len(), 1);
        assert_eq!(linked.hyperlinks[0].destination, "https://example.com");
    }

    #[test]
    fn prefix_shifts_link_columns() {
        let linked = annotate_web_urls_in_line(Line::from("https://example.com"));
        let prefixed = prefix_hyperlink_lines(vec![linked], Span::raw("• "), Span::raw("  "));

        assert_eq!(line_text(&prefixed[0].line), "• https://example.com");
        assert_eq!(prefixed[0].hyperlinks[0].columns.start, 2);
    }
}
