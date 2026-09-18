use std::fmt::Write as _;
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const DUMMY: Span = Span { start: 0, end: 0 };

    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }

    pub fn at(offset: usize) -> Self {
        Self::new(offset, offset)
    }

    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }

    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start) as usize
    }

    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }
}

pub struct SourceFile {
    pub name: String,
    pub text: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        let text = text.into();

        let mut line_starts = Vec::with_capacity(text.len() / 24 + 1);
        line_starts.push(0u32);
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }

        Self {
            name: name.into(),
            text,
            line_starts,
        }
    }

    pub fn line_index(&self, offset: u32) -> usize {
        let offset = offset.min(self.text.len() as u32);
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    pub fn line_col(&self, offset: u32) -> (usize, usize) {
        let offset = (offset as usize).min(self.text.len());
        let line = self.line_index(offset as u32);
        let start = self.line_starts[line] as usize;
        let col = self.text[start..offset].chars().count();
        (line + 1, col + 1)
    }

    pub fn line_start(&self, line_index: usize) -> u32 {
        self.line_starts[line_index]
    }

    pub fn line_text(&self, line_index: usize) -> &str {
        let start = self.line_starts[line_index] as usize;
        let end = self
            .line_starts
            .get(line_index + 1)
            .map(|&e| e as usize)
            .unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    pub fn snippet(&self, span: Span) -> &str {
        let start = (span.start as usize).min(self.text.len());
        let end = (span.end as usize).min(self.text.len()).max(start);
        &self.text[start..end]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
    pub primary: bool,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<&'static str>,
    pub message: String,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn new(severity: Severity, message: impl Into<String>) -> Self {
        Self {
            severity,
            code: None,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(Severity::Error, message)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, message)
    }

    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    pub fn primary(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
            primary: true,
        });
        self
    }

    pub fn note(mut self, message: impl Into<String>) -> Self {
        self.notes.push(message.into());
        self
    }

    pub fn primary_span(&self) -> Option<Span> {
        self.labels
            .iter()
            .find(|l| l.primary)
            .or_else(|| self.labels.first())
            .map(|l| l.span)
    }

    pub fn render(&self, file: &SourceFile) -> String {
        self.render_styled(file, false)
    }

    pub fn render_styled(&self, file: &SourceFile, color: bool) -> String {
        let paint = |code: &str, text: &str| {
            if color {
                format!("\x1b[{code}m{text}\x1b[0m")
            } else {
                text.to_string()
            }
        };
        let accent = match self.severity {
            Severity::Error => "1;31",
            Severity::Warning => "1;33",
            Severity::Note => "1;36",
        };
        let blue = "1;34";

        let mut out = String::new();

        let head = match self.code {
            Some(code) => format!("{}[{}]", self.severity.label(), code),
            None => self.severity.label().to_string(),
        };
        writeln!(
            out,
            "{}{}",
            paint(accent, &head),
            paint("1", &format!(": {}", self.message))
        )
        .unwrap();

        let gutter = self
            .labels
            .iter()
            .map(|l| file.line_index(l.span.start) + 1)
            .max()
            .map(|line| line.to_string().len())
            .unwrap_or(1);
        let pad = " ".repeat(gutter);
        let bar = paint(blue, "|");

        if let Some(span) = self.primary_span() {
            let (line, col) = file.line_col(span.start);
            writeln!(
                out,
                "{pad}{} {}:{}:{}",
                paint(blue, "-->"),
                file.name,
                line,
                col
            )
            .unwrap();
        }

        let mut sorted: Vec<&Label> = self.labels.iter().collect();
        sorted.sort_by_key(|l| l.span.start);

        if !sorted.is_empty() {
            writeln!(out, "{pad} {bar}").unwrap();
        }

        for label in sorted {
            let line_index = file.line_index(label.span.start);
            let text = file.line_text(line_index);
            let line_start = file.line_start(line_index);

            let col_start = (label.span.start - line_start) as usize;
            let col_end = (label.span.end - line_start) as usize;
            let clamped_start = col_start.min(text.len());
            let clamped_end = col_end.min(text.len()).max(clamped_start);

            let prefix_chars = text[..clamped_start].chars().count();
            let width_chars = text[clamped_start..clamped_end].chars().count().max(1);

            let number = format!("{:>gutter$}", line_index + 1);
            writeln!(out, "{} {bar} {text}", paint(blue, &number)).unwrap();

            let (marker, marker_color) = if label.primary {
                ('^', accent)
            } else {
                ('-', blue)
            };
            let mut underline = marker.to_string().repeat(width_chars);
            if !label.message.is_empty() {
                underline.push(' ');
                underline.push_str(&label.message);
            }
            writeln!(
                out,
                "{pad} {bar} {}{}",
                " ".repeat(prefix_chars),
                paint(marker_color, &underline)
            )
            .unwrap();
        }

        if !self.notes.is_empty() {
            writeln!(out, "{pad} {bar}").unwrap();
            for note in &self.notes {
                writeln!(out, "{pad} {} {note}", paint(blue, "= note:")).unwrap();
            }
        }

        out
    }
}

