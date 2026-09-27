use std::collections::HashMap;
use std::fmt;
use std::mem;

use crate::diag::Span;
use crate::ir::*;
use crate::route::remap;

#[derive(Debug)]
pub struct Reused {
    pub qubits_before: usize,
    pub qubits_after: usize,
    pub resets: usize,
}

impl fmt::Display for Reused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "qubit reuse fit {} qubits on {} wires with {} reset(s)",
            self.qubits_before, self.qubits_after, self.resets
        )
    }
}

fn quantum(op: &Op) -> bool {
    matches!(op, Op::Gate(_) | Op::Measure { .. } | Op::Reset { .. })
}

fn schedule(ops: Vec<Op>, qubits: usize) -> Vec<Op> {
    let mut waiting = vec![0usize; ops.len()];
    let mut next: Vec<Vec<usize>> = vec![Vec::new(); ops.len()];
    let mut last: HashMap<QubitId, usize> = HashMap::new();
    let mut remaining = vec![0usize; qubits];
    for (index, op) in ops.iter().enumerate() {
        for q in op.qubits() {
            remaining[q.index()] += 1;
            if let Some(before) = last.insert(q, index)
                && !next[before].contains(&index)
            {
                next[before].push(index);
                waiting[index] += 1;
            }
        }
    }

    let mut active = vec![false; qubits];
    let mut ready: Vec<usize> = (0..ops.len()).filter(|&i| waiting[i] == 0).collect();
    let mut order = Vec::with_capacity(ops.len());
    while !ready.is_empty() {
        let fresh = |i: usize| {
            ops[i]
                .qubits()
                .iter()
                .filter(|q| !active[q.index()])
                .count()
        };
        let pick = (0..ready.len())
            .min_by_key(|&k| (fresh(ready[k]), ready[k]))
            .unwrap_or(0);
        let id = ready.swap_remove(pick);
        for q in ops[id].qubits() {
            remaining[q.index()] -= 1;
            active[q.index()] = remaining[q.index()] > 0;
        }
        for &after in &next[id] {
            waiting[after] -= 1;
            if waiting[after] == 0 {
                ready.push(after);
            }
        }
        order.push(id);
    }

    let mut slots: Vec<Option<Op>> = ops.into_iter().map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

pub fn reuse(program: &mut Program) -> Option<Reused> {
    let qubits = program.num_qubits as usize;
    let [block] = &mut program.blocks[..] else {
        return None;
    };

    let mut ops = Vec::with_capacity(block.ops.len());
    let mut segment = Vec::new();
    for op in block.ops.clone() {
        if quantum(&op) {
            segment.push(op);
        } else {
            ops.extend(schedule(mem::take(&mut segment), qubits));
            ops.push(op);
        }
    }
    ops.extend(schedule(segment, qubits));

    let mut span = vec![(usize::MAX, 0usize); qubits];
    for (index, op) in ops.iter().enumerate() {
        for q in op.qubits() {
            let (first, last) = &mut span[q.index()];
            *first = (*first).min(index);
            *last = index;
        }
    }
    let mut used: Vec<usize> = (0..qubits).filter(|&q| span[q].0 != usize::MAX).collect();
    used.sort_by_key(|&q| span[q].0);

    let mut wires: Vec<usize> = Vec::new();
    let mut wire = vec![0usize; qubits];
    let mut reset_before: HashMap<usize, Vec<usize>> = HashMap::new();
    for &q in &used {
        let (first, last) = span[q];
        match wires.iter().position(|&free| free < first) {
            Some(w) => {
                wires[w] = last;
                wire[q] = w;
                reset_before.entry(first).or_default().push(w);
            }
            None => {
                wire[q] = wires.len();
                wires.push(last);
            }
        }
    }

    let resets: usize = reset_before.values().map(Vec::len).sum();
    if wires.len() >= used.len() {
        return None;
    }
    let mut out = Vec::with_capacity(ops.len() + resets);
    for (index, op) in ops.into_iter().enumerate() {
        for &w in reset_before.get(&index).into_iter().flatten() {
            out.push(Op::Reset {
                qubit: QubitId(w as u32),
                span: Span::DUMMY,
            });
        }
        out.push(remap(op, |q| wire[q.index()]));
    }
    block.ops = out;
    program.num_qubits = wires.len() as u32;
    if program.profile == Profile::Base {
        program.profile = Profile::Adaptive;
    }
    Some(Reused {
        qubits_before: qubits,
        qubits_after: wires.len(),
        resets,
    })
}
