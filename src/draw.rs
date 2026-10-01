use std::f64::consts::PI;
use std::fmt::Write;

use crate::ir::*;

#[derive(Clone)]
enum Cell {
    Wire,
    Gate(String),
    Control(usize),
    Dot,
    Target,
    Swap(Option<usize>),
    Meter(u32),
    Reset,
    Pass,
}

struct Column {
    cells: Vec<Cell>,
    links: Vec<(usize, usize)>,
    label: Option<String>,
}

fn angle(value: f64, latex: bool) -> String {
    let turns = value / PI;
    let pi = if latex { "\\pi" } else { "π" };
    for d in [1i64, 2, 3, 4, 6, 8, 16] {
        let n = (turns * d as f64).round() as i64;
        if ((turns * d as f64) - n as f64).abs() < 1e-9 {
            if n == 0 {
                return "0".into();
            }
            let sign = if n < 0 { "-" } else { "" };
            let top = if n.abs() == 1 {
                pi.to_string()
            } else {
                format!("{}{pi}", n.abs())
            };
            return match (d, latex) {
                (1, _) => format!("{sign}{top}"),
                (_, true) => format!("{sign}\\frac{{{top}}}{{{d}}}"),
                (_, false) => format!("{sign}{top}/{d}"),
            };
        }
    }
    format!("{value:.3}")
}

fn label(gate: &Gate, latex: bool) -> String {
    let pick = |plain: String, tex: String| if latex { tex } else { plain };
    let name = match gate.kind {
        GateKind::Unitary(_) => "U".to_string(),
        GateKind::SDag => pick("S†".into(), "S^\\dagger".into()),
        GateKind::TDag => pick("T†".into(), "T^\\dagger".into()),
        GateKind::SXDag => pick("SX†".into(), "\\sqrt{X}^\\dagger".into()),
        GateKind::SX => pick("SX".into(), "\\sqrt{X}".into()),
        GateKind::Rx | GateKind::Ry | GateKind::Rz => {
            let axis = &gate.kind.name()[1..];
            pick(format!("R{axis}"), format!("R_{axis}"))
        }
        GateKind::R1 => "P".into(),
        kind => kind.name().to_uppercase(),
    };
    let params: Vec<String> = gate
        .params
        .iter()
        .map(|p| {
            p.constant()
                .map_or_else(|| "θ".to_string(), |c| angle(c.as_f64(), latex))
        })
        .collect();
    if params.is_empty() {
        name
    } else {
        format!("{name}({})", params.join(", "))
    }
}

