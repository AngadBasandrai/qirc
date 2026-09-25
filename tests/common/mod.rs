use qirc::diag::Severity;
use qirc::driver::{self, Compilation};
use qirc::ir::Program;
use qirc::simulator::exec::{self, ExecConfig, ExecOutcome};
use qirc::simulator::state::State;

pub fn compile(source: &str, level: u8) -> Program {
    let compilation = driver::compile(source, level);
    let errors = errors(&compilation);
    assert!(
        errors.is_empty(),
        "compilation failed: {errors:?}\n{source}"
    );
    compilation.program
}

pub fn errors(c: &Compilation) -> Vec<&str> {
    c.diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect()
}

pub fn run(program: &Program, shots: u64, seed: u64) -> ExecOutcome {
    exec::execute(
        program,
        ExecConfig {
            shots,
            seed,
            keep_state: false,
        },
    )
}

pub fn final_state(program: &Program) -> State {
    exec::execute(program, ExecConfig::default())
        .final_state
        .expect("a final state")
}
