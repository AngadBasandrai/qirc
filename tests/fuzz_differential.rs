mod common;

use common::compile;
use qirc::ir::*;
use qirc::simulator::exec::{self, ExecConfig};
use qirc::simulator::matrix::{C64, Matrix2, matrix_for};
use qirc::simulator::state::Rng;

struct Gen(Rng);

impl Gen {
    fn new(seed: u64) -> Self {
        Gen(Rng::new(seed))
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.0.next_u64() % bound as u64) as usize
    }

    fn angle(&mut self) -> f64 {
        self.0.next_unit() * 6.0 - 3.0
    }

    fn two_distinct(&mut self, bound: usize) -> (usize, usize) {
        let a = self.below(bound);
        let mut b = self.below(bound);
        while b == a {
            b = self.below(bound);
        }
        (a, b)
    }
}

const ONE_QUBIT: &[&str] = &["h", "x", "y", "z", "s", "t"];
const ROTATIONS: &[&str] = &["rx", "ry", "rz"];

const DECLARATIONS: &str = "\
declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__y__body(%Qubit*)
declare void @__quantum__qis__z__body(%Qubit*)
declare void @__quantum__qis__s__body(%Qubit*)
declare void @__quantum__qis__t__body(%Qubit*)
declare void @__quantum__qis__rx__body(double, %Qubit*)
declare void @__quantum__qis__ry__body(double, %Qubit*)
declare void @__quantum__qis__rz__body(double, %Qubit*)
declare void @__quantum__qis__cx__body(%Qubit*, %Qubit*)
declare void @__quantum__qis__cz__body(%Qubit*, %Qubit*)
declare void @__quantum__qis__ccx__body(%Qubit*, %Qubit*, %Qubit*)
declare void @__quantum__qis__cry__body(double, %Qubit*, %Qubit*)
declare void @__quantum__qis__swap__body(%Qubit*, %Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare i1 @__quantum__qis__read_result__body(%Result*)
";

fn qubit(index: usize) -> String {
    format!("%Qubit* inttoptr (i64 {index} to %Qubit*)")
}

fn result(index: usize) -> String {
    format!("%Result* inttoptr (i64 {index} to %Result*)")
}

fn wrap(body: &str, qubits: usize, results: usize, profile: &str) -> String {
    format!(
        "%Qubit = type opaque\n%Result = type opaque\n\ndefine void @main() #0 {{\nentry:\n{body}  ret void\n}}\n\n{DECLARATIONS}\nattributes #0 = {{ \"entry_point\" \"qir_profiles\"=\"{profile}\" \"required_num_qubits\"=\"{qubits}\" \"required_num_results\"=\"{results}\" }}\n"
    )
}

fn random_unitary_circuit(rng: &mut Gen, qubits: usize, depth: usize) -> String {
    let mut body = String::new();

    for _ in 0..depth {
        match rng.below(7) {
            0 => {
                let name = ONE_QUBIT[rng.below(ONE_QUBIT.len())];
                body += &format!(
                    "  call void @__quantum__qis__{name}__body({})\n",
                    qubit(rng.below(qubits))
                );
            }
            1 => {
                let name = ROTATIONS[rng.below(ROTATIONS.len())];
                body += &format!(
                    "  call void @__quantum__qis__{name}__body(double {:.6e}, {})\n",
                    rng.angle(),
                    qubit(rng.below(qubits))
                );
            }
            2 | 3 if qubits >= 2 => {
                let name = if rng.below(2) == 0 { "cx" } else { "cz" };
                let (a, b) = rng.two_distinct(qubits);
                body += &format!(
                    "  call void @__quantum__qis__{name}__body({}, {})\n",
                    qubit(a),
                    qubit(b)
                );
            }
            4 if qubits >= 2 => {
                let (a, b) = rng.two_distinct(qubits);
                body += &format!(
                    "  call void @__quantum__qis__swap__body({}, {})\n",
                    qubit(a),
                    qubit(b)
                );
            }
            5 if qubits >= 3 => {
                let a = rng.below(qubits);
                let mut b = rng.below(qubits);
                let mut c = rng.below(qubits);
                while b == a {
                    b = rng.below(qubits);
                }
                while c == a || c == b {
                    c = rng.below(qubits);
                }
                body += &format!(
                    "  call void @__quantum__qis__ccx__body({}, {}, {})\n",
                    qubit(a),
                    qubit(b),
                    qubit(c)
                );
            }
            _ if qubits >= 2 => {
                let (a, b) = rng.two_distinct(qubits);
                body += &format!(
                    "  call void @__quantum__qis__cry__body(double {:.6e}, {}, {})\n",
                    rng.angle(),
                    qubit(a),
                    qubit(b)
                );
            }
            _ => {
                body += &format!("  call void @__quantum__qis__h__body({})\n", qubit(0));
            }
        }
    }

    wrap(&body, qubits, 0, "base_profile")
}

fn reference_state(program: &Program) -> Vec<C64> {
    let n = program.num_qubits as usize;
    let mut state = vec![C64::new(0.0, 0.0); 1usize << n];
    state[0] = C64::new(1.0, 0.0);

    for gate in program.gates() {
        let controls: Vec<usize> = gate.controls.iter().map(|q| q.index()).collect();
        let params: Vec<f64> = gate
            .params
            .iter()
            .filter_map(|p| p.constant().map(|c| c.as_f64()))
            .collect();

        if gate.kind == GateKind::Swap {
            let (a, b) = (gate.targets[0].index(), gate.targets[1].index());
            let mut next = state.clone();
            for (index, amplitude) in state.iter().enumerate() {
                if controls.iter().any(|c| (index >> c) & 1 == 0) {
                    continue;
                }
                if (index >> a) & 1 == (index >> b) & 1 {
                    continue;
                }
                next[index ^ (1 << a) ^ (1 << b)] = *amplitude;
            }
            state = next;
            continue;
        }

        let matrix: Matrix2 = matrix_for(gate.kind, &params);
        let target = gate.targets[0].index();
        let mut next = state.clone();

        for index in 0..state.len() {
            if (index >> target) & 1 == 1 {
                continue;
            }
            if controls.iter().any(|c| (index >> c) & 1 == 0) {
                continue;
            }
            let partner = index | (1 << target);
            let low = state[index];
            let high = state[partner];
            next[index] = matrix.a * low + matrix.b * high;
            next[partner] = matrix.c * low + matrix.d * high;
        }

        state = next;
    }

    state
}

fn simulated_state(program: &Program) -> Vec<C64> {
    let outcome = exec::execute(
        program,
        ExecConfig {
            shots: 0,
            seed: 1,
            keep_state: true,
        },
    );
    let state = outcome.final_state.expect("a final state");
    (0..state.len()).map(|i| state.amplitude(i)).collect()
}

fn assert_same_state(left: &[C64], right: &[C64], context: &str) {
    assert_eq!(left.len(), right.len(), "{context}: different widths");

    let Some(pivot) = (0..right.len()).find(|&i| right[i].norm() > 1e-9) else {
        panic!("{context}: the reference state vanished");
    };
    let phase = left[pivot] / right[pivot];

    for index in 0..left.len() {
        let difference = left[index] - phase * right[index];
        assert!(
            difference.norm() < 1e-9,
            "{context}: basis state {index} differs, {} vs {}",
            left[index],
            right[index]
        );
    }
}

#[test]
fn kernel_vs_reference() {
    let mut rng = Gen::new(11);

    for trial in 0..160 {
        let qubits = 2 + rng.below(4);
        let depth = 3 + rng.below(12);
        let source = random_unitary_circuit(&mut rng, qubits, depth);
        let program = compile(&source, 0);

        let reference = reference_state(&program);
        let simulated = simulated_state(&program);

        assert_eq!(reference.len(), simulated.len());
        for index in 0..reference.len() {
            let difference = reference[index] - simulated[index];
            assert!(
                difference.norm() < 1e-9,
                "trial {trial}: basis state {index} differs, reference {} vs kernel {}\n{source}",
                reference[index],
                simulated[index]
            );
        }
    }
}

#[test]
fn opt_levels_unitary() {
    let mut rng = Gen::new(23);

    for trial in 0..120 {
        let qubits = 2 + rng.below(4);
        let depth = 4 + rng.below(14);
        let source = random_unitary_circuit(&mut rng, qubits, depth);

        let baseline = simulated_state(&compile(&source, 0));

        for level in 1..=3u8 {
            let optimised = simulated_state(&compile(&source, level));
            assert_same_state(
                &baseline,
                &optimised,
                &format!("trial {trial} at -O{level}\n{source}"),
            );
        }
    }
}

fn random_measured_circuit(rng: &mut Gen, qubits: usize) -> String {
    let mut body = String::new();
    let mut results = 0usize;

    for _ in 0..(4 + rng.below(8)) {
        match rng.below(4) {
            0 if qubits >= 2 => {
                let (a, b) = rng.two_distinct(qubits);
                body += &format!(
                    "  call void @__quantum__qis__cx__body({}, {})\n",
                    qubit(a),
                    qubit(b)
                );
            }
            1 => {
                let target = rng.below(qubits);
                body += &format!(
                    "  call void @__quantum__qis__mz__body({}, {})\n",
                    qubit(target),
                    result(results)
                );
                results += 1;
            }
            _ => {
                let name = ONE_QUBIT[rng.below(ONE_QUBIT.len())];
                body += &format!(
                    "  call void @__quantum__qis__{name}__body({})\n",
                    qubit(rng.below(qubits))
                );
            }
        }
    }

    if results == 0 {
        body += &format!(
            "  call void @__quantum__qis__mz__body({}, {})\n",
            qubit(0),
            result(0)
        );
        results = 1;
    }

    wrap(&body, qubits, results, "adaptive_profile")
}

#[test]
fn opt_levels_measured() {
    let mut rng = Gen::new(37);

    for trial in 0..80 {
        let qubits = 2 + rng.below(3);
        let source = random_measured_circuit(&mut rng, qubits);

        let baseline = exec::execute(
            &compile(&source, 0),
            ExecConfig {
                shots: 600,
                seed: 4242,
                keep_state: false,
            },
        );

        for level in 1..=3u8 {
            let optimised = exec::execute(
                &compile(&source, level),
                ExecConfig {
                    shots: 600,
                    seed: 4242,
                    keep_state: false,
                },
            );

            assert_eq!(
                baseline.counts, optimised.counts,
                "trial {trial} at -O{level} changed the observed outcomes\n{source}"
            );
        }
    }
}

#[test]
fn shot_path_choice() {
    let mut rng = Gen::new(41);

    for _ in 0..60 {
        let qubits = 2 + rng.below(3);
        let source = random_measured_circuit(&mut rng, qubits);
        let program = compile(&source, 0);

        let mut measured = vec![false; program.num_qubits as usize];
        let mut expected = false;

        for op in program.ops() {
            match op {
                Op::Measure { qubit, .. } => measured[qubit.index()] = true,
                Op::Gate(gate) if gate.wires().any(|w| measured[w.index()]) => {
                    expected = true;
                }
                _ => {}
            }
        }

        assert_eq!(
            exec::needs_per_shot(&program),
            expected || !program.is_straight_line(),
            "the fast path decision was wrong for\n{source}"
        );
    }
}

fn random_slot_circuit(rng: &mut Gen) -> String {
    let first = rng.below(4);
    let second = rng.below(4);

    let body = format!(
        "  %slot = alloca i64
  store i64 {first}, ptr %slot
  call void @__quantum__qis__h__body({q0})
  call void @__quantum__qis__mz__body({q0}, {r0})
  %bit = call i1 @__quantum__qis__read_result__body({r0})
  %pick = select i1 %bit, i64 {second}, i64 {first}
  store i64 %pick, ptr %slot
  %back = load i64, ptr %slot
  %hot = icmp eq i64 %back, {second}
  br i1 %hot, label %flip, label %join
flip:
  call void @__quantum__qis__x__body({q1})
  br label %join
join:
  call void @__quantum__qis__mz__body({q1}, {r1})
",
        q0 = qubit(0),
        q1 = qubit(1),
        r0 = result(0),
        r1 = result(1)
    );

    wrap(&body, 2, 2, "adaptive_profile")
}

#[test]
fn opt_levels_slots() {
    let mut rng = Gen::new(53);

    for trial in 0..40 {
        let source = random_slot_circuit(&mut rng);

        let baseline = exec::execute(
            &compile(&source, 0),
            ExecConfig {
                shots: 400,
                seed: 909,
                keep_state: false,
            },
        );

        for level in 1..=3u8 {
            let optimised = exec::execute(
                &compile(&source, level),
                ExecConfig {
                    shots: 400,
                    seed: 909,
                    keep_state: false,
                },
            );

            assert_eq!(
                baseline.counts, optimised.counts,
                "trial {trial} at -O{level} changed a program that stores through memory\n{source}"
            );
        }
    }
}

#[test]
fn verifier_accepts() {
    let mut rng = Gen::new(67);

    for _ in 0..80 {
        let qubits = 2 + rng.below(4);
        let depth = 4 + rng.below(10);
        let source = random_unitary_circuit(&mut rng, qubits, depth);

        for level in 0..=3u8 {
            let program = compile(&source, level);
            let violations = qirc::verify::verify(&program);
            assert!(
                violations.is_empty(),
                "-O{level} produced invalid IR: {violations:?}\n{source}"
            );
        }
    }
}
