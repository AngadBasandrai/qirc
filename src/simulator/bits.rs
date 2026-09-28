use super::state::Rng;
use crate::ir::{Gate, GateKind};
use crate::phase;

pub const MAX_QUBITS: usize = 1 << 24;

pub struct Bits(Vec<u64>);

pub fn supports(gate: &Gate) -> bool {
    matches!(gate.kind, GateKind::X | GateKind::Y | GateKind::Swap) || phase::diagonal(gate.kind)
}

impl Bits {
    pub fn new(qubits: usize) -> Bits {
        Bits(vec![0; qubits.div_ceil(64).max(1)])
    }

    pub fn get(&self, q: usize) -> bool {
        self.0[q / 64] >> (q % 64) & 1 == 1
    }

    pub fn flip(&mut self, q: usize) {
        self.0[q / 64] ^= 1 << (q % 64);
    }

    pub fn apply(&mut self, gate: &Gate) {
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
