//! Box-drawn tables for console output.

/// ANSI foreground color of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Green,
    Yellow,
    Red,
}

impl Color {
    fn code(self) -> &'static str {
        match self {
            Self::Green => "32",
            Self::Yellow => "33",
            Self::Red => "31",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub color: Option<Color>,
}

impl Cell {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: None,
        }
    }

    pub fn colored(text: impl Into<String>, color: Option<Color>) -> Self {
        Self {
            text: text.into(),
            color,
        }
    }

    /// Text padded to `width` characters first, then colored, so escape codes never skew alignment.
    fn render(&self, width: usize) -> String {
        let padded = format!("{:width$}", self.text);
        match self.color {
            Some(color) => format!("\x1b[{}m{padded}\x1b[0m", color.code()),
            None => padded,
        }
    }
}

/// Renders `headers` and `rows` as a box-drawn table without a trailing newline.
/// Every row must have one cell per header.
pub fn render_table(headers: &[&str], rows: &[Vec<Cell>]) -> String {
    let widths: Vec<usize> = (0..headers.len())
        .map(|i| {
            let cells = rows.iter().map(|row| row[i].text.chars().count());
            cells.chain([headers[i].chars().count()]).max().unwrap_or(0)
        })
        .collect();
    let rule = |left: &str, mid: &str, right: &str| {
        let bars: Vec<_> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        format!("{left}{}{right}", bars.join(mid))
    };
    let line = |cells: Vec<String>| format!("│ {} │", cells.join(" │ "));
    let mut out = vec![
        rule("┌", "┬", "┐"),
        line(
            headers
                .iter()
                .zip(&widths)
                .map(|(h, w)| Cell::plain(*h).render(*w))
                .collect(),
        ),
        rule("├", "┼", "┤"),
    ];
    out.extend(rows.iter().map(|row| {
        line(
            row.iter()
                .zip(&widths)
                .map(|(cell, w)| cell.render(*w))
                .collect(),
        )
    }));
    out.push(rule("└", "┴", "┘"));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(color: Option<Color>) -> Vec<Vec<Cell>> {
        vec![
            vec![Cell::plain("a"), Cell::colored("long cell", color)],
            vec![Cell::plain("bcd"), Cell::colored("é", color)],
        ]
    }

    #[test]
    fn renders_two_by_two_table() {
        let expected = "\
┌─────┬───────────┐
│ H1  │ H2        │
├─────┼───────────┤
│ a   │ long cell │
│ bcd │ é         │
└─────┴───────────┘";
        assert_eq!(render_table(&["H1", "H2"], &rows(None)), expected);
    }

    #[test]
    fn colored_cells_align_with_plain_ones() {
        let strip = |s: &str| s.replace("\x1b[32m", "").replace("\x1b[0m", "");
        let colored = render_table(&["H1", "H2"], &rows(Some(Color::Green)));
        assert!(colored.contains("\x1b[32mé        \x1b[0m"));
        assert_eq!(strip(&colored), render_table(&["H1", "H2"], &rows(None)));
    }
}
