use std::path::PathBuf;
use std::time::Instant;

use crate::codegen;
use crate::diag::{Diagnostic, Severity, SourceFile};
use crate::ir::Program;
use crate::lower;
use crate::opt::{self, OptStats};
use crate::parse::parse_module;
use crate::sema;
use crate::simulator::exec::{self, ExecConfig};
use crate::simulator::simd;

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

pub struct Options {
    pub input: PathBuf,
    pub emit: Emit,
    pub opt_level: u8,
    pub shots: u64,
    pub seed: Option<u64>,
    pub show_state: bool,
    pub verbose: bool,
    pub output: Option<PathBuf>,
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

pub struct Compilation {
    pub program: Program,
    pub stats: OptStats,
    pub diagnostics: Vec<Diagnostic>,
    pub parse_time: std::time::Duration,
    pub lower_time: std::time::Duration,
    pub opt_time: std::time::Duration,
}

pub fn compile(source: &str, opt_level: u8) -> Compilation {
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

    let started = Instant::now();
    let stats = opt::optimise(&mut program, opt_level);
    let opt_time = started.elapsed();

    Compilation {
        program,
        stats,
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
    let compilation = compile(&source, options.opt_level);

    let mut errors = 0;
    for diagnostic in &compilation.diagnostics {
        eprint!("{}", diagnostic.render(&file));
        eprintln!();
        if diagnostic.severity == Severity::Error {
            errors += 1;
        }
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

    let seed = options.seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x5EED)
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

    if let Some(state) = &outcome.final_state {
        println!();
        if outcome.sampled {
            print!("{state}");
        } else {
            println!("final state of the last shot (measurement has collapsed it):");
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
