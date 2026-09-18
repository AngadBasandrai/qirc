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

    let Some(entry_name) = module.entry_point().map(|f| f.sig.name.clone()) else {
        return Inlined {
            module: working,
            diagnostics,
        };
    };

    let mut tag = 0usize;

    for round in 0..MAX_ROUNDS {
        let Some(index) = working
            .functions
            .iter()
            .position(|f| f.sig.name == entry_name)
        else {
            break;
        };

        let Some(site) = find_call_site(&working, &working.functions[index]) else {
            break;
        };

        let callee = working.functions[site.callee_index].clone();
        let caller = working.functions[index].clone();

        tag += 1;
        match expand(&caller, &callee, &site, tag) {
            Ok(expanded) => working.functions[index] = expanded,
            Err(diagnostic) => {
                diagnostics.push(diagnostic);
                break;
            }
        }

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

fn expand(
    caller: &Function,
    callee: &Function,
    site: &CallSite,
    tag: usize,
) -> Result<Function, Diagnostic> {
    let host = &caller.blocks[site.block];
    let InstKind::Call(call) = &host.instructions[site.instruction].kind else {
        return Err(Diagnostic::error("inlining lost its call site"));
    };

    let prefix = format!("{}.{tag}.", callee.sig.name);
    let continuation = format!("{prefix}continue");

    let mut substitution: HashMap<String, Value> = HashMap::new();
    for (param, argument) in callee.sig.params.iter().zip(&call.args) {
        if let Some(name) = &param.name {
            substitution.insert(name.clone(), argument.value.clone());
        }
    }

    let renamer = Renamer {
        prefix: prefix.clone(),
        substitution,
    };

    let mut blocks: Vec<BasicBlock> = Vec::new();

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

    let mut returns: Vec<(Value, String)> = Vec::new();

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

    let tail_block = &caller.blocks[site.block];
    let mut tail = BasicBlock {
        label: continuation,
        instructions: tail_block.instructions[site.instruction + 1..].to_vec(),
        terminator: tail_block.terminator.clone(),
        span: tail_block.span,
    };

    if let Some(result) = &host.instructions[site.instruction].result
        && !returns.is_empty()
    {
        let ty = call.ret_ty.clone();
        tail.instructions.insert(
            0,
            Instruction {
                result: Some(result.clone()),
                kind: InstKind::Phi {
                    ty,
                    incoming: returns.into_iter().collect(),
                },
                span: host.instructions[site.instruction].span,
            },
        );
    }

    blocks.push(tail);

    let host_label = host.label.clone();
    let continuation_label = format!("{prefix}continue");
    for block in &mut blocks {
        if block.label.starts_with(&prefix) && block.label != continuation_label {
            continue;
        }
        for inst in &mut block.instructions {
            if let InstKind::Phi { incoming, .. } = &mut inst.kind {
                for (_, label) in incoming.iter_mut() {
                    if *label == host_label {
                        *label = continuation_label.clone();
                    }
                }
            }
        }
    }

    Ok(Function {
        sig: caller.sig.clone(),
        blocks,
        span: caller.span,
    })
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
        Value::Local(format!("{}{name}", self.prefix))
    }

    fn label(&self, label: &str) -> String {
        format!("{}{label}", self.prefix)
    }

    fn define(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
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
            result: inst.result.as_deref().map(|name| self.define(name)),
            kind: self.kind(&inst.kind),
            span: inst.span,
        }
    }

    fn kind(&self, kind: &InstKind) -> InstKind {
        match kind {
            InstKind::Call(call) => InstKind::Call(Call {
                tail: call.tail,
                ret_ty: call.ret_ty.clone(),
                explicit_fn_ty: call.explicit_fn_ty.clone(),
                callee: call.callee.clone(),
                args: call
                    .args
                    .iter()
                    .map(|arg| Argument {
                        ty: arg.ty.clone(),
                        attrs: arg.attrs.clone(),
                        value: self.value(&arg.value),
                        span: arg.span,
                    })
                    .collect(),
                attr_groups: call.attr_groups.clone(),
                span: call.span,
            }),
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
                    .map(|(value, label)| (self.value(value), self.label(label)))
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
                target: self.label(target),
            },
            Terminator::CondBr {
                cond,
                if_true,
                if_false,
            } => Terminator::CondBr {
                cond: self.typed(cond),
                if_true: self.label(if_true),
                if_false: self.label(if_false),
            },
            Terminator::Switch {
                scrutinee,
                default,
                cases,
            } => Terminator::Switch {
                scrutinee: self.typed(scrutinee),
                default: self.label(default),
                cases: cases
                    .iter()
                    .map(|(value, label)| (self.typed(value), self.label(label)))
                    .collect(),
            },
            Terminator::Unreachable => Terminator::Unreachable,
        }
    }
}