fn columns(program: &Program, latex: bool) -> Vec<Column> {
    let n = program.num_qubits as usize;
    let mut all = Vec::new();
    for block in &program.blocks {
        let mut frontier = vec![0usize; n];
        let mut local: Vec<Column> = Vec::new();
        for op in &block.ops {
            let wires: Vec<usize> = op.qubits().iter().map(|q| q.index()).collect();
            let valid_shape = match op {
                Op::Gate(gate) => gate.has_valid_shape(),
                Op::Measure { result, .. } => result.0 < program.num_results,
                _ => true,
            };
            if !valid_shape || wires.is_empty() || wires.iter().any(|&q| q >= n) {
                continue;
            }
            let (Some(&lo), Some(&hi)) = (wires.iter().min(), wires.iter().max()) else {
                continue;
            };
            let at = (lo..=hi).map(|q| frontier[q]).max().unwrap_or(0);
            frontier[lo..=hi].fill(at + 1);
            while local.len() <= at {
                local.push(Column {
                    cells: vec![Cell::Wire; n],
                    links: Vec::new(),
                    label: None,
                });
            }
            let Column {
                cells: column,
                links,
                ..
            } = &mut local[at];
            if lo != hi {
                links.push((lo, hi));
            }
            if hi > lo + 1 {
                column[lo + 1..hi].fill(Cell::Pass);
            }
            match op {
                Op::Measure { qubit, result, .. } => column[qubit.index()] = Cell::Meter(result.0),
                Op::Reset { qubit, .. } => column[qubit.index()] = Cell::Reset,
                Op::Gate(gate) => {
                    let target = gate.targets[0].index();
                    for c in &gate.controls {
                        column[c.index()] = Cell::Control(target);
                    }
                    match (gate.kind, gate.controls.is_empty()) {
                        (GateKind::Swap, _) => {
                            column[target] = Cell::Swap(gate.targets.get(1).map(|q| q.index()));
                            if let Some(other) = gate.targets.get(1) {
                                column[other.index()] = Cell::Swap(None);
                            }
                        }
                        (GateKind::X, false) => column[target] = Cell::Target,
                        (GateKind::Z, false) => column[target] = Cell::Dot,
                        _ => {
                            for t in &gate.targets {
                                column[t.index()] = Cell::Gate(label(gate, latex));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(first) = local.first_mut()
            && program.blocks.len() > 1
        {
            first.label = Some(block.label.clone());
        }
        all.extend(local);
    }
    all
}

pub fn quantikz(program: &Program) -> String {
    let n = program.num_qubits as usize;
    let columns = columns(program, true);
    let mut rows: Vec<String> = (0..n).map(|q| format!("\\lstick{{$q_{{{q}}}$}}")).collect();
    for (index, column) in columns.iter().enumerate() {
        let next = columns.get(index + 1).and_then(|c| c.label.as_ref());
        for (q, cell) in column.cells.iter().enumerate() {
            let text = match cell {
                Cell::Wire | Cell::Pass => "\\qw".to_string(),
                Cell::Gate(text) => format!("\\gate{{{text}}}"),
                Cell::Control(target) => format!("\\ctrl{{{}}}", *target as i64 - q as i64),
                Cell::Dot => "\\control{}".into(),
                Cell::Target => "\\targ{}".into(),
                Cell::Swap(Some(other)) => format!("\\swap{{{}}}", *other as i64 - q as i64),
                Cell::Swap(None) => "\\targX{}".into(),
                Cell::Meter(_) => "\\meter{}".into(),
                Cell::Reset => "\\gate{\\ket{0}}".into(),
            };
            let slice = match (next, q) {
                (Some(label), 0) => format!(" \\slice{{{}}}", label.replace('_', "\\_")),
                _ => String::new(),
            };
            write!(rows[q], " & {text}{slice}").unwrap();
        }
    }
    let mut out = String::from("\\begin{quantikz}\n");
    let last = rows.len().saturating_sub(1);
    for (q, row) in rows.iter().enumerate() {
        out.push_str(row);
        out.push_str(if q == last {
            " & \\qw\n"
        } else {
            " & \\qw \\\\\n"
        });
    }
    out.push_str("\\end{quantikz}\n");
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn svg(program: &Program) -> String {
    const ROW: f64 = 40.0;
    const GAP: f64 = 14.0;
    const LEFT: f64 = 48.0;
    let n = program.num_qubits as usize;
    let columns = columns(program, false);
    let top = if program.blocks.len() > 1 { 34.0 } else { 16.0 };
    let y = |q: usize| top + q as f64 * ROW + ROW / 2.0;
    let text_width = |text: &str| 14.0 + 7.4 * text.chars().count() as f64;

    let mut body = String::new();
    let mut x = LEFT;
    for column in &columns {
        let width = column
            .cells
            .iter()
            .map(|cell| match cell {
                Cell::Gate(text) => text_width(text),
                Cell::Meter(r) => text_width(&format!("M r{r}")),
                _ => 28.0,
            })
            .fold(28.0, f64::max);
        let cx = x + width / 2.0;
        if let Some(label) = &column.label {
            writeln!(
                body,
                r#"<text x="{x}" y="14" class="block">{}</text>"#,
                escape(label)
            )
            .unwrap();
            writeln!(
                body,
                r#"<line x1="{}" x2="{}" y1="20" y2="{}" class="sep"/>"#,
                x - GAP / 2.0,
                x - GAP / 2.0,
                top + n as f64 * ROW
            )
            .unwrap();
        }
        for &(lo, hi) in &column.links {
            writeln!(
                body,
                r#"<line x1="{cx}" x2="{cx}" y1="{}" y2="{}" class="link"/>"#,
                y(lo),
                y(hi)
            )
            .unwrap();
        }
        for (q, cell) in column.cells.iter().enumerate() {
            let (cy, half) = (y(q), 12.0);
            match cell {
                Cell::Wire | Cell::Pass => {}
                Cell::Gate(_) | Cell::Meter(_) | Cell::Reset => {
                    let (text, class) = match cell {
                        Cell::Gate(text) => (text.clone(), "box"),
                        Cell::Meter(r) => (format!("M r{r}"), "meter"),
                        _ => ("|0⟩".to_string(), "meter"),
                    };
                    let w = text_width(&text).min(width);
                    writeln!(
                        body,
                        r#"<rect x="{}" y="{}" width="{w}" height="24" rx="4" class="{class}"/>"#,
                        cx - w / 2.0,
                        cy - half
                    )
                    .unwrap();
                    writeln!(
                        body,
                        r#"<text x="{cx}" y="{}" text-anchor="middle">{}</text>"#,
                        cy + 4.0,
                        escape(&text)
                    )
                    .unwrap();
                }
                Cell::Control(_) | Cell::Dot => {
                    writeln!(body, r#"<circle cx="{cx}" cy="{cy}" r="5" class="dot"/>"#).unwrap();
                }
                Cell::Target => {
                    writeln!(body, r#"<circle cx="{cx}" cy="{cy}" r="10" class="plus"/><path d="M{} {cy}H{}M{cx} {}V{}" class="link"/>"#, cx - 10.0, cx + 10.0, cy - 10.0, cy + 10.0).unwrap();
                }
                Cell::Swap(_) => {
                    writeln!(
                        body,
                        r#"<path d="M{} {}L{} {}M{} {}L{} {}" class="link"/>"#,
                        cx - 6.0,
                        cy - 6.0,
                        cx + 6.0,
                        cy + 6.0,
                        cx + 6.0,
                        cy - 6.0,
                        cx - 6.0,
                        cy + 6.0
                    )
                    .unwrap();
                }
            }
        }
        x += width + GAP;
    }
    let width = x + 12.0;
    let height = top + n as f64 * ROW + 8.0;
    let mut out = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
<style>
text {{ font: 12px ui-monospace, Menlo, Consolas, monospace; fill: #14202b; }}
.label {{ fill: #586775; }}
.block {{ fill: #586775; font-weight: 600; }}
.wire {{ stroke: #9aa8b5; stroke-width: 1; }}
.link {{ stroke: #0a6a92; stroke-width: 1.5; fill: none; }}
.box {{ fill: #ffffff; stroke: #0a6a92; stroke-width: 1.2; }}
.meter {{ fill: #f6f8fa; stroke: #586775; stroke-width: 1.2; }}
.dot {{ fill: #0a6a92; }}
.plus {{ fill: #ffffff; stroke: #0a6a92; stroke-width: 1.5; }}
.sep {{ stroke: #d3dbe2; stroke-dasharray: 4 4; }}
</style>
<rect width="100%" height="100%" fill="#ffffff"/>
"##
    );
    for q in 0..n {
        writeln!(
            out,
            r#"<line x1="{}" x2="{}" y1="{}" y2="{}" class="wire"/>"#,
            LEFT - 8.0,
            width - 8.0,
            y(q),
            y(q)
        )
        .unwrap();
        writeln!(
            out,
            r#"<text x="{}" y="{}" text-anchor="end" class="label">q{q}</text>"#,
            LEFT - 14.0,
            y(q) + 4.0
        )
        .unwrap();
    }
    out.push_str(&body);
    out.push_str("</svg>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;

    #[test]
    fn drawings_skip_malformed_operations() {
        let mut program = Program::new("malformed", Profile::Unrestricted);
        program.num_qubits = 1;
        program.num_results = 1;
        program.blocks.push(Block {
            id: BlockId(0),
            label: "entry".into(),
            ops: vec![
                Op::Gate(Gate {
                    kind: GateKind::X,
                    controls: vec![QubitId(2)],
                    targets: vec![QubitId(0)],
                    params: Vec::new(),
                    span: Span::DUMMY,
                }),
                Op::Gate(Gate {
                    kind: GateKind::X,
                    controls: vec![QubitId(0)],
                    targets: Vec::new(),
                    params: Vec::new(),
                    span: Span::DUMMY,
                }),
                Op::Gate(Gate {
                    kind: GateKind::X,
                    controls: vec![QubitId(0)],
                    targets: vec![QubitId(0)],
                    params: Vec::new(),
                    span: Span::DUMMY,
                }),
                Op::Measure {
                    qubit: QubitId(2),
                    result: ResultId(0),
                    span: Span::DUMMY,
                },
                Op::Measure {
                    qubit: QubitId(0),
                    result: ResultId(2),
                    span: Span::DUMMY,
                },
                Op::Reset {
                    qubit: QubitId(2),
                    span: Span::DUMMY,
                },
                Op::Gate(Gate {
                    kind: GateKind::H,
                    controls: Vec::new(),
                    targets: vec![QubitId(0)],
                    params: Vec::new(),
                    span: Span::DUMMY,
                }),
            ],
            term: Term::Ret(None),
            span: Span::DUMMY,
        });

        let latex = quantikz(&program);
        let image = svg(&program);
        assert!(latex.contains("\\gate{H}"), "{latex}");
        assert!(image.contains(">H</text>"), "{image}");
        assert!(!image.contains("M r2"), "{image}");
    }
}
