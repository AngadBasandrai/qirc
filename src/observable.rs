use std::fmt;

use crate::calibration::Calibration;
use crate::cut;
use crate::equiv;
use crate::ir::{Expr, Op, Program, QubitId, ResultId, Term, keep_marked};
use crate::pauli;
use crate::route::remap;
use crate::simulator::bits::{self, Bits};
use crate::simulator::exec::{self, ExecConfig, Snapshot};
use crate::simulator::matrix::Matrix2;
use crate::simulator::mps::Mps;
use crate::simulator::state::{self, State};
use crate::simulator::tableau::Tableau;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pauli {
    X,
    Y,
    Z,
}

#[derive(Debug)]
struct Product {
    weight: f64,
    paulis: Vec<(usize, Pauli)>,
}

#[derive(Debug)]
pub struct Observable {
    terms: Vec<Product>,
    qubits: usize,
}

impl Product {
    fn masks(&self, wire: &[usize]) -> Vec<(usize, bool, bool)> {
        self.paulis
            .iter()
            .map(|&(q, pauli)| (wire[q], pauli != Pauli::Z, pauli != Pauli::X))
            .collect()
    }

    fn value(&self, state: &State, wire: &[usize]) -> f64 {
        let (mut x, mut z) = (0usize, 0usize);
        for (w, flips, phases) in self.masks(wire) {
            if flips {
                x |= 1 << w;
            }
            if phases {
                z |= 1 << w;
            }
        }
        self.weight * state.pauli_element(state, x, z).re
    }

    fn propagated(&self, program: &Program, wire: &[usize]) -> Result<f64, String> {
        Ok(self.weight * pauli::propagate(program, &self.masks(wire))?)
    }

    fn classical_value(&self, bits: &Bits, wire: &[usize]) -> f64 {
        if self.paulis.iter().any(|&(_, pauli)| pauli != Pauli::Z) {
            return 0.0;
        }
        let ones = self
            .paulis
            .iter()
            .filter(|&&(q, _)| bits.get(wire[q]))
            .count();
        if ones % 2 == 1 {
            -self.weight
        } else {
            self.weight
        }
    }

    fn stabilizer_value(&self, program: &Program, wire: &[usize]) -> f64 {
        let tableau = exec::finish(program, Tableau::new(program.num_qubits as usize));
        self.tableau_value(&tableau, wire)
    }

    fn chain_value(&self, chain: &Mps) -> f64 {
        let paulis: Vec<(usize, Matrix2)> = self
            .paulis
            .iter()
            .map(|&(q, pauli)| {
                let m = match pauli {
                    Pauli::X => Matrix2::x(),
                    Pauli::Y => Matrix2::y(),
                    Pauli::Z => Matrix2::z(),
                };
                (q, m)
            })
            .collect();
        self.weight * chain.expectation(&paulis).re
    }

    fn tableau_value(&self, tableau: &Tableau, wire: &[usize]) -> f64 {
        if self.paulis.is_empty() {
            return self.weight;
        }
        let mut tableau = tableau.clone();
        let wires: Vec<usize> = self
            .paulis
            .iter()
            .map(|&(q, pauli)| {
                let w = wire[q];
                if pauli == Pauli::Y {
                    for _ in 0..3 {
                        tableau.s(w);
                    }
                }
                if pauli != Pauli::Z {
                    tableau.h(w);
                }
                w
            })
            .collect();
        let first = wires[0];
        for &w in &wires[1..] {
            tableau.cx(w, first);
        }
        if tableau.random(first) {
            0.0
        } else if tableau.measure_with(first, || false) {
            -self.weight
        } else {
            self.weight
        }
    }
}

