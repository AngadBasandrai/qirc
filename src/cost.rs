use std::collections::{HashMap, HashSet};
use std::f64::consts::FRAC_PI_2;
use std::fmt;

use crate::calibration::Calibration;
use crate::ir::*;

const MAX_PATHS: usize = 256;

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Tally {
    pub t: usize,
    pub cx: usize,
    pub gates: usize,
    pub depth: usize,
    pub live: usize,
    pub rotations: usize,
    pub success: Option<f64>,
    pub time: Option<f64>,
}

impl Tally {
    fn of<'a>(blocks: impl Iterator<Item = &'a Block>, calibration: Option<&Calibration>) -> Tally {
        let blocks: Vec<&Block> = blocks.collect();
        let ops = || blocks.iter().flat_map(|b| &b.ops);
        let mut tally = Tally::default();
        if let Some(c) = calibration {
            let (time, idle) = c.schedule(ops());
            tally.success = Some((-c.infidelity(ops()) - idle).exp());
            tally.time = c.timed().then_some(time);
        }
        let mut frontier: HashMap<QubitId, usize> = HashMap::new();
        let mut open: HashMap<QubitId, usize> = HashMap::new();
        let mut intervals = Vec::new();
        for op in blocks.iter().flat_map(|b| &b.ops) {
            let qubits = op.qubits();
            let Some(layer) = qubits
                .iter()
                .map(|q| frontier.get(q).copied().unwrap_or(0) + 1)
                .max()
            else {
                continue;
            };
            for &q in &qubits {
                frontier.insert(q, layer);
                open.entry(q).or_insert(layer);
            }
            match op {
                Op::Gate(gate) => {
                    tally.gates += 1;
                    tally.cx += usize::from(qubits.len() > 1);
                    tally.t += usize::from(
                        gate.controls.is_empty()
                            && matches!(gate.kind, GateKind::T | GateKind::TDag),
                    );
                    tally.rotations += usize::from(
                        gate.controls.is_empty()
                            && matches!(
                                gate.kind,
                                GateKind::Rx | GateKind::Ry | GateKind::Rz | GateKind::R1
                            )
                            && gate.constant_angle().is_none_or(|angle| {
                                let quarters = angle / FRAC_PI_2;
                                (quarters - quarters.round()).abs() > 1e-9
                            }),
                    );
                }
                Op::Reset { qubit, .. } => {
                    intervals.extend(open.remove(qubit).map(|start| (start, layer)))
                }
                _ => {}
            }
        }
        intervals.extend(open.iter().map(|(q, &start)| (start, frontier[q])));
        tally.depth = frontier.into_values().max().unwrap_or(0);
        tally.live = peak(&intervals);
        tally
    }

    fn max(self, other: Tally) -> Tally {
        Tally {
            t: self.t.max(other.t),
            cx: self.cx.max(other.cx),
            gates: self.gates.max(other.gates),
            depth: self.depth.max(other.depth),
            live: self.live.max(other.live),
            rotations: self.rotations.max(other.rotations),
            success: match (self.success, other.success) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
            time: match (self.time, other.time) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            },
        }
    }
}

fn peak(intervals: &[(usize, usize)]) -> usize {
    let mut events: Vec<(usize, isize)> = intervals
        .iter()
        .flat_map(|&(start, end)| [(start, 1), (end + 1, -1)])
        .collect();
    events.sort_unstable();
    let mut live = 0isize;
    let mut most = 0;
    for (_, step) in events {
        live += step;
        most = most.max(live);
    }
    most as usize
}

pub struct Path {
    pub labels: Vec<String>,
    pub again: Option<String>,
    pub tally: Tally,
}

pub struct Report {
    pub paths: Vec<Path>,
    pub worst: Tally,
    pub truncated: bool,
}

struct Frame {
    block: BlockId,
    successors: Vec<BlockId>,
    next: usize,
}

fn frame(program: &Program, block: BlockId) -> Frame {
    let mut successors = program.block(block).term.successors();
    successors.dedup();
    Frame {
        block,
        successors,
        next: 0,
    }
}

fn path(
    program: &Program,
    frames: &[Frame],
    again: Option<BlockId>,
    calibration: Option<&Calibration>,
) -> Path {
    let label = |id: BlockId| program.block(id).label.clone();
    Path {
        labels: frames.iter().map(|f| label(f.block)).collect(),
        again: again.map(label),
        tally: Tally::of(frames.iter().map(|f| program.block(f.block)), calibration),
    }
}

pub fn analyse(program: &Program, calibration: Option<&Calibration>) -> Report {
    let mut paths = Vec::new();
    let mut truncated = false;
    let mut frames = vec![frame(program, program.entry)];
    let mut on_path = HashSet::from([program.entry]);
    if frames[0].successors.is_empty() {
        paths.push(path(program, &frames, None, calibration));
    }

    while let Some(top) = frames.last_mut() {
        let Some(&next) = top.successors.get(top.next) else {
            on_path.remove(&top.block);
            frames.pop();
            continue;
        };
        top.next += 1;
        let again = on_path.contains(&next);
        if !again {
            on_path.insert(next);
            frames.push(frame(program, next));
        }
        let ends = again || frames.last().is_some_and(|f| f.successors.is_empty());
        if !ends {
            continue;
        }
        if paths.len() == MAX_PATHS {
            truncated = true;
            break;
        }
        paths.push(path(program, &frames, again.then_some(next), calibration));
    }

    let worst = paths
        .iter()
        .fold(Tally::default(), |worst, p| worst.max(p.tally));
    Report {
        paths,
        worst,
        truncated,
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<String> = self
            .paths
            .iter()
            .map(|p| {
                let mut name = p.labels.join(" > ");
                if let Some(again) = &p.again {
                    name.push_str(&format!(" > {again} again"));
                }
                name
            })
            .collect();
        let width = names
            .iter()
            .map(String::len)
            .max()
            .unwrap_or(0)
            .max("worst listed".len());
        let row = |f: &mut fmt::Formatter<'_>, name: &str, t: &Tally| {
            write!(
                f,
                "{name:<width$}  {:>6}  {:>6}  {:>6}  {:>6}  {:>6}",
                t.t, t.cx, t.gates, t.depth, t.live
            )?;
            if let Some(time) = t.time {
                write!(f, "  {:>9.2}", time / 1000.0)?;
            }
            match t.success {
                Some(success) => writeln!(f, "  {success:>8.4}"),
                None => writeln!(f),
            }
        };
        write!(
            f,
            "{:<width$}  {:>6}  {:>6}  {:>6}  {:>6}  {:>6}",
            "path", "t", "cx", "gates", "depth", "live"
        )?;
        if self.worst.time.is_some() {
            write!(f, "  {:>9}", "time us")?;
        }
        if self.worst.success.is_some() {
            writeln!(f, "  {:>8}", "success")?;
        } else {
            writeln!(f)?;
        }
        for (name, p) in names.iter().zip(&self.paths) {
            row(f, name, &p.tally)?;
        }
        if self.paths.len() > 1 {
            let label = if self.truncated {
                "worst listed"
            } else {
                "worst case"
            };
            row(f, label, &self.worst)?;
        }
        if self.truncated {
            writeln!(f, "only the first {MAX_PATHS} paths are listed")?;
        }
        if self.paths.iter().any(|p| p.again.is_some()) {
            writeln!(
                f,
                "a path ending in `again` loops back, so its counts cover one pass through the loop"
            )?;
        }
        Ok(())
    }
}
