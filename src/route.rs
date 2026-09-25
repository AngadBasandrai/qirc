use std::collections::VecDeque;
use std::fmt;

use crate::ir::*;

#[derive(Clone)]
pub struct Coupling {
    pub qubits: usize,
    pub edges: Vec<(usize, usize)>,
}

impl Coupling {
    pub fn line(qubits: usize) -> Coupling {
        Coupling {
            qubits,
            edges: (0..qubits.saturating_sub(1)).map(|i| (i, i + 1)).collect(),
        }
    }

    pub fn ring(qubits: usize) -> Coupling {
        let mut coupling = Coupling::line(qubits);
        if qubits > 2 {
            coupling.edges.push((qubits - 1, 0));
        }
        coupling
    }

    pub fn grid(rows: usize, columns: usize) -> Coupling {
        let mut edges = Vec::new();
        for row in 0..rows {
            for column in 0..columns {
                let here = row * columns + column;
                if column + 1 < columns {
                    edges.push((here, here + 1));
                }
                if row + 1 < rows {
                    edges.push((here, here + columns));
                }
            }
        }
        Coupling {
            qubits: rows * columns,
            edges,
        }
    }

    pub fn full(qubits: usize) -> Coupling {
        let mut edges = Vec::new();
        for a in 0..qubits {
            for b in (a + 1)..qubits {
                edges.push((a, b));
            }
        }
        Coupling { qubits, edges }
    }

    pub fn parse(spec: &str) -> Option<Coupling> {
        if let Some(rest) = spec.strip_prefix("line:") {
            return Some(Coupling::line(rest.parse().ok()?));
        }
        if let Some(rest) = spec.strip_prefix("ring:") {
            return Some(Coupling::ring(rest.parse().ok()?));
        }
        if let Some(rest) = spec.strip_prefix("full:") {
            return Some(Coupling::full(rest.parse().ok()?));
        }
        if let Some(rest) = spec.strip_prefix("grid:") {
            let (rows, columns) = rest.split_once('x')?;
            return Some(Coupling::grid(rows.parse().ok()?, columns.parse().ok()?));
        }

        let mut edges = Vec::new();
        let mut highest = 0usize;
        for pair in spec.split(',') {
            let (a, b) = pair.trim().split_once('-')?;
            let a: usize = a.trim().parse().ok()?;
            let b: usize = b.trim().parse().ok()?;
            highest = highest.max(a).max(b);
            edges.push((a, b));
        }

        if edges.is_empty() {
            return None;
        }

        Some(Coupling {
            qubits: highest + 1,
            edges,
        })
    }

    pub fn connected(&self, a: usize, b: usize) -> bool {
        self.edges
            .iter()
            .any(|&(x, y)| (x == a && y == b) || (x == b && y == a))
    }

    pub fn neighbours(&self, of: usize) -> Vec<usize> {
        let mut out = Vec::new();
        for &(a, b) in &self.edges {
            if a == of {
                out.push(b);
            } else if b == of {
                out.push(a);
            }
        }
        out
    }

    pub fn shortest_path(&self, from: usize, to: usize) -> Option<Vec<usize>> {
        if from == to {
            return Some(vec![from]);
        }

        let mut came_from = vec![usize::MAX; self.qubits];
        let mut seen = vec![false; self.qubits];
        let mut queue = VecDeque::new();

        seen[from] = true;
        queue.push_back(from);

        while let Some(current) = queue.pop_front() {
            for next in self.neighbours(current) {
                if next >= self.qubits || seen[next] {
                    continue;
                }
                seen[next] = true;
                came_from[next] = current;

                if next == to {
                    let mut path = vec![to];
                    let mut walk = to;
                    while walk != from {
                        walk = came_from[walk];
                        path.push(walk);
                    }
                    path.reverse();
                    return Some(path);
                }

                queue.push_back(next);
            }
        }

        None
    }
}

#[derive(Debug)]
pub struct RouteStats {
    pub swaps_inserted: usize,
    pub final_layout: Vec<usize>,
}

impl fmt::Display for RouteStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "routing inserted {} swap(s), final layout {:?}",
            self.swaps_inserted, self.final_layout
        )
    }
}

pub fn route(program: &mut Program, coupling: &Coupling) -> Result<RouteStats, String> {
    if (program.num_qubits as usize) > coupling.qubits {
        return Err(format!(
            "the program needs {} qubits but the coupling map has {}",
            program.num_qubits, coupling.qubits
        ));
    }

    if !program.is_straight_line() {
        return Err("cannot route a program with control flow".into());
    }

    for gate in program.gates() {
        let wires = gate.wires().count();
        if wires > 2 {
            return Err(format!(
                "`{}{}` acts on {wires} qubits, the router only handles one and two qubit gates",
                "c".repeat(gate.controls.len()),
                gate.kind.name()
            ));
        }
    }

    let width = coupling.qubits;
    let mut physical: Vec<usize> = (0..width).collect();
    let mut logical_at: Vec<usize> = (0..width).collect();
    let mut swaps = 0usize;

    let block = &mut program.blocks[0];
    let mut rewritten = Vec::with_capacity(block.ops.len());

    for op in block.ops.drain(..) {
        match op {
            Op::Gate(mut gate) => {
                let wires: Vec<QubitId> = gate.wires().collect();

                if wires.len() == 2 {
                    let a = physical[wires[0].index()];
                    let b = physical[wires[1].index()];

                    if !coupling.connected(a, b) {
                        let Some(path) = coupling.shortest_path(a, b) else {
                            return Err(format!(
                                "the coupling map has no path between physical qubits {a} and {b}"
                            ));
                        };

                        for step in 0..path.len().saturating_sub(2) {
                            let (x, y) = (path[step], path[step + 1]);
                            rewritten.push(Op::Gate(Gate {
                                kind: GateKind::Swap,
                                controls: Vec::new(),
                                targets: vec![QubitId(x as u32), QubitId(y as u32)],
                                params: Vec::new(),
                                span: gate.span,
                            }));

                            logical_at.swap(x, y);
                            physical[logical_at[x]] = x;
                            physical[logical_at[y]] = y;
                            swaps += 1;
                        }
                    }
                }

                for q in gate.controls.iter_mut().chain(&mut gate.targets) {
                    *q = remap(&physical, *q);
                }

                rewritten.push(Op::Gate(gate));
            }

            Op::Measure {
                qubit,
                result,
                span,
            } => rewritten.push(Op::Measure {
                qubit: remap(&physical, qubit),
                result,
                span,
            }),

            Op::Reset { qubit, span } => rewritten.push(Op::Reset {
                qubit: remap(&physical, qubit),
                span,
            }),

            other => rewritten.push(other),
        }
    }

    block.ops = rewritten;
    program.num_qubits = coupling.qubits as u32;

    Ok(RouteStats {
        swaps_inserted: swaps,
        final_layout: physical,
    })
}

