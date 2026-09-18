use std::collections::HashMap;
use std::fmt;

use crate::ir::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Violation {
    pub message: String,
    pub block: Option<String>,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.block {
            Some(label) => write!(f, "[{label}] {}", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

pub fn verify(program: &Program) -> Vec<Violation> {
    let mut out = Vec::new();

    check_structure(program, &mut out);
    if !out.is_empty() {
        return out;
    }

    check_resources(program, &mut out);
    let definitions = check_ssa(program, &mut out);
    check_defs(program, &definitions, &mut out);
    check_phi_shape(program, &mut out);

    out
}

fn violation(program: &Program, block: BlockId, message: impl Into<String>) -> Violation {
    Violation {
        message: message.into(),
        block: program
            .blocks
            .get(block.0 as usize)
            .map(|b| b.label.clone()),
    }
}

fn check_structure(program: &Program, out: &mut Vec<Violation>) {
    if program.blocks.is_empty() {
        out.push(Violation {
            message: "program has no blocks".into(),
            block: None,
        });
        return;
    }

    if program.entry.0 as usize >= program.blocks.len() {
        out.push(Violation {
            message: format!("entry block {} does not exist", program.entry.0),
            block: None,
        });
    }

    for (index, block) in program.blocks.iter().enumerate() {
        if block.id.0 as usize != index {
            out.push(Violation {
                message: format!(
                    "block `{}` is stored at index {index} but carries id {}",
                    block.label, block.id.0
                ),
                block: Some(block.label.clone()),
            });
        }

        for successor in block.term.successors() {
            if successor.0 as usize >= program.blocks.len() {
                out.push(violation(
                    program,
                    block.id,
                    format!("branches to block {} which does not exist", successor.0),
                ));
            }
        }
    }
}

fn check_resources(program: &Program, out: &mut Vec<Violation>) {
    for block in &program.blocks {
        for op in &block.ops {
            match op {
                Op::Gate(gate) => {
                    for wire in gate.wires() {
                        if wire.0 >= program.num_qubits {
                            out.push(violation(
                                program,
                                block.id,
                                format!(
                                    "gate `{}` uses q{} but the register holds {}",
                                    gate.kind.name(),
                                    wire.0,
                                    program.num_qubits
                                ),
                            ));
                        }
                    }

                    let mut seen: Vec<QubitId> = Vec::new();
                    for wire in gate.wires() {
                        if seen.contains(&wire) {
                            out.push(violation(
                                program,
                                block.id,
                                format!("gate `{}` names q{} twice", gate.kind.name(), wire.0),
                            ));
                            break;
                        }
                        seen.push(wire);
                    }

                    if gate.params.len() != gate.kind.param_count() {
                        out.push(violation(
                            program,
                            block.id,
                            format!(
                                "gate `{}` carries {} parameters but takes {}",
                                gate.kind.name(),
                                gate.params.len(),
                                gate.kind.param_count()
                            ),
                        ));
                    }

                    if gate.targets.len() != gate.kind.arity() {
                        out.push(violation(
                            program,
                            block.id,
                            format!(
                                "gate `{}` has {} targets but takes {}",
                                gate.kind.name(),
                                gate.targets.len(),
                                gate.kind.arity()
                            ),
                        ));
                    }
                }

                Op::Measure { qubit, result, .. } => {
                    if qubit.0 >= program.num_qubits {
                        out.push(violation(
                            program,
                            block.id,
                            format!("measures q{} outside the register", qubit.0),
                        ));
                    }
                    if result.0 >= program.num_results {
                        out.push(violation(
                            program,
                            block.id,
                            format!("writes r{} outside the result register", result.0),
                        ));
                    }
                }

                Op::Reset { qubit, .. } => {
                    if qubit.0 >= program.num_qubits {
                        out.push(violation(
                            program,
                            block.id,
                            format!("resets q{} outside the register", qubit.0),
                        ));
                    }
                }

                Op::Store { slot, .. } => {
                    if slot.0 >= program.num_slots {
                        out.push(violation(
                            program,
                            block.id,
                            format!("stores to slot {} which was never allocated", slot.0),
                        ));
                    }
                }

                Op::Assign {
                    expr: Expr::Load(slot),
                    ..
                } => {
                    if slot.0 >= program.num_slots {
                        out.push(violation(
                            program,
                            block.id,
                            format!("loads slot {} which was never allocated", slot.0),
                        ));
                    }
                }

                Op::Assign {
                    expr: Expr::ReadResult(result),
                    ..
                } if result.0 >= program.num_results => {
                    out.push(violation(
                        program,
                        block.id,
                        format!("reads r{} outside the result register", result.0),
                    ));
                }

                _ => {}
            }
        }
    }
}

fn check_ssa(program: &Program, out: &mut Vec<Violation>) -> HashMap<ValueId, (BlockId, usize)> {
    let mut definitions: HashMap<ValueId, (BlockId, usize)> = HashMap::new();

    for block in &program.blocks {
        for (position, op) in block.ops.iter().enumerate() {
            if let Some(dest) = op.defined_value() {
                if definitions.contains_key(&dest) {
                    out.push(violation(
                        program,
                        block.id,
                        format!("%{} is assigned more than once", dest.0),
                    ));
                }
                definitions.insert(dest, (block.id, position));
            }
        }
    }

    definitions
}

fn operand_uses(op: &Op) -> Vec<Operand> {
    match op {
        Op::Gate(gate) => gate.params.clone(),
        Op::Store { value, .. } => vec![*value],
        Op::Assign { expr, .. } => match expr {
            Expr::Const(_) | Expr::ReadResult(_) | Expr::Load(_) => Vec::new(),
            Expr::Copy(o) | Expr::Cast { operand: o, .. } => vec![*o],
            Expr::Binary { lhs, rhs, .. }
            | Expr::ICmp { lhs, rhs, .. }
            | Expr::FCmp { lhs, rhs, .. } => vec![*lhs, *rhs],
            Expr::Select {
                cond,
                if_true,
                if_false,
            } => vec![*cond, *if_true, *if_false],
            Expr::Phi(_) => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn check_defs(
    program: &Program,
    definitions: &HashMap<ValueId, (BlockId, usize)>,
    out: &mut Vec<Violation>,
) {
    let idom = immediate_dominators(program);

    for block in &program.blocks {
        for (position, op) in block.ops.iter().enumerate() {
            for operand in operand_uses(op) {
                let Some(value) = operand.value() else {
                    continue;
                };
                let Some(&(def_block, def_position)) = definitions.get(&value) else {
                    report_unreachable(program, out, block.id, value, "an operand");
                    continue;
                };

                let reaches = if def_block == block.id {
                    def_position < position
                } else {
                    dominates(&idom, def_block, block.id)
                };

                if !reaches {
                    report_unreachable(program, out, block.id, value, "an operand");
                }
            }

            if let Op::Assign {
                expr: Expr::Phi(incoming),
                ..
            } = op
            {
                for (from, operand) in incoming {
                    let Some(value) = operand.value() else {
                        continue;
                    };
                    let Some(&(def_block, _)) = definitions.get(&value) else {
                        report_unreachable(program, out, block.id, value, "a phi operand");
                        continue;
                    };
                    if !dominates(&idom, def_block, *from) {
                        out.push(violation(
                            program,
                            block.id,
                            format!(
                                "phi takes %{} from block {} but the definition does not dominate that edge",
                                value.0, from.0
                            ),
                        ));
                    }
                }
            }
        }

        let terminator_operand = match &block.term {
            Term::CondBr { cond, .. } => Some(*cond),
            Term::Switch { scrutinee, .. } => Some(*scrutinee),
            Term::Ret(Some(operand)) => Some(*operand),
            _ => None,
        };

        if let Some(operand) = terminator_operand
            && let Some(value) = operand.value()
        {
            match definitions.get(&value) {
                None => report_unreachable(program, out, block.id, value, "the terminator"),
                Some(&(def_block, _)) => {
                    if def_block != block.id && !dominates(&idom, def_block, block.id) {
                        report_unreachable(program, out, block.id, value, "the terminator");
                    }
                }
            }
        }
    }
}

fn report_unreachable(
    program: &Program,
    out: &mut Vec<Violation>,
    block: BlockId,
    value: ValueId,
    what: &str,
) {
    out.push(violation(
        program,
        block,
        format!("{what} uses %{} which has no reaching definition", value.0),
    ));
}

fn check_phi_shape(program: &Program, out: &mut Vec<Violation>) {
    for block in &program.blocks {
        let predecessors = program.predecessors(block.id);

        for op in &block.ops {
            let Op::Assign {
                expr: Expr::Phi(incoming),
                ..
            } = op
            else {
                continue;
            };

            for (from, _) in incoming {
                if !predecessors.contains(from) {
                    out.push(violation(
                        program,
                        block.id,
                        format!("phi names block {} which is not a predecessor", from.0),
                    ));
                }
            }
        }
    }
}

pub fn reverse_postorder(program: &Program) -> Vec<BlockId> {
    let mut visited = vec![false; program.blocks.len()];
    let mut order = Vec::new();
    let mut stack = vec![(program.entry, 0usize)];

    if program.entry.0 as usize >= program.blocks.len() {
        return order;
    }
    visited[program.entry.0 as usize] = true;

    while let Some((block, index)) = stack.pop() {
        let successors = program.blocks[block.0 as usize].term.successors();

        if index < successors.len() {
            stack.push((block, index + 1));
            let next = successors[index];
            let slot = next.0 as usize;
            if slot < visited.len() && !visited[slot] {
                visited[slot] = true;
                stack.push((next, 0));
            }
        } else {
            order.push(block);
        }
    }

    order.reverse();
    order
}

pub fn immediate_dominators(program: &Program) -> Vec<Option<BlockId>> {
    let count = program.blocks.len();
    let mut idom: Vec<Option<BlockId>> = vec![None; count];

    if program.entry.0 as usize >= count {
        return idom;
    }

    let order = reverse_postorder(program);
    let mut position = vec![usize::MAX; count];
    for (index, block) in order.iter().enumerate() {
        position[block.0 as usize] = index;
    }

    idom[program.entry.0 as usize] = Some(program.entry);

    let mut changed = true;
    while changed {
        changed = false;

        for &block in &order {
            if block == program.entry {
                continue;
            }

            let mut new_idom: Option<BlockId> = None;
            for predecessor in program.predecessors(block) {
                if idom[predecessor.0 as usize].is_none() {
                    continue;
                }
                new_idom = Some(match new_idom {
                    None => predecessor,
                    Some(current) => intersect(&idom, &position, predecessor, current),
                });
            }

            if new_idom.is_some() && idom[block.0 as usize] != new_idom {
                idom[block.0 as usize] = new_idom;
                changed = true;
            }
        }
    }

    idom
}

fn intersect(
    idom: &[Option<BlockId>],
    position: &[usize],
    mut a: BlockId,
    mut b: BlockId,
) -> BlockId {
    let mut guard = 0;

    while a != b {
        guard += 1;
        if guard > idom.len() * 4 {
            return a;
        }

        while position[a.0 as usize] > position[b.0 as usize] {
            match idom[a.0 as usize] {
                Some(next) if next != a => a = next,
                _ => break,
            }
        }
        while position[b.0 as usize] > position[a.0 as usize] {
            match idom[b.0 as usize] {
                Some(next) if next != b => b = next,
                _ => break,
            }
        }

        if position[a.0 as usize] == position[b.0 as usize] && a != b {
            return a;
        }
    }

    a
}

pub fn dominates(idom: &[Option<BlockId>], ancestor: BlockId, block: BlockId) -> bool {
    if ancestor == block {
        return true;
    }

    let mut current = block;
    let mut guard = 0;

    while let Some(parent) = idom.get(current.0 as usize).copied().flatten() {
        guard += 1;
        if guard > idom.len() + 1 {
            return false;
        }
        if parent == ancestor {
            return true;
        }
        if parent == current {
            return false;
        }
        current = parent;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;

    fn program_with(ops: Vec<Op>, num_slots: u32) -> Program {
        let mut program = Program::new("t", Profile::Unrestricted);
        program.num_qubits = 2;
        program.num_results = 1;
        program.num_slots = num_slots;
        program.blocks.push(Block {
            id: BlockId(0),
            label: "entry".into(),
            ops,
            term: Term::Ret(None),
            span: Span::DUMMY,
        });
        program
    }

    fn gate_with_param(param: Operand) -> Op {
        Op::Gate(Gate {
            kind: GateKind::Rz,
            controls: Vec::new(),
            targets: vec![QubitId(0)],
            params: vec![param],
            span: Span::DUMMY,
        })
    }

    #[test]
    fn accepts_valid() {
        let program = program_with(
            vec![
                Op::Assign {
                    dest: ValueId(0),
                    expr: Expr::Const(Const::Float(0.5)),
                    span: Span::DUMMY,
                },
                gate_with_param(Operand::Value(ValueId(0))),
            ],
            0,
        );
        assert_eq!(verify(&program), Vec::new());
    }

    #[test]
    fn dangling_operand() {
        let program = program_with(vec![gate_with_param(Operand::Value(ValueId(7)))], 0);
        let found = verify(&program);
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("no reaching definition"));
    }

    #[test]
    fn dangling_store() {
        let program = program_with(
            vec![Op::Store {
                slot: SlotId(0),
                value: Operand::Value(ValueId(3)),
                span: Span::DUMMY,
            }],
            1,
        );
        let found = verify(&program);
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("%3"));
    }

    #[test]
    fn use_before_def() {
        let program = program_with(
            vec![
                gate_with_param(Operand::Value(ValueId(0))),
                Op::Assign {
                    dest: ValueId(0),
                    expr: Expr::Const(Const::Float(0.5)),
                    span: Span::DUMMY,
                },
            ],
            0,
        );
        assert!(!verify(&program).is_empty());
    }

    #[test]
    fn double_assign() {
        let program = program_with(
            vec![
                Op::Assign {
                    dest: ValueId(0),
                    expr: Expr::Const(Const::Int(1)),
                    span: Span::DUMMY,
                },
                Op::Assign {
                    dest: ValueId(0),
                    expr: Expr::Const(Const::Int(2)),
                    span: Span::DUMMY,
                },
            ],
            0,
        );
        assert!(
            verify(&program)
                .iter()
                .any(|v| v.message.contains("assigned more than once"))
        );
    }

    #[test]
    fn wire_out_of_range() {
        let program = program_with(
            vec![Op::Gate(Gate {
                kind: GateKind::H,
                controls: Vec::new(),
                targets: vec![QubitId(9)],
                params: Vec::new(),
                span: Span::DUMMY,
            })],
            0,
        );
        assert!(verify(&program).iter().any(|v| v.message.contains("q9")));
    }

    #[test]
    fn repeated_wire() {
        let program = program_with(
            vec![Op::Gate(Gate {
                kind: GateKind::X,
                controls: vec![QubitId(0)],
                targets: vec![QubitId(0)],
                params: Vec::new(),
                span: Span::DUMMY,
            })],
            0,
        );
        assert!(verify(&program).iter().any(|v| v.message.contains("twice")));
    }

    #[test]
    fn dangling_branch() {
        let mut program = program_with(Vec::new(), 0);
        program.blocks[0].term = Term::Br(BlockId(5));
        assert!(
            verify(&program)
                .iter()
                .any(|v| v.message.contains("does not exist"))
        );
    }

    #[test]
    fn diamond_dominance() {
        let mut program = Program::new("d", Profile::Unrestricted);
        program.num_qubits = 1;
        for (index, label) in ["entry", "left", "right", "join"].iter().enumerate() {
            program.blocks.push(Block {
                id: BlockId(index as u32),
                label: (*label).into(),
                ops: Vec::new(),
                term: Term::Ret(None),
                span: Span::DUMMY,
            });
        }
        program.blocks[0].term = Term::CondBr {
            cond: Operand::Const(Const::Bool(true)),
            if_true: BlockId(1),
            if_false: BlockId(2),
        };
        program.blocks[1].term = Term::Br(BlockId(3));
        program.blocks[2].term = Term::Br(BlockId(3));

        let idom = immediate_dominators(&program);
        assert!(dominates(&idom, BlockId(0), BlockId(3)));
        assert!(dominates(&idom, BlockId(0), BlockId(1)));
        assert!(!dominates(&idom, BlockId(1), BlockId(3)));
        assert!(!dominates(&idom, BlockId(2), BlockId(1)));
    }

    #[test]
    fn phi_non_predecessor() {
        let mut program = Program::new("p", Profile::Unrestricted);
        program.num_qubits = 1;
        for (index, label) in ["entry", "next"].iter().enumerate() {
            program.blocks.push(Block {
                id: BlockId(index as u32),
                label: (*label).into(),
                ops: Vec::new(),
                term: Term::Ret(None),
                span: Span::DUMMY,
            });
        }
        program.blocks[0].term = Term::Br(BlockId(1));
        program.blocks[1].ops.push(Op::Assign {
            dest: ValueId(0),
            expr: Expr::Phi(vec![(BlockId(1), Operand::Const(Const::Int(1)))]),
            span: Span::DUMMY,
        });

        assert!(
            verify(&program)
                .iter()
                .any(|v| v.message.contains("not a predecessor"))
        );
    }
}
