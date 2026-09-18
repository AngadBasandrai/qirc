use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Instant;

use crate::codegen;
use crate::diag::{Diagnostic, Severity, SourceFile};
use crate::ir::Program;
use crate::lower;
use crate::opt::{self, OptStats};
use crate::parse::parse_module;
use crate::route::{self, Coupling, RouteStats};
use crate::sema;
use crate::simulator::exec::{self, ExecConfig};
use crate::simulator::simd;
use crate::simulator::state;
use crate::transpile::{self, Basis, TranspileStats};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Emit {
    Run,
    Ir,
    Qasm3,
    Qir,
    Json,
    Circuit,
    Check,
}

impl Emit {
    pub fn parse(text: &str) -> Option<Emit> {
        Some(match text {
            "run" => Emit::Run,
            "ir" => Emit::Ir,
            "qasm" | "qasm3" => Emit::Qasm3,
            "qir" | "llvm" => Emit::Qir,
            "json" => Emit::Json,
            "circuit" => Emit::Circuit,
            "check" => Emit::Check,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    Auto,
    Always,
    Never,
}

impl Color {
    fn enabled(self) -> bool {
        match self {
            Color::Always => {
                enable_ansi();
                true
            }
            Color::Never => false,
            Color::Auto => {
                std::env::var_os("NO_COLOR").is_none()
                    && io::stderr().is_terminal()
                    && enable_ansi()
            }
        }
    }
}

#[cfg(windows)]
fn enable_ansi() -> bool {
    use std::os::windows::io::AsRawHandle;

    unsafe extern "system" {
        fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
    }

    const VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    let handle = io::stderr().as_raw_handle();
    let mut mode = 0u32;

    unsafe {
        GetConsoleMode(handle, &mut mode) != 0
            && SetConsoleMode(handle, mode | VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
fn enable_ansi() -> bool {
    true
}

pub struct Options {
    pub input: PathBuf,
    pub emit: Emit,
    pub opt_level: u8,
    pub shots: u64,
    pub seed: Option<u64>,
    pub show_state: bool,
    pub verbose: bool,
    pub output: Option<PathBuf>,
    pub verify_each: bool,
    pub basis: Option<Basis>,
    pub coupling: Option<Coupling>,
    pub color: Color,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            emit: Emit::Run,
            opt_level: 1,
            shots: 0,
            seed: None,
            show_state: true,
            verbose: false,
            output: None,
            verify_each: false,
            basis: None,
            coupling: None,
            color: Color::Auto,
        }
    }
}

pub const USAGE: &str = "\
qirc: a QIR compiler and state vector simulator

usage:
  qirc <input.ll> [options]

options:
  --emit <kind>     run | ir | qasm3 | qir | json | circuit | check   (default: run)
  -O<n>             optimisation level 0 to 3                         (default: 1)
  --shots <n>       sample n measurement outcomes
  --seed <n>        seed the random number generator
  --no-state        do not print the final state vector
  -o <path>         write emitted output to a file
  --color <when>    auto | always | never                              (default: auto)
  --basis <name>    decompose into a target gate set: rz-sx-cx | rz-ry-cz
  --coupling <map>  route onto hardware: line:N | ring:N | grid:RxC | full:N
                    or an explicit edge list such as 0-1,1-2,2-3
  --verify-each     run the IR verifier after lowering and after every pass
  -v, --verbose     report pipeline statistics
  -h, --help        show this message
";

pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut input: Option<PathBuf> = None;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_str();

        match arg {
            "-h" | "--help" => return Err(String::new()),

            "--emit" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--emit needs a kind".to_string())?;
                options.emit =
                    Emit::parse(value).ok_or_else(|| format!("unknown emit kind `{value}`"))?;
                index += 2;
            }

            "--shots" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--shots needs a count".to_string())?;
                options.shots = value
                    .parse()
                    .map_err(|_| format!("invalid shot count `{value}`"))?;
                index += 2;
            }

            "--seed" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--seed needs a number".to_string())?;
                options.seed = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid seed `{value}`"))?,
                );
                index += 2;
            }

