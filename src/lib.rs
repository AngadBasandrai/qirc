#![cfg_attr(docsrs, doc = include_str!("../README.md"))]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod ast;
pub mod calibration;
pub mod codegen;
pub mod cost;
mod cut;
pub mod diag;
pub mod draw;
#[deny(clippy::undocumented_unsafe_blocks)]
pub mod driver;
pub mod equiv;
pub mod explain;
pub mod gridsynth;
pub mod inline;
pub mod ir;
pub mod json;
mod kak;
pub mod lex;
pub mod lower;
pub mod lsp;
pub mod observable;
pub mod opt;
pub mod parse;
mod pauli;
mod phase;
pub mod progress;
pub mod provider;
pub mod pulse;
pub mod qasm;
pub mod qasm2;
pub mod qis;
mod qsd;
pub mod resources;
pub mod reuse;
pub mod rotations;
pub mod route;
pub mod sema;
pub mod simulator;
pub mod stim;
pub mod surface;
pub mod synth;
pub mod transpile;
pub mod verify;
mod zx;
