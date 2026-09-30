use std::fmt::Write as _;
use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
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
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

#[derive(Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

#[derive(Debug)]
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
        });
        self
    }

    pub fn note(mut self, message: impl Into<String>) -> Self {
        self.notes.push(message.into());
        self
    }

    pub fn primary_span(&self) -> Option<Span> {
        self.labels.first().map(|l| l.span)
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

            let mut underline = "^".repeat(width_chars);
            if !label.message.is_empty() {
                underline.push(' ');
                underline.push_str(&label.message);
            }
            writeln!(
                out,
                "{pad} {bar} {}{}",
                " ".repeat(prefix_chars),
                paint(accent, &underline)
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

fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut table = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in table.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in table[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let change = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (table[i - 1][j - 1] + change)
                .min(table[i - 1][j] + 1)
                .min(table[i][j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(table[i - 2][j - 2] + 1);
            }
            table[i][j] = best;
        }
    }
    table[a.len()][b.len()]
}

pub fn suggest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (name.chars().count() / 3).max(1);
    candidates
        .into_iter()
        .filter(|candidate| *candidate != name)
        .map(|candidate| (distance(name, candidate), candidate))
        .filter(|&(steps, _)| steps <= limit)
        .min_by_key(|&(steps, _)| steps)
        .map(|(_, candidate)| candidate)
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
    fn suggests() {
        assert_eq!(suggest("--gate", ["--gates", "--emit"]), Some("--gates"));
        assert_eq!(suggest("qams3", ["qasm3", "qasm2", "qir"]), Some("qasm3"));
        assert_eq!(suggest("toffoli", ["h", "cx"]), None);
        assert_eq!(suggest("h", ["h", "x"]), Some("x"));
    }
}
