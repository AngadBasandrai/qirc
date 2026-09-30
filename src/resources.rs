use std::fmt;

use crate::calibration::Calibration;
use crate::cost;
use crate::gridsynth;
use crate::ir::*;
use crate::rotations;
use crate::transpile::{self, GateSet};

const THRESHOLD: f64 = 0.01;
const PREFACTOR: f64 = 0.03;
const DEFAULT_ERROR: f64 = 1e-3;
const DEFAULT_CYCLE: f64 = 400.0;
const MAX_DISTANCE: usize = 101;
const FACTORY_TILES: usize = 11;
const FACTORY_STEPS: usize = 11;
const MAX_LEVELS: usize = 3;

pub struct Estimate {
    pub logical: usize,
    pub t: usize,
    pub rotations: usize,
    pub per_rotation: usize,
    pub states: usize,
    pub steps: usize,
    pub distance: usize,
    pub levels: usize,
    pub factories: usize,
    pub factory_tiles: usize,
    pub data_tiles: usize,
    pub data_qubits: usize,
    pub factory_qubits: usize,
    pub cycles: usize,
    pub runtime: f64,
    pub error: f64,
    pub cycle: f64,
    pub budget: f64,
    pub shares: [f64; 3],
    pub looped: bool,
    pub t_depth: Option<usize>,
}

pub(crate) fn logical_rate(error: f64, distance: usize) -> f64 {
    PREFACTOR * (error / THRESHOLD).powf((distance + 1) as f64 / 2.0)
}

pub(crate) fn lowered(program: &Program) -> Result<Program, String> {
    let mut lowered = program.clone();
    let set = GateSet::parse("h,s,sdg,t,tdg,x,y,z,cx,cz")?.with_rotations();
    transpile::transpile(&mut lowered, &set);
    for block in &mut lowered.blocks {
        let mut ops = Vec::with_capacity(block.ops.len());
        for op in block.ops.drain(..) {
            let named = match &op {
                Op::Gate(gate)
                    if gate.controls.is_empty()
                        && matches!(gate.kind, GateKind::Rz | GateKind::R1) =>
                {
                    gate.constant_angle()
                        .and_then(gridsynth::exact)
                        .map(|kinds| (kinds, gate.targets.clone(), gate.span))
                }
                _ => None,
            };
            match named {
                Some((kinds, targets, span)) => ops.extend(kinds.into_iter().map(|kind| {
                    Op::Gate(Gate {
                        kind,
                        controls: Vec::new(),
                        targets: targets.clone(),
                        params: Vec::new(),
                        span,
                    })
                })),
                None => ops.push(op),
            }
        }
        block.ops = ops;
    }
    Ok(lowered)
}

fn format_count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_time(ns: f64) -> String {
    let units = [
        (86_400e9, "days"),
        (3_600e9, "h"),
        (60e9, "min"),
        (1e9, "s"),
        (1e6, "ms"),
        (1e3, "us"),
    ];
    units.iter().find(|(scale, _)| ns >= *scale).map_or_else(
        || format!("{ns:.0} ns"),
        |(scale, unit)| format!("{:.3} {unit}", ns / scale),
    )
}

pub fn estimate(
    program: &Program,
    calibration: Option<&Calibration>,
    budget: f64,
) -> Result<Estimate, String> {
    let (error, cycle) = calibration.map_or((None, None), Calibration::surface);
    let error = error.unwrap_or(DEFAULT_ERROR);
    let cycle = cycle.unwrap_or(DEFAULT_CYCLE);
    if error >= THRESHOLD {
        return Err(format!(
            "a physical error rate of {error} is above the surface code threshold of {THRESHOLD}"
        ));
    }
    let lowered = lowered(program)?;
    let report = cost::analyse(&lowered, None);
    let t_depth = rotations::build(&lowered)
        .ok()
        .map(|rotations| rotations.t_depth);
    let looped = report.paths.iter().any(|path| path.again.is_some());
    let worst = report.worst;
    let logical = worst.live.max(1);
    let (t, rotations) = (worst.t, worst.rotations);
    let shares = if rotations > 0 {
        [budget / 3.0; 3]
    } else {
        [budget / 2.0, budget / 2.0, 0.0]
    };
    let per_rotation = if rotations > 0 {
        (3.0 * (rotations as f64 / shares[2]).log2()).ceil() as usize
    } else {
        0
    };
    let states = t + rotations * per_rotation;
    let steps = states.max(1);

    let mut levels = 0;
    let mut factory_tiles = 0;
    if states > 0 {
        let mut output = error;
        while levels < MAX_LEVELS && (levels == 0 || states as f64 * output > shares[1]) {
            output = 35.0 * output.powi(3);
            factory_tiles = 15 * factory_tiles + FACTORY_TILES;
            levels += 1;
        }
        if states as f64 * output > shares[1] {
            return Err(format!(
                "{MAX_LEVELS} levels of 15-to-1 distillation cannot make {states} magic states within the budget of {budget}"
            ));
        }
    }
    let factories = states.min(FACTORY_STEPS);
    let data_tiles = 2 * logical + (8.0 * logical as f64).sqrt().ceil() as usize + 1;
    let tiles = data_tiles + factories * factory_tiles;
    let latency = if states > 0 {
        FACTORY_STEPS * levels
    } else {
        0
    };
    let distance = (3..=MAX_DISTANCE)
        .step_by(2)
        .find(|&d| {
            tiles as f64 * ((steps + latency) * d) as f64 * logical_rate(error, d) <= shares[0]
        })
        .ok_or_else(|| format!("no code distance up to {MAX_DISTANCE} keeps logical errors within the budget of {budget}"))?;
    let per_tile = 2 * distance * distance;
    let cycles = (steps + latency) * distance;
    Ok(Estimate {
        logical,
        t,
        rotations,
        per_rotation,
        states,
        steps,
        distance,
        levels,
        factories,
        factory_tiles,
        data_tiles,
        data_qubits: data_tiles * per_tile,
        factory_qubits: factories * factory_tiles * per_tile,
        cycles,
        runtime: cycles as f64 * cycle,
        error,
        cycle,
        budget,
        shares,
        looped,
        t_depth,
    })
}

