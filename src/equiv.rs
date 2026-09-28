use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::ir::Program;
use crate::simulator::exec::{self, Backend};
use crate::simulator::matrix::C64;
use crate::simulator::state::State;
use crate::simulator::tableau::Tableau;

const PRUNE: f64 = 1e-12;
const MAX_RUNS: usize = 20_000;
const TOLERANCE: f64 = 1e-9;

enum Final {
    Vector(State),
    Stabilizer(Tableau),
}

trait Explore: Backend + Sized {
    fn fresh(qubits: usize) -> Self;
    fn chance(&mut self, qubit: usize) -> f64;
    fn settle(&mut self, qubit: usize, outcome: bool);
    fn wrap(self) -> Final;
}

impl Explore for State {
    fn fresh(qubits: usize) -> State {
        State::new(qubits)
    }

    fn chance(&mut self, qubit: usize) -> f64 {
        self.qubit_probability(qubit)
    }

    fn settle(&mut self, qubit: usize, outcome: bool) {
        self.collapse(qubit, outcome);
    }

    fn wrap(self) -> Final {
        Final::Vector(self)
    }
}

impl Explore for Tableau {
    fn fresh(qubits: usize) -> Tableau {
        Tableau::new(qubits)
    }

    fn chance(&mut self, qubit: usize) -> f64 {
        if self.random(qubit) {
            0.5
        } else if self.clone().measure_with(qubit, || false) {
            1.0
        } else {
            0.0
        }
    }

    fn settle(&mut self, qubit: usize, outcome: bool) {
        self.measure_with(qubit, || outcome);
    }

    fn wrap(self) -> Final {
        Final::Stabilizer(self)
    }
}

pub struct Branch {
    probability: f64,
    paths: usize,
    state: Final,
}

impl Branch {
    pub fn probability(&self) -> f64 {
        self.probability
    }

    pub fn vector(&self) -> Option<&State> {
        match &self.state {
            Final::Vector(state) if self.paths == 1 => Some(state),
            _ => None,
        }
    }
}

pub struct Outcomes {
    pub branches: BTreeMap<String, Branch>,
    pub unexplored: f64,
}

pub fn explore(program: &Program) -> Outcomes {
    if exec::stabilizer(program) {
        explore_with::<Tableau>(program)
    } else {
        explore_with::<State>(program)
    }
}

fn explore_with<S: Explore>(program: &Program) -> Outcomes {
    let mut branches: BTreeMap<String, Branch> = BTreeMap::new();
    let mut unexplored = 0.0;
    let mut pending = VecDeque::from([(Vec::new(), 1.0)]);
    let mut runs = 0;

    while let Some((script, weight)) = pending.pop_front() {
        if runs == MAX_RUNS {
            unexplored += weight;
            continue;
        }
        runs += 1;

        let mut state = S::fresh(program.num_qubits as usize);
        let mut fork = None;
        let mut used = 0;
        let run = exec::run_with(
            program,
            &mut state,
            &mut |state, qubit| {
                let one = state.chance(qubit);
                let Some(&outcome) = script.get(used) else {
                    fork = Some(one);
                    return None;
                };
                used += 1;
                state.settle(qubit, outcome);
                Some(outcome)
            },
            &mut |_, _, _| {},
        );

        if let Some(one) = fork {
            for (outcome, chance) in [(false, 1.0 - one), (true, one)] {
                let next = weight * chance;
                if next < PRUNE {
                    unexplored += next;
                    continue;
                }
                let mut longer = script.clone();
                longer.push(outcome);
                pending.push_back((longer, next));
            }
            continue;
        }

        if run.aborted {
            unexplored += weight;
            continue;
        }

        let mut key = exec::format_results(&run.results);
        if !run.returned.is_empty() {
            key = format!("{key} returning {}", exec::format_returned(&run.returned));
        }
        match branches.get_mut(&key) {
            Some(branch) => {
                branch.probability += weight;
                branch.paths += 1;
            }
            None => {
                branches.insert(
                    key,
                    Branch {
                        probability: weight,
                        paths: 1,
                        state: state.wrap(),
                    },
                );
            }
        }
    }

    Outcomes {
        branches,
        unexplored,
    }
}

pub fn compare(left: &Outcomes, right: &Outcomes, states: bool) -> Vec<String> {
    let keys: BTreeSet<&String> = left.branches.keys().chain(right.branches.keys()).collect();
    let mut differences = Vec::new();

    for key in keys {
        let (a, b) = (left.branches.get(key), right.branches.get(key));
        let chance = |branch: Option<&Branch>| branch.map_or(0.0, |b| b.probability);
        let (pa, pb) = (chance(a), chance(b));
        if (pa - pb).abs() > TOLERANCE {
            differences.push(format!("outcome {key}: probability {pa:.6} vs {pb:.6}"));
            continue;
        }
        if let (true, Some(a), Some(b)) = (states, a, b)
            && a.paths == 1
            && b.paths == 1
        {
            match (&a.state, &b.state) {
                (Final::Vector(x), Final::Vector(y)) => {
                    let overlap = overlap(x, y);
                    if (overlap - 1.0).abs() > TOLERANCE {
                        differences.push(format!(
                            "outcome {key}: final states differ, overlap {overlap:.6}"
                        ));
                    }
                }
                (Final::Stabilizer(x), Final::Stabilizer(y)) => {
                    if x.canonical() != y.canonical() {
                        differences.push(format!("outcome {key}: final stabilizer states differ"));
                    }
                }
                _ => differences.push(format!(
                    "outcome {key}: one program is Clifford and the other is not, so their final states cannot be compared"
                )),
            }
        }
    }

    differences
}

fn overlap(a: &State, b: &State) -> f64 {
    (0..a.len().min(b.len()))
        .map(|i| a.amplitude(i).conj() * b.amplitude(i))
        .sum::<C64>()
        .norm()
}