            "-o" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "-o needs a path".to_string())?;
                options.output = Some(PathBuf::from(value));
                index += 2;
            }

            "--basis" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--basis needs a name".to_string())?;
                options.basis =
                    Some(Basis::parse(value).ok_or_else(|| format!("unknown basis `{value}`"))?);
                index += 2;
            }

            "--coupling" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--coupling needs a map".to_string())?;
                options.coupling = Some(
                    Coupling::parse(value)
                        .ok_or_else(|| format!("unknown coupling map `{value}`"))?,
                );
                index += 2;
            }

            "--color" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--color needs auto, always or never".to_string())?;
                options.color = match value.as_str() {
                    "auto" => Color::Auto,
                    "always" => Color::Always,
                    "never" => Color::Never,
                    _ => return Err(format!("unknown color setting `{value}`")),
                };
                index += 2;
            }

            "--verify-each" => {
                options.verify_each = true;
                index += 1;
            }

            "--no-state" => {
                options.show_state = false;
                index += 1;
            }

            "-v" | "--verbose" => {
                options.verbose = true;
                index += 1;
            }

            _ if arg.starts_with("-O") => {
                let level = arg[2..]
                    .parse::<u8>()
                    .map_err(|_| format!("invalid optimisation level `{arg}`"))?;
                if level > 3 {
                    return Err(format!("optimisation level {level} is out of range"));
                }
                options.opt_level = level;
                index += 1;
            }

            _ if arg.starts_with('-') => return Err(format!("unknown option `{arg}`")),

            path => {
                if input.replace(PathBuf::from(path)).is_some() {
                    return Err("expected exactly one input file".into());
                }
                index += 1;
            }
        }
    }

    options.input = input.ok_or_else(|| "no input file given".to_string())?;
    Ok(options)
}

#[derive(Default)]
pub struct Target {
    pub basis: Option<Basis>,
    pub coupling: Option<Coupling>,
}

pub struct Compilation {
    pub program: Program,
    pub stats: OptStats,
    pub transpiled: Option<TranspileStats>,
    pub routed: Option<RouteStats>,
    pub diagnostics: Vec<Diagnostic>,
    pub parse_time: std::time::Duration,
    pub lower_time: std::time::Duration,
    pub opt_time: std::time::Duration,
}

pub fn compile(source: &str, opt_level: u8) -> Compilation {
    compile_verified(source, opt_level, false)
}

pub fn compile_verified(source: &str, opt_level: u8, verify_each: bool) -> Compilation {
    compile_for(source, opt_level, verify_each, &Target::default())
}

pub fn compile_for(source: &str, opt_level: u8, verify_each: bool, target: &Target) -> Compilation {
    let mut diagnostics = Vec::new();

    let started = Instant::now();
    let (module, parse_errors) = parse_module(source);
    let parse_time = started.elapsed();
    diagnostics.extend(parse_errors);

    let started = Instant::now();
    let lowered = lower::lower(&module);
    let lower_time = started.elapsed();
    diagnostics.extend(lowered.diagnostics);

    let mut program = lowered.program;
    diagnostics.extend(sema::validate(&program));

    let mut lowering_violations = Vec::new();
    let lowered_cleanly = !diagnostics.iter().any(|d| d.severity == Severity::Error);
    if verify_each && lowered_cleanly {
        for found in crate::verify::verify(&program) {
            lowering_violations.push(format!("after lowering: {found}"));
        }
    }

    let started = Instant::now();
    let mut stats = opt::optimise_verified(&mut program, opt_level, verify_each && lowered_cleanly);
    let opt_time = started.elapsed();

    lowering_violations.append(&mut stats.violations);
    stats.violations = lowering_violations;

    let transpiled = target
        .basis
        .map(|basis| transpile::transpile(&mut program, basis));

    let mut routed = None;
    if let Some(coupling) = &target.coupling {
        match route::route(&mut program, coupling) {
            Ok(stats) => routed = Some(stats),
            Err(message) => diagnostics.push(
                Diagnostic::error(format!("cannot route this program: {message}"))
                    .with_code("QIR0400"),
            ),
        }
    }

    if verify_each && (transpiled.is_some() || routed.is_some()) {
        for found in crate::verify::verify(&program) {
            stats.violations.push(format!("after targeting: {found}"));
        }
    }

    Compilation {
        program,
        stats,
        transpiled,
        routed,
        diagnostics,
        parse_time,
        lower_time,
        opt_time,
    }
}

