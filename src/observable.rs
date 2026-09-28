use std::fmt;

use crate::equiv;
use crate::ir::{Expr, Op, Program, QubitId, ResultId, Term};
use crate::simulator::exec::{self, ExecConfig};
use crate::simulator::matrix::C64;
use crate::simulator::state::{self, State};

#[derive(Clone, Debug, PartialEq)]
pub struct Observable {
    terms: Vec<(f64, u64, u64, u32)>,
    qubits: usize,
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
            let mut term = (if negative { -1.0 } else { 1.0 }, 0u64, 0u64, 0u32);
            for word in body.replace('*', " ").split_whitespace() {
                if let Ok(value) = word.parse::<f64>() {
                    term.0 *= value;
                    continue;
                }
                let mut chars = word.chars().peekable();
                while let Some(letter) = chars.next() {
                    let mut digits = String::new();
                    while let Some(d) = chars.next_if(char::is_ascii_digit) {
                        digits.push(d);
                    }
                    let qubit: u32 = digits.parse().map_err(|_| {
                        format!("`{word}` needs a qubit number after each Pauli letter")
                    })?;
                    if qubit >= 64 {
                        return Err(format!(
                            "qubit {qubit} is past the 64 qubits an observable can name"
                        ));
                    }
                    let bit = 1u64 << qubit;
                    if (term.1 | term.2) & bit != 0 {
                        return Err(format!("qubit {qubit} appears twice in one term"));
                    }
                    match letter.to_ascii_uppercase() {
                        'X' => term.1 |= bit,
                        'Z' => term.2 |= bit,
                        'Y' => {
                            term.1 |= bit;
                            term.2 |= bit;
                            term.3 += 1;
                        }
                        'I' => {}
                        other => {
                            return Err(format!("`{other}` is not a Pauli, expected I, X, Y or Z"));
                        }
                    }
                    qubits = qubits.max(qubit as usize + 1);
                }
            }
            terms.push(term);
        }
        if terms.is_empty() {
            return Err("the observable has no terms".into());
        }
        Ok(Observable { terms, qubits })
    }

    fn value(&self, state: &State) -> f64 {
        let phases = [
            C64::new(1.0, 0.0),
            C64::new(0.0, 1.0),
            C64::new(-1.0, 0.0),
            C64::new(0.0, -1.0),
        ];
        self.terms
            .iter()
            .map(|&(weight, x, z, ys)| {
                let sum: C64 = (0..state.len())
                    .map(|j| {
                        let sign = if (j as u64 & z).count_ones() % 2 == 1 {
                            -1.0
                        } else {
                            1.0
                        };
                        state.amplitude(j ^ x as usize).conj() * state.amplitude(j) * sign
                    })
                    .sum();
                weight * (phases[ys as usize % 4] * sum).re
            })
            .sum()
    }
}

impl fmt::Display for Observable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, &(weight, x, z, _)) in self.terms.iter().enumerate() {
            let sign = if weight < 0.0 { "-" } else { "+" };
            if index > 0 {
                write!(f, " {sign} ")?;
            } else if weight < 0.0 {
                write!(f, "-")?;
            }
            let paulis: Vec<String> = (0..64)
                .filter(|q| (x | z) >> q & 1 == 1)
                .map(|q| {
                    let letter = match (x >> q & 1, z >> q & 1) {
                        (1, 1) => 'Y',
                        (1, _) => 'X',
                        _ => 'Z',
                    };
                    format!("{letter}{q}")
                })
                .collect();
            if weight.abs() != 1.0 || paulis.is_empty() {
                write!(f, "{}", weight.abs())?;
                if !paulis.is_empty() {
                    write!(f, " ")?;
                }
            }
            write!(f, "{}", paulis.join(" "))?;
        }
        Ok(())
    }
}

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
        let mut index = 0;
        block.ops.retain(|_| {
            index += 1;
            keep[index - 1]
        });
    }
    stripped
}

pub fn expectation(program: &Program, observable: &Observable) -> Result<f64, String> {
    let qubits = program.num_qubits as usize;
    if observable.qubits > qubits {
        return Err(format!(
            "the observable names qubit {} but the program has {qubits}",
            observable.qubits - 1
        ));
    }
    if qubits > state::MAX_QUBITS {
        return Err(format!(
            "expectation values need a state vector, which supports at most {} qubits",
            state::MAX_QUBITS
        ));
    }
    let stripped = terminal(program);
    if !exec::needs_per_shot(&stripped)
        && !stripped.ops().any(|op| matches!(op, Op::Measure { .. }))
    {
        let config = ExecConfig {
            shots: 0,
            seed: 1,
            keep_state: true,
        };
        let outcome = exec::execute_vector(&stripped, config);
        let state = outcome
            .final_state
            .ok_or("the program did not produce a state")?;
        return Ok(observable.value(&state));
    }
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

    #[test]
    fn parses() {
        let o = Observable::parse("Z0 Z1 + 0.5*X2 - 2 Y3 - I0").unwrap();
        assert_eq!(
            o.terms,
            [
                (1.0, 0, 3, 0),
                (0.5, 4, 0, 0),
                (-2.0, 8, 8, 1),
                (-1.0, 0, 0, 0)
            ]
        );
        assert_eq!(o.qubits, 4);
        assert_eq!(Observable::parse("-Z0").unwrap().terms, [(-1.0, 0, 1, 0)]);
        for bad in ["", "Z", "Q0", "Z0 Z0", "Z99"] {
            assert!(Observable::parse(bad).is_err(), "{bad}");
        }
    }
}
