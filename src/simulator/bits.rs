use super::state::Rng;
use crate::ir::{Gate, GateKind};
use crate::phase;

pub const MAX_QUBITS: usize = 1 << 24;

pub struct Bits {
    qubits: usize,
    words: Vec<u64>,
}

pub fn supports(gate: &Gate) -> bool {
    if !gate.has_valid_shape() {
        return false;
    }
    match (gate.kind, gate.targets.len()) {
        (GateKind::X | GateKind::Y, 1) | (GateKind::Swap, 2) => true,
        (kind, 1) if phase::diagonal(kind) => true,
        _ => false,
    }
}

impl Bits {
    pub fn new(qubits: usize) -> Bits {
        Bits {
            qubits,
            words: vec![0; qubits.div_ceil(64).max(1)],
        }
    }

    pub fn get(&self, q: usize) -> bool {
        assert!(q < self.qubits, "qubit is outside the bit simulator");
        self.words[q / 64] >> (q % 64) & 1 == 1
    }

    pub fn flip(&mut self, q: usize) {
        assert!(q < self.qubits, "qubit is outside the bit simulator");
        self.words[q / 64] ^= 1 << (q % 64);
    }

    pub fn apply(&mut self, gate: &Gate) {
        assert!(supports(gate), "gate is not supported by the bit simulator");
        let wires: Vec<usize> = gate.wires().map(|q| q.index()).collect();
        assert!(
            wires.iter().all(|&q| q < self.qubits),
            "gate qubit is outside the bit simulator"
        );
        for (i, &wire) in wires.iter().enumerate() {
            assert!(
                !wires[..i].contains(&wire),
                "a gate cannot use the same qubit more than once"
            );
        }
        if !gate.controls.iter().all(|c| self.get(c.index())) {
            return;
        }
        match (gate.kind, &gate.targets[..]) {
            (GateKind::Swap, [a, b]) if self.get(a.index()) != self.get(b.index()) => {
                self.flip(a.index());
                self.flip(b.index());
            }
            (GateKind::X | GateKind::Y, targets) => {
                for target in targets {
                    self.flip(target.index());
                }
            }
            _ => {}
        }
    }

    pub fn measure(&mut self, q: usize, _: &mut Rng) -> bool {
        self.get(q)
    }
}

#[cfg(test)]
mod tests {
    use super::Bits;
    use crate::diag::Span;
    use crate::ir::{Gate, GateKind, QubitId};

    fn gate(kind: GateKind, targets: Vec<QubitId>) -> Gate {
        Gate {
            kind,
            controls: Vec::new(),
            targets,
            params: Vec::new(),
            span: Span::DUMMY,
        }
    }

    #[test]
    #[should_panic(expected = "not supported")]
    fn rejects_unsupported_gate_shape() {
        Bits::new(1).apply(&gate(GateKind::H, vec![QubitId(0)]));
    }

    #[test]
    #[should_panic(expected = "outside")]
    fn rejects_out_of_bounds_qubit() {
        Bits::new(1).apply(&gate(GateKind::X, vec![QubitId(1)]));
    }
}
