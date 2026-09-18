use qirc::diag::Severity;
use qirc::driver;
use qirc::ir::Program;

pub fn compile(source: &str, level: u8) -> Program {
    let compilation = driver::compile(source, level);
    let errors: Vec<&str> = compilation
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect();
    assert!(
        errors.is_empty(),
        "compilation failed: {errors:?}\n{source}"
    );
    compilation.program
}