impl Observable {
    pub fn parse(text: &str) -> Result<Observable, String> {
        let mut terms = Vec::new();
        let mut qubits = 0;
        for chunk in text.replace('-', "+-").split('+') {
            let chunk = chunk.trim();
            if chunk.is_empty() {
                continue;
            }
            let (negative, body) = match chunk.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, chunk),
            };
            let mut term = Product {
                weight: if negative { -1.0 } else { 1.0 },
                paulis: Vec::new(),
            };
            for word in body.replace('*', " ").split_whitespace() {
                if let Ok(value) = word.parse::<f64>() {
                    term.weight *= value;
                    continue;
                }
                let mut chars = word.chars().peekable();
                while let Some(letter) = chars.next() {
                    let mut digits = String::new();
                    while let Some(d) = chars.next_if(char::is_ascii_digit) {
                        digits.push(d);
                    }
                    let qubit: usize = digits.parse().map_err(|_| {
                        format!("`{word}` needs a qubit number after each Pauli letter")
                    })?;
                    if term.paulis.iter().any(|&(q, _)| q == qubit) {
                        return Err(format!("qubit {qubit} appears twice in one term"));
                    }
                    let pauli = match letter.to_ascii_uppercase() {
                        'X' => Pauli::X,
                        'Y' => Pauli::Y,
                        'Z' => Pauli::Z,
                        'I' => continue,
                        other => {
                            return Err(format!("`{other}` is not a Pauli, expected I, X, Y or Z"));
                        }
                    };
                    term.paulis.push((qubit, pauli));
                    qubits = qubits.max(qubit + 1);
                }
            }
            term.paulis.sort_by_key(|&(q, _)| q);
            terms.push(term);
        }
        if terms.is_empty() {
            return Err("the observable has no terms".into());
        }
        Ok(Observable { terms, qubits })
    }

    fn value(&self, state: &State) -> f64 {
        let identity: Vec<usize> = (0..self.qubits).collect();
        self.terms
            .iter()
            .map(|term| term.value(state, &identity))
            .sum()
    }

    fn snapshot(&self, snapshot: Snapshot) -> f64 {
        let identity: Vec<usize> = (0..self.qubits).collect();
        self.terms
            .iter()
            .map(|term| match snapshot {
                Snapshot::Vector(state) => term.value(state, &identity),
                Snapshot::Tableau(tableau) => term.tableau_value(tableau, &identity),
                Snapshot::Bits(bits) => term.classical_value(bits, &identity),
                Snapshot::Chain(chain) => term.chain_value(chain),
            })
            .sum()
    }
}

impl fmt::Display for Observable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, term) in self.terms.iter().enumerate() {
            let sign = if term.weight < 0.0 { "-" } else { "+" };
            if index > 0 {
                write!(f, " {sign} ")?;
            } else if term.weight < 0.0 {
                write!(f, "-")?;
            }
            let paulis: Vec<String> = term
                .paulis
                .iter()
                .map(|&(q, pauli)| format!("{pauli:?}{q}"))
                .collect();
            if term.weight.abs() != 1.0 || paulis.is_empty() {
                write!(f, "{}", term.weight.abs())?;
                if !paulis.is_empty() {
                    write!(f, " ")?;
                }
            }
            write!(f, "{}", paulis.join(" "))?;
        }
        Ok(())
    }
}

type Wide = ((Vec<bool>, Vec<usize>), Program, String, Vec<cut::Weighted>);

fn terminal(program: &Program) -> Program {
    let mut stripped = program.clone();
    for block in &mut stripped.blocks {
        if !matches!(block.term, Term::Ret(_) | Term::Unreachable) {
            continue;
        }
        let mut later: Vec<QubitId> = Vec::new();
        let mut read: Vec<ResultId> = Vec::new();
        let mut keep = vec![true; block.ops.len()];
        for (index, op) in block.ops.iter().enumerate().rev() {
            match op {
                Op::Measure { qubit, result, .. }
                    if !later.contains(qubit) && !read.contains(result) =>
                {
                    keep[index] = false;
                }
                Op::Assign {
                    expr: Expr::ReadResult(result),
                    ..
                } => read.push(*result),
                _ => later.extend(op.qubits()),
            }
        }
        keep_marked(&mut block.ops, &keep);
    }
    stripped
}

fn check(program: &Program, observable: &Observable) -> Result<(), String> {
    if observable.qubits > program.num_qubits as usize {
        return Err(format!(
            "the observable names qubit {} but the program has {}",
            observable.qubits - 1,
            program.num_qubits
        ));
    }
    Ok(())
}

fn fits(program: &Program) -> Result<(), String> {
    if program.num_qubits as usize > state::MAX_QUBITS {
        return Err(format!(
            "expectation values need a state vector, which supports at most {} qubits",
            state::MAX_QUBITS
        ));
    }
    Ok(())
}

fn cone(program: &Program, qubits: &[usize]) -> (Program, Vec<usize>, Vec<bool>) {
    let ops = &program.blocks[0].ops;
    let mut inside = vec![false; program.num_qubits as usize];
    for &q in qubits {
        inside[q] = true;
    }
    let mut keep = vec![false; ops.len()];
    for (index, op) in ops.iter().enumerate().rev() {
        let wires = op.qubits();
        if wires.is_empty() {
            keep[index] = true;
        } else if wires.iter().any(|q| inside[q.index()]) {
            keep[index] = true;
            for q in wires {
                inside[q.index()] = true;
            }
        }
    }
    let mut wire = vec![0; inside.len()];
    let mut count = 0;
    for (q, &used) in inside.iter().enumerate() {
        if used {
            wire[q] = count;
            count += 1;
        }
    }
    let mut sub = program.clone();
    sub.blocks[0].ops = ops
        .iter()
        .zip(&keep)
        .filter(|(_, kept)| **kept)
        .map(|(op, _)| remap(op.clone(), |q| wire[q.index()]))
        .collect();
    sub.num_qubits = count as u32;
    (sub, wire, keep)
}