impl fmt::Display for Estimate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let row = |f: &mut fmt::Formatter<'_>, name: &str, value: String| {
            writeln!(f, "{name:<18}{value}")
        };
        row(f, "logical qubits", format_count(self.logical))?;
        row(f, "T gates", format_count(self.t))?;
        if let Some(depth) = self.t_depth.filter(|_| self.t > 0) {
            row(
                f,
                "T depth",
                format!(
                    "{}, as layers of commuting Pauli product rotations",
                    format_count(depth)
                ),
            )?;
        }
        if self.rotations > 0 {
            row(
                f,
                "rotations",
                format!(
                    "{}, each {} T gates within {:.1e}",
                    format_count(self.rotations),
                    self.per_rotation,
                    self.shares[2] / self.rotations as f64
                ),
            )?;
        }
        row(f, "magic states", format_count(self.states))?;
        row(f, "logical steps", format_count(self.steps))?;
        row(f, "code distance", self.distance.to_string())?;
        if self.states > 0 {
            row(
                f,
                "distillation",
                format!(
                    "{} level{} of 15-to-1, {} factor{} of {} tiles",
                    self.levels,
                    if self.levels == 1 { "" } else { "s" },
                    self.factories,
                    if self.factories == 1 { "y" } else { "ies" },
                    format_count(self.factory_tiles)
                ),
            )?;
        } else {
            row(f, "distillation", "none, the program is Clifford".into())?;
        }
        row(
            f,
            "physical qubits",
            format!(
                "{} ({} for data, {} for factories)",
                format_count(self.data_qubits + self.factory_qubits),
                format_count(self.data_qubits),
                format_count(self.factory_qubits)
            ),
        )?;
        row(
            f,
            "runtime",
            format!(
                "{}, {} cycles of {} ns",
                format_time(self.runtime),
                format_count(self.cycles),
                self.cycle
            ),
        )?;
        let [logical, magic, synthesis] = self.shares;
        row(
            f,
            "error budget",
            if self.rotations > 0 {
                format!(
                    "{}: {logical:.1e} logical, {magic:.1e} magic states, {synthesis:.1e} synthesis",
                    self.budget
                )
            } else {
                format!(
                    "{}: {logical:.1e} logical, {magic:.1e} magic states",
                    self.budget
                )
            },
        )?;
        if self.looped {
            row(
                f,
                "loops",
                "counted once per path, so a loop that repeats needs more than this".into(),
            )?;
        }
        row(
            f,
            "assumptions",
            format!(
                "physical error {} per operation, surface code with 0.03 (p/0.01)^((d+1)/2) per cycle",
                self.error
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qasm;

    fn program(body: &str) -> Program {
        let (program, errors) = qasm::lower(&format!(
            "OPENQASM 2.0;\ninclude \"qelib1.inc\";\nqreg q[4];\n{body}"
        ));
        assert!(errors.is_empty(), "{errors:?}");
        program
    }

    #[test]
    fn estimates() {
        let clifford = estimate(&program("h q[0];\ncx q[0], q[1];\n"), None, 1e-3).unwrap();
        assert_eq!(
            (clifford.states, clifford.levels, clifford.factories),
            (0, 0, 0)
        );
        let toffoli = estimate(&program("ccx q[0], q[1], q[2];\nt q[3];\n"), None, 1e-3).unwrap();
        assert_eq!((toffoli.t, toffoli.rotations, toffoli.states), (8, 0, 8));
        assert_eq!(toffoli.t_depth, Some(1));
        assert_eq!(toffoli.levels, 1);
        let rotated = estimate(&program("rz(0.3) q[0];\nrx(0.2) q[1];\n"), None, 1e-3).unwrap();
        assert_eq!(rotated.rotations, 2);
        assert_eq!(rotated.states, 2 * rotated.per_rotation);
        let strict = estimate(&program("ccx q[0], q[1], q[2];\n"), None, 1e-9).unwrap();
        assert!(
            strict.distance > toffoli.distance,
            "{} {}",
            strict.distance,
            toffoli.distance
        );
        assert!(!toffoli.looped);
        let (repeating, errors) = qasm::lower(
            "OPENQASM 3.0;\ninclude \"stdgates.inc\";\nqubit[1] q;\nbit b;\nh q[0];\nb = measure q[0];\nwhile (b) {\n  t q[0];\n  h q[0];\n  b = measure q[0];\n}\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let looped = estimate(&repeating, None, 1e-3).unwrap();
        assert!(looped.looped && looped.to_string().contains("counted once"));
        let noisy = Calibration::parse("cx 0 1 0.02\n").unwrap();
        assert!(estimate(&program("t q[0];\n"), Some(&noisy), 1e-3).is_err());
    }
}