pub fn run(options: Options) -> i32 {
    let source = match std::fs::read_to_string(&options.input) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("error: cannot read {}: {error}", options.input.display());
            return 1;
        }
    };

    let name = options.input.display().to_string();
    let file = SourceFile::new(name.clone(), source.clone());
    let compilation = compile_for(
        &source,
        options.opt_level,
        options.verify_each,
        &Target {
            basis: options.basis,
            coupling: options.coupling.clone(),
        },
    );

    for violation in &compilation.stats.violations {
        eprintln!("internal error: verifier: {violation}");
    }

    let color = options.color.enabled();
    let mut errors = 0;
    for diagnostic in &compilation.diagnostics {
        eprint!("{}", diagnostic.render_styled(&file, color));
        eprintln!();
        if diagnostic.severity == Severity::Error {
            errors += 1;
        }
    }

    if !compilation.stats.violations.is_empty() {
        eprintln!(
            "error: aborting after {} verifier violation(s)",
            compilation.stats.violations.len()
        );
        return 1;
    }

    if errors > 0 {
        eprintln!(
            "error: aborting due to {errors} previous error{}",
            if errors == 1 { "" } else { "s" }
        );
        return 1;
    }

    let program = &compilation.program;

    if options.verbose {
        eprintln!(
            "parse {:?}, lower {:?}, optimise {:?}",
            compilation.parse_time, compilation.lower_time, compilation.opt_time
        );
        eprint!("{}", compilation.stats);
        if let Some(stats) = &compilation.transpiled {
            eprintln!("{stats}");
        }
        if let Some(stats) = &compilation.routed {
            eprintln!("{stats}");
        }
    }

    let emitted = match options.emit {
        Emit::Check => {
            println!(
                "ok: {} qubits, {} results, {} gates, depth {}, profile {}",
                program.num_qubits,
                program.num_results,
                program.gate_count(),
                program.depth(),
                program.profile.name()
            );
            return 0;
        }
        Emit::Ir => format!("{program}"),
        Emit::Qasm3 => codegen::emit_qasm3(program),
        Emit::Qir => codegen::emit_qir(program),
        Emit::Json => codegen::emit_json(program),
        Emit::Circuit => codegen::emit_circuit(program),
        Emit::Run => return execute(&options, &compilation),
    };

    match &options.output {
        Some(path) => {
            if let Err(error) = std::fs::write(path, &emitted) {
                eprintln!("error: cannot write {}: {error}", path.display());
                return 1;
            }
            eprintln!("wrote {}", path.display());
        }
        None => print!("{emitted}"),
    }

    0
}

fn execute(options: &Options, compilation: &Compilation) -> i32 {
    let program = &compilation.program;

    let qubits = program.num_qubits as usize;
    if qubits > state::MAX_QUBITS {
        eprintln!(
            "error: this program needs {qubits} qubits, but the simulator supports at most {}",
            state::MAX_QUBITS
        );
        match state::memory_required(qubits) {
            Some(bytes) => eprintln!(
                "note: a {qubits} qubit state vector would need {:.1} GiB of memory",
                bytes as f64 / (1024.0 * 1024.0 * 1024.0)
            ),
            None => eprintln!("note: a {qubits} qubit state vector does not fit in memory"),
        }
        eprintln!("note: use --emit qir, qasm3, json or circuit to compile without simulating");
        return 1;
    }

    let seed = options.seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
    });

    println!("source:  {}", options.input.display());
    println!("kernel:  {}", simd::backend());
    println!(
        "program: {} qubits, {} results, {} gates, depth {}, profile {}",
        program.num_qubits,
        program.num_results,
        program.gate_count(),
        program.depth(),
        program.profile.name()
    );

    if compilation.stats.changed() {
        println!(
            "optimised: {} gates removed ({} -> {})",
            compilation.stats.gates_removed(),
            compilation.stats.gates_before,
            compilation.stats.gates_after
        );
    }

    println!();
    print!("{}", codegen::emit_circuit(program));

    let started = Instant::now();
    let outcome = exec::execute(
        program,
        ExecConfig {
            shots: options.shots,
            seed,
            keep_state: options.show_state,
        },
    );
    let elapsed = started.elapsed();

    if outcome.aborted {
        eprintln!("error: execution did not terminate within the step limit");
        return 1;
    }

    if let Some(state) = &outcome.final_state {
        println!();
        if outcome.sampled {
            print!("{state}");
        } else {
            println!("state after the last shot:");
            print!("{state}");
        }

        println!();
        for qubit in 0..program.num_qubits as usize {
            println!("  P(q{qubit} = 1) = {:.6}", state.qubit_probability(qubit));
        }
    }

    if !outcome.messages.is_empty() {
        println!();
        for message in &outcome.messages {
            println!("message: {message}");
        }
    }

    if !outcome.outputs.is_empty() {
        println!();
        println!("output recording:");
        for record in &outcome.outputs {
            println!("  {record}");
        }
    }

    if !outcome.counts.is_empty() {
        let total: u64 = outcome.counts.values().sum();
        println!();
        println!(
            "measurement over {total} shots ({}):",
            if outcome.sampled {
                "sampled from the final state"
            } else {
                "simulated per shot"
            }
        );

        let mut rows: Vec<(&String, &u64)> = outcome.counts.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));

        for (bits, count) in rows {
            println!("  {bits}  {count:>8}   {:.4}", *count as f64 / total as f64);
        }
    }

    println!();
    println!("simulated in {elapsed:.3?}");
    0
}