fn final_state(program: &Program) -> Result<State, String> {
    let config = ExecConfig {
        shots: 0,
        seed: 1,
        keep_state: true,
    };
    exec::execute_vector(program, config)
        .final_state
        .ok_or_else(|| "the program did not produce a state".into())
}

pub fn noisy_expectation(
    program: &Program,
    observable: &Observable,
    calibration: &Calibration,
    shots: u64,
    seed: u64,
) -> Result<f64, String> {
    check(program, observable)?;
    let config = ExecConfig {
        shots,
        seed,
        keep_state: false,
    };
    exec::average_noisy(&terminal(program), config, calibration, |snapshot| {
        observable.snapshot(snapshot)
    })
    .ok_or_else(|| "the program has gates the wide simulators cannot run".into())
}

pub fn extrapolate(
    program: &Program,
    observable: &Observable,
    calibration: &Calibration,
    shots: u64,
    seed: u64,
) -> Result<([f64; 3], f64), String> {
    let mut values = [0.0; 3];
    for (value, scale) in values.iter_mut().zip([1.0, 2.0, 3.0]) {
        *value = noisy_expectation(program, observable, &calibration.scaled(scale), shots, seed)?;
    }
    Ok((values, 3.0 * values[0] - 3.0 * values[1] + values[2]))
}