fn remap(physical: &[usize], q: QubitId) -> QubitId {
    QubitId(physical[q.index()] as u32)
}

pub fn respects(program: &Program, coupling: &Coupling) -> bool {
    program.gates().all(|gate| {
        let wires: Vec<QubitId> = gate.wires().collect();
        match wires.len() {
            0 | 1 => true,
            2 => coupling.connected(wires[0].index(), wires[1].index()),
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;

    fn line_program(pairs: &[(u32, u32)], qubits: u32) -> Program {
        let mut program = Program::new("r", Profile::Unrestricted);
        program.num_qubits = qubits;
        program.blocks.push(Block {
            id: BlockId(0),
            label: "entry".into(),
            ops: pairs
                .iter()
                .map(|(c, t)| {
                    Op::Gate(Gate {
                        kind: GateKind::X,
                        controls: vec![QubitId(*c)],
                        targets: vec![QubitId(*t)],
                        params: Vec::new(),
                        span: Span::DUMMY,
                    })
                })
                .collect(),
            term: Term::Ret(None),
            span: Span::DUMMY,
        });
        program
    }

    #[test]
    fn parse_specs() {
        assert_eq!(
            Coupling::parse("line:3").unwrap().edges,
            vec![(0, 1), (1, 2)]
        );
        assert_eq!(Coupling::parse("ring:3").unwrap().edges.len(), 3);
        assert_eq!(Coupling::parse("grid:2x2").unwrap().edges.len(), 4);
        assert_eq!(Coupling::parse("full:4").unwrap().edges.len(), 6);

        let explicit = Coupling::parse("0-1, 2-3").unwrap();
        assert_eq!(explicit.qubits, 4);
        assert!(explicit.connected(2, 3));
        assert!(!explicit.connected(1, 2));

        assert!(Coupling::parse("nonsense").is_none());
    }

    #[test]
    fn shortest_line_path() {
        let line = Coupling::line(5);
        assert_eq!(line.shortest_path(0, 4).unwrap(), vec![0, 1, 2, 3, 4]);
        assert_eq!(line.shortest_path(2, 2).unwrap(), vec![2]);
        assert_eq!(
            Coupling::parse("0-1").unwrap().shortest_path(0, 1),
            Some(vec![0, 1])
        );
    }

    #[test]
    fn adjacent() {
        let mut program = line_program(&[(0, 1), (1, 2)], 3);
        let stats = route(&mut program, &Coupling::line(3)).unwrap();
        assert_eq!(stats.swaps_inserted, 0);
        assert!(respects(&program, &Coupling::line(3)));
    }

    #[test]
    fn distant_pair() {
        let coupling = Coupling::line(5);
        let mut program = line_program(&[(0, 4)], 5);

        let stats = route(&mut program, &coupling).unwrap();
        assert_eq!(stats.swaps_inserted, 3);
        assert!(respects(&program, &coupling));
    }

    #[test]
    fn branching() {
        let mut program = line_program(&[(0, 1)], 2);
        program.blocks.push(Block {
            id: BlockId(1),
            label: "second".into(),
            ops: Vec::new(),
            term: Term::Ret(None),
            span: Span::DUMMY,
        });
        program.blocks[0].term = Term::Br(BlockId(1));

        assert!(
            route(&mut program, &Coupling::line(2))
                .unwrap_err()
                .contains("control flow")
        );
    }

    #[test]
    fn three_qubit_gate() {
        let mut program = Program::new("t", Profile::Unrestricted);
        program.num_qubits = 3;
        program.blocks.push(Block {
            id: BlockId(0),
            label: "entry".into(),
            ops: vec![Op::Gate(Gate {
                kind: GateKind::X,
                controls: vec![QubitId(0), QubitId(1)],
                targets: vec![QubitId(2)],
                params: Vec::new(),
                span: Span::DUMMY,
            })],
            term: Term::Ret(None),
            span: Span::DUMMY,
        });

        let message = route(&mut program, &Coupling::line(3)).unwrap_err();
        assert!(message.contains("acts on 3 qubits"), "{message}");
    }

    #[test]
    fn small_device() {
        let mut program = line_program(&[(0, 1)], 4);
        assert!(
            route(&mut program, &Coupling::line(2))
                .unwrap_err()
                .contains("needs 4 qubits")
        );
    }
}