#[derive(Default)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    pub fn extend(&mut self, other: Diagnostics) {
        self.items.extend(other.items);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.items
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.items
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.items.iter()
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str =
        "define void @main() {\nentry:\n  call void @__quantum__qis__foo(i64 0)\n  ret void\n}\n";

    fn file() -> SourceFile {
        SourceFile::new("test.ll", SRC)
    }

    #[test]
    fn positions() {
        let f = file();
        assert_eq!(f.line_col(0), (1, 1));
        assert_eq!(f.line_col(22), (2, 1));
        let idx = SRC.find("call").unwrap() as u32;
        assert_eq!(f.line_col(idx), (3, 3));
    }

    #[test]
    fn lines() {
        let f = file();
        assert_eq!(f.line_text(1), "entry:");
        assert_eq!(f.line_text(0), "define void @main() {");
    }

    #[test]
    fn snippets() {
        let f = file();
        let start = SRC.find("@__quantum__qis__foo").unwrap();
        let span = Span::new(start, start + "@__quantum__qis__foo".len());
        assert_eq!(f.snippet(span), "@__quantum__qis__foo");
    }

    #[test]
    fn render_caret() {
        let f = file();
        let start = SRC.find("@__quantum__qis__foo").unwrap();
        let span = Span::new(start, start + "@__quantum__qis__foo".len());

        let rendered = Diagnostic::error("unknown quantum intrinsic")
            .with_code("QIR0102")
            .primary(span, "not a known instruction")
            .render(&f);

        assert!(rendered.starts_with("error[QIR0102]: unknown quantum intrinsic\n"));
        assert!(rendered.contains("--> test.ll:3:13"));
        assert!(rendered.contains("call void @__quantum__qis__foo(i64 0)"));

        let caret_line = rendered
            .lines()
            .find(|l| l.contains('^'))
            .expect("a caret line");
        let caret_col = caret_line.find('^').unwrap();
        let code_line = rendered
            .lines()
            .find(|l| l.contains("call void"))
            .expect("the source line");
        let target_col = code_line.find("@__quantum").unwrap();
        assert_eq!(caret_col, target_col);
        assert_eq!(
            caret_line.matches('^').count(),
            "@__quantum__qis__foo".len()
        );
    }

    #[test]
    fn counts() {
        let mut bag = Diagnostics::new();
        assert!(!bag.has_errors());

        bag.push(Diagnostic::warning("unused qubit"));
        assert!(!bag.has_errors());
        assert_eq!(bag.warning_count(), 1);

        bag.push(Diagnostic::error("bad"));
        assert!(bag.has_errors());
        assert_eq!(bag.error_count(), 1);
        assert_eq!(bag.len(), 2);
    }
}
