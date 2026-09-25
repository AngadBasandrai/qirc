use std::collections::HashMap;

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::qis;

const MAX_ROUNDS: usize = 64;

pub struct Inlined {
    pub module: Module,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn inline_module(module: &Module) -> Inlined {
    let mut working = module.clone();
    let mut diagnostics = Vec::new();

    let Some(index) = module.entry_point().and_then(|entry| {
        module
            .functions
            .iter()
            .position(|f| f.sig.name == entry.sig.name)
    }) else {
        return Inlined {
            module: working,
            diagnostics,
        };
    };

    for round in 0..MAX_ROUNDS {
        let Some(site) = find_call_site(&working, &working.functions[index]) else {
            break;
        };

        let expanded = expand(
            &working.functions[index],
            &working.functions[site.callee_index],
            &site,
            round + 1,
        );
        working.functions[index] = expanded;

        if round + 1 == MAX_ROUNDS {
            diagnostics.push(
                Diagnostic::warning("stopped inlining after the round limit")
                    .note("the program may call helper functions recursively"),
            );
        }
    }

    Inlined {
        module: working,
        diagnostics,
    }
}

struct CallSite {
    block: usize,
    instruction: usize,
    callee_index: usize,
}

fn find_call_site(module: &Module, caller: &Function) -> Option<CallSite> {
    for (block_index, block) in caller.blocks.iter().enumerate() {
        for (inst_index, inst) in block.instructions.iter().enumerate() {
            let InstKind::Call(call) = &inst.kind else {
                continue;
            };
            let Some(name) = call.callee_name() else {
                continue;
            };
            if qis::resolve(name).is_some() || name == caller.sig.name {
                continue;
            }
            let Some(callee_index) = module.functions.iter().position(|f| f.sig.name == name)
            else {
                continue;
            };
            if module.functions[callee_index].blocks.len() <= 1 {
                continue;
            }
            return Some(CallSite {
                block: block_index,
                instruction: inst_index,
                callee_index,
            });
        }
    }
    None
}

fn expand(caller: &Function, callee: &Function, site: &CallSite, tag: usize) -> Function {
    let host = &caller.blocks[site.block];
    let inst = &host.instructions[site.instruction];
    let InstKind::Call(call) = &inst.kind else {
        unreachable!()
    };

    let prefix = format!("{}.{tag}.", callee.sig.name);
    let continuation = format!("{prefix}continue");

    let substitution = callee
        .sig
        .params
        .iter()
        .zip(&call.args)
        .filter_map(|(p, a)| Some((p.name.clone()?, a.value.clone())))
        .collect();

    let renamer = Renamer {
        prefix: prefix.clone(),
        substitution,
    };

    let mut blocks = Vec::new();

    for (index, block) in caller.blocks.iter().enumerate() {
        if index != site.block {
            blocks.push(block.clone());
            continue;
        }

        let mut head = block.clone();
        head.instructions.truncate(site.instruction);
        head.terminator = Terminator::Br {
            target: format!("{prefix}{}", callee.blocks[0].label),
        };
        blocks.push(head);
    }

    let mut returns = Vec::new();

    for block in &callee.blocks {
        let mut cloned = BasicBlock {
            label: format!("{prefix}{}", block.label),
            instructions: block
                .instructions
                .iter()
                .map(|inst| renamer.instruction(inst))
                .collect(),
            terminator: renamer.terminator(&block.terminator),
            span: block.span,
        };

        if let Terminator::Ret(value) = &cloned.terminator {
            if let Some(typed) = value {
                returns.push((typed.value.clone(), cloned.label.clone()));
            }
            cloned.terminator = Terminator::Br {
                target: continuation.clone(),
            };
        }

        blocks.push(cloned);
    }

    let mut tail = BasicBlock {
        label: continuation.clone(),
        instructions: host.instructions[site.instruction + 1..].to_vec(),
        terminator: host.terminator.clone(),
        span: host.span,
    };

    if let Some(result) = &inst.result
        && !returns.is_empty()
    {
        tail.instructions.insert(
            0,
            Instruction {
                result: Some(result.clone()),
                kind: InstKind::Phi {
                    ty: call.ret_ty.clone(),
                    incoming: returns,
                },
                span: inst.span,
            },
        );
    }

    blocks.push(tail);

    for block in &mut blocks {
        if block.label.starts_with(&prefix) && block.label != continuation {
            continue;
        }
        for inst in &mut block.instructions {
            if let InstKind::Phi { incoming, .. } = &mut inst.kind {
                for (_, label) in incoming.iter_mut() {
                    if *label == host.label {
                        *label = continuation.clone();
                    }
                }
            }
        }
    }

    Function {
        sig: caller.sig.clone(),
        blocks,
        span: caller.span,
    }
}

struct Renamer {
    prefix: String,
    substitution: HashMap<String, Value>,
}

impl Renamer {
    fn local(&self, name: &str) -> Value {
        if let Some(value) = self.substitution.get(name) {
            return value.clone();
        }
        Value::Local(self.name(name))
    }

    fn name(&self, s: &str) -> String {
        format!("{}{s}", self.prefix)
    }