pub fn expectation(program: &Program, observable: &Observable) -> Result<f64, String> {
    check(program, observable)?;
    let stripped = terminal(program);
    let straight = stripped.blocks.len() == 1
        && !exec::needs_per_shot(&stripped)
        && !stripped.ops().any(|op| matches!(op, Op::Measure { .. }));
    if straight {
        let mut total = 0.0;
        let mut wide: Vec<Wide> = Vec::new();
        for term in &observable.terms {
            let qubits: Vec<usize> = term.paulis.iter().map(|&(q, _)| q).collect();
            if qubits.is_empty() {
                total += term.weight;
                continue;
            }
            let (sub, wire, kept) = cone(&stripped, &qubits);
            total += if sub.gates().all(bits::supports) {
                term.classical_value(
                    &exec::finish(&sub, Bits::new(sub.num_qubits as usize)),
                    &wire,
                )
            } else if exec::stabilizer(&sub) {
                term.stabilizer_value(&sub, &wire)
            } else if sub.num_qubits as usize > state::MAX_QUBITS {
                match term.propagated(&sub, &wire) {
                    Ok(value) => value,
                    Err(reason) => {
                        let masks = (term.weight, term.masks(&wire));
                        match wide
                            .iter_mut()
                            .find(|(key, ..)| key.0 == kept && key.1 == wire)
                        {
                            Some((.., terms)) => terms.push(masks),
                            None => wide.push(((kept, wire), sub, reason, vec![masks])),
                        }
                        0.0
                    }
                }
            } else {
                term.value(&final_state(&sub)?, &wire)
            };
        }
        for (_, sub, reason, terms) in &wide {
            total += cut::expectation(sub, terms).map_err(|cut| format!("{reason}, and {cut}"))?;
        }
        return Ok(total);
    }
    fits(program)?;
    let outcomes = equiv::explore(&stripped);
    if outcomes.unexplored > 1e-9 {
        return Err("the program has too many measurement branches to average exactly".into());
    }
    let mut total = 0.0;
    for branch in outcomes.branches.values() {
        let state = branch
            .vector()
            .ok_or("two measurement paths end in the same outcome, so their states cannot be averaged exactly")?;
        total += branch.probability() * observable.value(state);
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qasm;
    use crate::simulator::state::Rng;

    #[test]
    fn parses() {
        let o = Observable::parse("Z0 Z1 + 0.5*X2 - 2 Y3 - I0 + Z150").unwrap();
        assert_eq!(o.to_string(), "Z0 Z1 + 0.5 X2 - 2 Y3 - 1 + Z150");
        assert_eq!(o.qubits, 151);
        assert_eq!(Observable::parse("-Z0").unwrap().to_string(), "-Z0");
        for bad in ["", "Z", "Q0", "Z0 Z0"] {
            assert!(Observable::parse(bad).is_err(), "{bad}");
        }
    }

    fn random_terms(
        circuits: usize,
        gates: &[(&str, usize)],
        mut check: impl FnMut(&str, &Product, &Program, &State),
    ) {
        let mut rng = Rng::new(3);
        let mut pick = |n: usize| rng.next_u64() as usize % n;
        for _ in 0..circuits {
            let mut source = String::from(
                "OPENQASM 2.0;
include \"qelib1.inc\";
qreg q[5];
",
            );
            for _ in 0..25 {
                let (gate, arity) = gates[pick(gates.len())];
                let first = pick(5);
                let wires: Vec<String> = (0..arity)
                    .map(|k| format!("q[{}]", (first + k) % 5))
                    .collect();
                source += &format!(
                    "{gate} {};
",
                    wires.join(", ")
                );
            }
            let (program, diagnostics) = qasm::lower(&source);
            assert!(diagnostics.is_empty());
            let state = final_state(&program).unwrap();
            for _ in 0..10 {
                let mut text = String::new();
                for q in 0..5 {
                    text += ["I", "X", "Y", "Z"][pick(4)];
                    text += &format!("{q} ");
                }
                let term = &Observable::parse(&text).unwrap().terms[0];
                if !term.paulis.is_empty() {
                    check(&source, term, &program, &state);
                }
            }
        }
    }

    #[test]
    fn stabilizer_terms() {
        let gates = [
            ("h", 1),
            ("s", 1),
            ("sdg", 1),
            ("x", 1),
            ("y", 1),
            ("z", 1),
            ("sx", 1),
            ("cx", 2),
            ("cz", 2),
            ("cy", 2),
            ("swap", 2),
        ];
        let identity: Vec<usize> = (0..5).collect();
        random_terms(100, &gates, |source, term, program, state| {
            let exact = term.value(state, &identity);
            let tableau = term.stabilizer_value(program, &identity);
            assert!((exact - tableau).abs() < 1e-9, "{source}");
        });
    }

    #[test]
    fn chain_terms() {
        let gates = [
            ("h", 1),
            ("t", 1),
            ("sx", 1),
            ("rx(0.3)", 1),
            ("u3(0.4, 0.2, 1.3)", 1),
            ("cx", 2),
            ("cz", 2),
            ("crz(0.5)", 2),
            ("swap", 2),
        ];
        let identity: Vec<usize> = (0..5).collect();
        random_terms(40, &gates, |source, term, program, state| {
            let exact = term.value(state, &identity);
            let chain = exec::finish(program, Mps::new(5, 64));
            assert!((exact - term.chain_value(&chain)).abs() < 1e-9, "{source}");
        });
    }

    #[test]
    fn propagation() {
        let gates = [
            ("h", 1),
            ("t", 1),
            ("sdg", 1),
            ("sx", 1),
            ("y", 1),
            ("rx(0.3)", 1),
            ("ry(1.1)", 1),
            ("u3(0.4, 0.2, 1.3)", 1),
            ("cx", 2),
            ("cz", 2),
            ("crz(0.5)", 2),
            ("swap", 2),
            ("ccx", 3),
        ];
        let identity: Vec<usize> = (0..5).collect();
        random_terms(30, &gates, |source, term, program, state| {
            let exact = term.value(state, &identity);
            let propagated = term.propagated(program, &identity).unwrap();
            assert!((exact - propagated).abs() < 1e-9, "{source}");
        });
    }
    #[test]
    fn cutting() {
        let mut rng = Rng::new(9);
        let mut forward = Vec::new();
        for layer in 0..3 {
            for q in 0..34 {
                forward.push(format!("ry({:.3}) q[{q}];", 3.0 * rng.next_unit()));
                forward.push(format!("rz({:.3}) q[{q}];", 3.0 * rng.next_unit()));
            }
            for q in (0..33).filter(|&q| q != 16) {
                forward.push(format!("cx q[{q}], q[{}];", q + 1));
            }
            if layer % 3 == 1 {
                forward.push("cx q[16], q[17];".into());
            }
        }
        let backward: Vec<String> = forward
            .iter()
            .rev()
            .map(|line| line.replace("ry(", "ry(-").replace("rz(", "rz(-"))
            .collect();
        let source = format!(
            "OPENQASM 2.0;
include \"qelib1.inc\";
qreg q[34];
{}
{}
",
            forward.join(
                "
"
            ),
            backward.join(
                "
"
            )
        );
        let (program, errors) = qasm::lower(&source);
        assert!(errors.is_empty(), "{errors:?}");
        let observable = Observable::parse("Z0 Z33 + X10 + Z16 Z17 + 0.5 Y3").unwrap();
        let value = expectation(&program, &observable).unwrap();
        assert!((value - 2.0).abs() < 1e-9, "{value}");
    }
}