    fn value(&self, value: &Value) -> Value {
        match value {
            Value::Local(name) => self.local(name),
            Value::Aggregate(items) => {
                Value::Aggregate(items.iter().map(|tv| self.typed(tv)).collect())
            }
            Value::ConstExpr(expr) => Value::ConstExpr(Box::new(match expr.as_ref() {
                ConstExpr::Cast { op, operand, to } => ConstExpr::Cast {
                    op: *op,
                    operand: self.typed(operand),
                    to: to.clone(),
                },
                ConstExpr::GetElementPtr {
                    inbounds,
                    base_ty,
                    ptr,
                    indices,
                } => ConstExpr::GetElementPtr {
                    inbounds: *inbounds,
                    base_ty: base_ty.clone(),
                    ptr: self.typed(ptr),
                    indices: indices.iter().map(|tv| self.typed(tv)).collect(),
                },
                ConstExpr::Binary { op, lhs, rhs } => ConstExpr::Binary {
                    op: *op,
                    lhs: self.typed(lhs),
                    rhs: self.typed(rhs),
                },
            })),
            other => other.clone(),
        }
    }

    fn typed(&self, typed: &TypedValue) -> TypedValue {
        TypedValue {
            ty: typed.ty.clone(),
            value: self.value(&typed.value),
            span: typed.span,
        }
    }

    fn instruction(&self, inst: &Instruction) -> Instruction {
        Instruction {
            result: inst.result.as_deref().map(|name| self.name(name)),
            kind: self.kind(&inst.kind),
            span: inst.span,
        }
    }

    fn kind(&self, kind: &InstKind) -> InstKind {
        match kind {
            InstKind::Call(call) => {
                let mut call = call.clone();
                for arg in &mut call.args {
                    arg.value = self.value(&arg.value);
                }
                InstKind::Call(call)
            }
            InstKind::Binary { op, ty, lhs, rhs } => InstKind::Binary {
                op: *op,
                ty: ty.clone(),
                lhs: self.value(lhs),
                rhs: self.value(rhs),
            },
            InstKind::ICmp { pred, ty, lhs, rhs } => InstKind::ICmp {
                pred: *pred,
                ty: ty.clone(),
                lhs: self.value(lhs),
                rhs: self.value(rhs),
            },
            InstKind::FCmp { pred, ty, lhs, rhs } => InstKind::FCmp {
                pred: *pred,
                ty: ty.clone(),
                lhs: self.value(lhs),
                rhs: self.value(rhs),
            },
            InstKind::Cast { op, operand, to } => InstKind::Cast {
                op: *op,
                operand: self.typed(operand),
                to: to.clone(),
            },
            InstKind::Select {
                cond,
                if_true,
                if_false,
            } => InstKind::Select {
                cond: self.typed(cond),
                if_true: self.typed(if_true),
                if_false: self.typed(if_false),
            },
            InstKind::Phi { ty, incoming } => InstKind::Phi {
                ty: ty.clone(),
                incoming: incoming
                    .iter()
                    .map(|(value, label)| (self.value(value), self.name(label)))
                    .collect(),
            },
            InstKind::Alloca { ty, count } => InstKind::Alloca {
                ty: ty.clone(),
                count: count.as_ref().map(|tv| self.typed(tv)),
            },
            InstKind::Load { ty, ptr } => InstKind::Load {
                ty: ty.clone(),
                ptr: self.typed(ptr),
            },
            InstKind::Store { value, ptr } => InstKind::Store {
                value: self.typed(value),
                ptr: self.typed(ptr),
            },
            InstKind::GetElementPtr {
                inbounds,
                base_ty,
                ptr,
                indices,
            } => InstKind::GetElementPtr {
                inbounds: *inbounds,
                base_ty: base_ty.clone(),
                ptr: self.typed(ptr),
                indices: indices.iter().map(|tv| self.typed(tv)).collect(),
            },
            InstKind::ExtractValue { aggregate, indices } => InstKind::ExtractValue {
                aggregate: self.typed(aggregate),
                indices: indices.clone(),
            },
            InstKind::InsertValue {
                aggregate,
                value,
                indices,
            } => InstKind::InsertValue {
                aggregate: self.typed(aggregate),
                value: self.typed(value),
                indices: indices.clone(),
            },
            InstKind::Freeze(tv) => InstKind::Freeze(self.typed(tv)),
            InstKind::Fence => InstKind::Fence,
            InstKind::Unsupported { opcode } => InstKind::Unsupported {
                opcode: opcode.clone(),
            },
        }
    }

    fn terminator(&self, term: &Terminator) -> Terminator {
        match term {
            Terminator::Ret(value) => Terminator::Ret(value.as_ref().map(|tv| self.typed(tv))),
            Terminator::Br { target } => Terminator::Br {
                target: self.name(target),
            },
            Terminator::CondBr {
                cond,
                if_true,
                if_false,
            } => Terminator::CondBr {
                cond: self.typed(cond),
                if_true: self.name(if_true),
                if_false: self.name(if_false),
            },
            Terminator::Switch {
                scrutinee,
                default,
                cases,
            } => Terminator::Switch {
                scrutinee: self.typed(scrutinee),
                default: self.name(default),
                cases: cases
                    .iter()
                    .map(|(value, label)| (self.typed(value), self.name(label)))
                    .collect(),
            },
            Terminator::Unreachable => Terminator::Unreachable,
        }
    }
}
