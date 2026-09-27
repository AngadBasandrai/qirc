use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::slice;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::calibration::Calibration;
use crate::codegen;
use crate::cost;
use crate::diag::{Diagnostic, Severity, SourceFile};
use crate::equiv;
use crate::ir::Program;
use crate::lower;
use crate::opt::{self, OptStats};
use crate::parse::parse_module;
use crate::route::{self, Coupling, Relabelled, RouteStats};
use crate::sema;
use crate::simulator::exec::{self, ExecConfig};
use crate::simulator::simd;
use crate::simulator::state;
use crate::synth::{self, Cost};
use crate::transpile::{self, GateSet, TranspileStats};
use crate::verify;

const DIFF_QUBITS: usize = 20;

#[derive(PartialEq, Debug)]
pub enum Emit {
    Run,
    Ir,
    Qasm3,
    Qir,
    Json,
    Circuit,
    Cost,
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
            "cost" => Emit::Cost,
            "check" => Emit::Check,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
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

#[derive(Default)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    fn print(&mut self, text: impl fmt::Display) {
        self.stdout.push_str(&text.to_string());
    }

    fn println(&mut self, text: impl fmt::Display) {
        self.print(text);
        self.stdout.push('\n');
    }

    fn eprint(&mut self, text: impl fmt::Display) {
        self.stderr.push_str(&text.to_string());
    }

    fn eprintln(&mut self, text: impl fmt::Display) {
        self.eprint(text);
        self.stderr.push('\n');
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn timed<T>(work: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = work();
    (value, started.elapsed())
}

#[cfg(target_arch = "wasm32")]
fn timed<T>(work: impl FnOnce() -> T) -> (T, Duration) {
    (work(), Duration::ZERO)
}

#[cfg(not(target_arch = "wasm32"))]
fn clock_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
}

#[cfg(target_arch = "wasm32")]
fn clock_seed() -> u64 {
    1
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
    pub gates: Option<GateSet>,
    pub resynth: Option<usize>,
    pub cost: Cost,
    pub relabel: bool,
    pub coupling: Option<Coupling>,
    pub calibration: Option<Calibration>,
    pub color: Color,
    pub diff: bool,
    pub other: Option<PathBuf>,
}

impl Options {
    pub fn target(&self) -> Target {
        Target {
            gates: self.gates.clone(),
            coupling: self.coupling.clone(),
            resynth: self.resynth,
            cost: self.cost,
            relabel: self.relabel,
            calibration: self.calibration.clone(),
        }
    }
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
            gates: None,
            resynth: None,
            cost: Cost::Gates,
            relabel: false,
            coupling: None,
            calibration: None,
            color: Color::Auto,
            diff: false,
            other: None,
        }
    }
}

pub const USAGE: &str = "\
qirc: a QIR compiler and state vector simulator

usage:
  qirc <input.ll> [options]
  qirc diff <a.ll> [b.ll] [options]
                    check that b.ll, or a.ll compiled with the options,
                    behaves exactly like a.ll at -O0

options:
  --emit <kind>     run | ir | qasm3 | qir | json | circuit | cost | check
                    (default: run)
  -O<n>             optimisation level 0 to 3                         (default: 1)
  --shots <n>       sample n measurement outcomes
  --seed <n>        seed the random number generator
  --no-state        do not print the final state vector
  -o <path>         write emitted output to a file
  --color <when>    auto | always | never                              (default: auto)
  --gates <set>     target gate set: rz-sx-cx | rz-ry-cz or a list like rz,sx,cx
  --exclude <list>  leave gates out of the target set, such as h,t
  --basis <name>    same as --gates
  --resynth <n>     replace runs of gates by at most n gates, 1 to 6
                    (default 1 from -O2)
  --cost <model>    what resynthesis minimises: gates | cx | ibm       (default: gates)
  --relabel         remove swaps at the end of the program by permuting qubits
  --calibration <f> device error rates: lines of cx a b e, single q e, readout q e
  --coupling <map>  route onto hardware: line:N | ring:N | grid:RxC | full:N
                    or an explicit edge list such as 0-1,1-2,2-3
  --verify-each     run the IR verifier after lowering and after every pass
  -v, --verbose     report pipeline statistics
  -h, --help        show this message
";

pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut input: Option<PathBuf> = None;
    let mut excluded: Option<&str> = None;
    let mut args = args.iter();
    if args.as_slice().first().is_some_and(|a| a == "diff") {
        args.next();
        options.diff = true;
    }

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err(String::new()),

            "--emit" => {
                let value = value(&mut args, "--emit needs a kind")?;
                options.emit =
                    Emit::parse(value).ok_or_else(|| format!("unknown emit kind `{value}`"))?;
            }

            "--shots" => {
                let value = value(&mut args, "--shots needs a count")?;
                options.shots = value
                    .parse()
                    .map_err(|_| format!("invalid shot count `{value}`"))?;
            }

            "--seed" => {
                let value = value(&mut args, "--seed needs a number")?;
                options.seed = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid seed `{value}`"))?,
                );
            }

            "-o" => {
                let value = value(&mut args, "-o needs a path")?;
                options.output = Some(PathBuf::from(value));
            }

            "--gates" | "--basis" => {
                let value = value(&mut args, "--gates needs a gate set")?;
                options.gates = Some(GateSet::parse(value)?);
            }

            "--exclude" => excluded = Some(value(&mut args, "--exclude needs a list of gates")?),

            "--resynth" => {
                let value = value(&mut args, "--resynth needs a length")?;
                options.resynth = match value.parse() {
                    Ok(n @ 1..=6) => Some(n),
                    _ => return Err(format!("resynthesis length `{value}` must be 1 to 6")),
                };
            }

            "--cost" => {
                let value = value(&mut args, "--cost needs a model")?;
                options.cost = Cost::parse(value).ok_or_else(|| {
                    format!("unknown cost model `{value}`, expected gates, cx or ibm")
                })?;
            }

            "--relabel" => options.relabel = true,

            "--calibration" => {
                let path = value(&mut args, "--calibration needs a file")?;
                let text = fs::read_to_string(path)
                    .map_err(|error| format!("cannot read calibration {path}: {error}"))?;
                options.calibration = Some(
                    Calibration::parse(&text)
                        .map_err(|error| format!("calibration {path}: {error}"))?,
                );
            }

            "--coupling" => {
                let value = value(&mut args, "--coupling needs a map")?;
                options.coupling = Some(
                    Coupling::parse(value)
                        .ok_or_else(|| format!("unknown coupling map `{value}`"))?,
                );
            }

            "--color" => {
                let value = value(&mut args, "--color needs auto, always or never")?;
                options.color = match value {
                    "auto" => Color::Auto,
                    "always" => Color::Always,
                    "never" => Color::Never,
                    _ => return Err(format!("unknown color setting `{value}`")),
                };
            }

            "--verify-each" => options.verify_each = true,

            "--no-state" => options.show_state = false,

            "-v" | "--verbose" => options.verbose = true,

            arg if arg.starts_with("-O") => {
                let level = arg[2..]
                    .parse::<u8>()
                    .map_err(|_| format!("invalid optimisation level `{arg}`"))?;
                if level > 3 {
                    return Err(format!("optimisation level {level} is out of range"));
                }
                options.opt_level = level;
            }

            arg if arg.starts_with('-') => return Err(format!("unknown option `{arg}`")),

            path if input.is_none() => input = Some(PathBuf::from(path)),

            path if options.diff && options.other.is_none() => {
                options.other = Some(PathBuf::from(path));
            }

            _ => return Err("too many input files".into()),
        }
    }

    if let Some(list) = excluded {
        options.gates = Some(
            options
                .gates
                .take()
                .unwrap_or_else(GateSet::native)
                .without(list)?,
        );
    }
    options.input = input.ok_or_else(|| "no input file given".to_string())?;
    Ok(options)
}

fn value<'a>(args: &mut slice::Iter<'a, String>, missing: &str) -> Result<&'a str, String> {
    args.next()
        .map(String::as_str)
        .ok_or_else(|| missing.to_string())
}

#[derive(Default)]
pub struct Target {
    pub gates: Option<GateSet>,
    pub coupling: Option<Coupling>,
    pub resynth: Option<usize>,
    pub cost: Cost,
    pub relabel: bool,
    pub calibration: Option<Calibration>,
}

pub struct Compilation {
    pub program: Program,
    pub stats: OptStats,
    pub transpiled: Option<TranspileStats>,
    pub routed: Option<RouteStats>,
    pub relabelled: Option<Relabelled>,
    pub diagnostics: Vec<Diagnostic>,
    pub parse_time: Duration,
    pub lower_time: Duration,
    pub opt_time: Duration,
}

pub fn compile(source: &str, opt_level: u8) -> Compilation {
    compile_for(source, opt_level, false, &Target::default())
}

pub fn compile_for(source: &str, opt_level: u8, verify_each: bool, target: &Target) -> Compilation {
    let mut diagnostics = Vec::new();

    let ((module, parse_errors), parse_time) = timed(|| parse_module(source));
    diagnostics.extend(parse_errors);

    let (lowered, lower_time) = timed(|| lower::lower(&module));
    diagnostics.extend(lowered.diagnostics);

    let mut program = lowered.program;
    diagnostics.extend(sema::validate(&program));

    let mut lowering_violations = Vec::new();
    let lowered_cleanly = !diagnostics.iter().any(|d| d.severity == Severity::Error);
    if verify_each && lowered_cleanly {
        for found in verify::verify(&program) {
            lowering_violations.push(format!("after lowering: {found}"));
        }
    }

    let (mut stats, opt_time) =
        timed(|| opt::optimise(&mut program, opt_level, verify_each && lowered_cleanly));

    lowering_violations.append(&mut stats.violations);
    stats.violations = lowering_violations;

    let coupling = target
        .coupling
        .clone()
        .or_else(|| target.calibration.as_ref().map(Calibration::coupling));
    let relabel = target.relabel || coupling.is_some();
    let mut relabelled = relabel.then(|| route::elide_swaps(&mut program));

    let transpiled = target
        .gates
        .as_ref()
        .map(|set| transpile::transpile(&mut program, set));
    if let Some(stats) = &transpiled
        && !stats.leftover.is_empty()
    {
        diagnostics.push(
            Diagnostic::warning(format!(
                "no exact form in gate set {} for {}, left as written",
                stats.set,
                stats.leftover.join(", ")
            ))
            .with_code("QIR0401"),
        );
    }

    let native = GateSet::native();
    let set = target.gates.as_ref().unwrap_or(&native);
    let limit = target.resynth.unwrap_or(usize::from(opt_level >= 2));
    let replaced = synth::resynthesize(&mut program, set, limit, target.cost);
    if let Some(first) = &mut relabelled {
        let again = route::elide_swaps(&mut program);
        first.swaps_removed += again.swaps_removed;
        first.final_layout = first
            .final_layout
            .iter()
            .map(|&w| again.final_layout[w])
            .collect();
    }
    if replaced > 0 {
        stats.applied.push(("resynthesize", replaced));
        stats.gates_after = program.gate_count();
        stats.depth_after = program.depth();
        stats.ops_after = program.op_count();
    }

    let mut routed = None;
    if let Some(coupling) = &coupling {
        if target.gates.is_none() {
            transpile::transpile(&mut program, &GateSet::native().local());
        }
        match route::route(&mut program, coupling, target.calibration.as_ref()) {
            Ok(mut stats) => {
                if let Some(relabelled) = &relabelled {
                    let physical = stats.final_layout.clone();
                    for (q, slot) in stats.final_layout.iter_mut().enumerate() {
                        *slot = physical[relabelled.final_layout.get(q).copied().unwrap_or(q)];
                    }
                }
                routed = Some(stats);
            }
            Err(message) => diagnostics.push(
                Diagnostic::error(format!("cannot route this program: {message}"))
                    .with_code("QIR0400"),
            ),
        }
    }
    if routed.is_some() {
        if let Some(gates) = &target.gates {
            transpile::transpile(&mut program, gates);
        }
        synth::resynthesize(&mut program, set, limit, target.cost);
    }

    if verify_each && (transpiled.is_some() || routed.is_some()) {
        for found in verify::verify(&program) {
            stats.violations.push(format!("after targeting: {found}"));
        }
    }

    Compilation {
        program,
        stats,
        transpiled,
        routed,
        relabelled,
        diagnostics,
        parse_time,
        lower_time,
        opt_time,
    }
}

fn read(path: &PathBuf, output: &mut Output) -> Option<SourceFile> {
    match fs::read_to_string(path) {
        Ok(text) => Some(SourceFile::new(path.display().to_string(), text)),
        Err(error) => {
            output.eprintln(format_args!(
                "error: cannot read {}: {error}",
                path.display()
            ));
            None
        }
    }
}

fn report(file: &SourceFile, compilation: &Compilation, color: bool, output: &mut Output) -> bool {
    for violation in &compilation.stats.violations {
        output.eprintln(format_args!("internal error: verifier: {violation}"));
    }

    let mut errors = 0;
    for diagnostic in &compilation.diagnostics {
        output.eprintln(format_args!("{}", diagnostic.render_styled(file, color)));
        if diagnostic.severity == Severity::Error {
            errors += 1;
        }
    }

    if !compilation.stats.violations.is_empty() {
        output.eprintln(format_args!(
            "error: aborting after {} verifier violation(s)",
            compilation.stats.violations.len()
        ));
        return false;
    }

    if errors > 0 {
        output.eprintln(format_args!(
            "error: aborting due to {errors} previous error{}",
            if errors == 1 { "" } else { "s" }
        ));
        return false;
    }

    true
}

fn diff(options: &Options, left: &SourceFile, right: &SourceFile, output: &mut Output) -> i32 {
    let color = options.color.enabled();

    let reference = compile_for(&left.text, 0, false, &Target::default());
    let candidate = compile_for(
        &right.text,
        options.opt_level,
        options.verify_each,
        &options.target(),
    );
    if !report(left, &reference, color, output) || !report(right, &candidate, color, output) {
        return 1;
    }

    for program in [&reference.program, &candidate.program] {
        if program.num_qubits as usize > DIFF_QUBITS {
            output.eprintln(format_args!(
                "error: diff follows every branch and supports at most {DIFF_QUBITS} qubits"
            ));
            return 1;
        }
    }

    let states = options.coupling.is_none() && options.calibration.is_none() && !options.relabel;
    let (a, b) = (
        equiv::explore(&reference.program),
        equiv::explore(&candidate.program),
    );
    let differences = equiv::compare(&a, &b, states);

    let unexplored = a.unexplored.max(b.unexplored);
    let complete = unexplored <= 1e-9;
    let count = a.branches.len();
    let noun = if count == 1 {
        "outcome agrees"
    } else {
        "outcomes agree"
    };
    let checked = if states {
        "probability and final state"
    } else {
        "probability"
    };

    if !differences.is_empty() {
        output.println(format_args!("different:"));
        for difference in differences.iter().take(8) {
            output.println(format_args!("  {difference}"));
        }
    } else if complete {
        output.println(format_args!("equivalent: {count} {noun} in {checked}"));
    } else {
        output.println(format_args!(
            "inconclusive: {count} {noun} in {checked}, but {unexplored:.2e} of the probability was not explored"
        ));
    }
    if !states {
        output.println(format_args!(
            "note: final states are not compared because the compiled program moves qubits"
        ));
    }

    match (differences.is_empty(), complete) {
        (false, _) => 1,
        (true, true) => 0,
        (true, false) => 2,
    }
}

pub fn run(options: Options) -> i32 {
    let mut output = Output::default();
    let code = match read(&options.input, &mut output) {
        Some(file) => match &options.other {
            Some(path) => match read(path, &mut output) {
                Some(other) => run_source(&options, &file, Some(&other), &mut output),
                None => 1,
            },
            None => run_source(&options, &file, None, &mut output),
        },
        None => 1,
    };
    eprint!("{}", output.stderr);
    print!("{}", output.stdout);
    code
}

pub fn run_source(
    options: &Options,
    file: &SourceFile,
    other: Option<&SourceFile>,
    output: &mut Output,
) -> i32 {
    if options.diff {
        return diff(options, file, other.unwrap_or(file), output);
    }

    let compilation = compile_for(
        &file.text,
        options.opt_level,
        options.verify_each,
        &options.target(),
    );

    let color = options.color.enabled();
    if !report(file, &compilation, color, output) {
        return 1;
    }

    let program = &compilation.program;

    if options.verbose {
        output.eprintln(format_args!(
            "parse {:?}, lower {:?}, optimise {:?}",
            compilation.parse_time, compilation.lower_time, compilation.opt_time
        ));
        output.eprint(format_args!("{}", compilation.stats));
        if let Some(stats) = &compilation.transpiled {
            output.eprintln(format_args!("{stats}"));
        }
        if let Some(stats) = &compilation.relabelled {
            output.eprintln(stats);
        }
        if let Some(stats) = &compilation.routed {
            output.eprintln(format_args!("{stats}"));
        }
    }

    let emitted = match options.emit {
        Emit::Check => {
            output.println(format_args!("ok: {}", summary(program)));
            return 0;
        }
        Emit::Ir => format!("{program}"),
        Emit::Qasm3 => match codegen::emit_qasm3(program) {
            Ok(text) => text,
            Err(reason) => {
                output.eprintln(format_args!("error: cannot emit OpenQASM 3: {reason}"));
                output.eprintln(format_args!("note: --emit qir keeps the whole program"));
                return 1;
            }
        },
        Emit::Qir => match codegen::emit_qir(program) {
            Ok(text) => text,
            Err(reason) => {
                output.eprintln(format_args!("error: cannot emit QIR: {reason}"));
                return 1;
            }
        },
        Emit::Json => codegen::emit_json(program),
        Emit::Circuit => codegen::emit_circuit(program),
        Emit::Cost => cost::analyse(program, options.calibration.as_ref()).to_string(),
        Emit::Run => return execute(options, &compilation, output),
    };

    match &options.output {
        Some(path) => {
            if let Err(error) = fs::write(path, &emitted) {
                output.eprintln(format_args!(
                    "error: cannot write {}: {error}",
                    path.display()
                ));
                return 1;
            }
            output.eprintln(format_args!("wrote {}", path.display()));
        }
        None => output.print(emitted),
    }

    0
}

fn execute(options: &Options, compilation: &Compilation, output: &mut Output) -> i32 {
    let program = &compilation.program;

    let qubits = program.num_qubits as usize;
    if qubits > state::MAX_QUBITS {
        output.eprintln(format_args!(
            "error: this program needs {qubits} qubits, but the simulator supports at most {}",
            state::MAX_QUBITS
        ));
        match state::memory_required(qubits) {
            Some(bytes) => output.eprintln(format_args!(
                "note: a {qubits} qubit state vector would need {:.1} GiB of memory",
                bytes as f64 / (1024.0 * 1024.0 * 1024.0)
            )),
            None => output.eprintln(format_args!(
                "note: a {qubits} qubit state vector does not fit in memory"
            )),
        }
        output.eprintln(format_args!(
            "note: use --emit qir, qasm3, json or circuit to compile without simulating"
        ));
        return 1;
    }

    let seed = options.seed.unwrap_or_else(clock_seed);

    output.println(format_args!("source:  {}", options.input.display()));
    output.println(format_args!("kernel:  {}", simd::backend()));
    output.println(format_args!("program: {}", summary(program)));

    if compilation.stats.changed() {
        output.println(format_args!(
            "optimised: {} gates removed ({} -> {})",
            compilation.stats.gates_removed(),
            compilation.stats.gates_before,
            compilation.stats.gates_after
        ));
    }

    output.println("");
    output.print(format_args!("{}", codegen::emit_circuit(program)));

    let (outcome, elapsed) = timed(|| {
        exec::execute(
            program,
            ExecConfig {
                shots: options.shots,
                seed,
                keep_state: options.show_state,
            },
        )
    });

    if outcome.aborted {
        output.eprintln(format_args!(
            "error: execution did not terminate within the step limit"
        ));
        return 1;
    }

    if let Some(state) = &outcome.final_state {
        output.println("");
        if !outcome.sampled {
            output.println(format_args!("state after the last shot:"));
        }
        output.print(format_args!("{state}"));

        output.println("");
        for qubit in 0..program.num_qubits as usize {
            output.println(format_args!(
                "  P(q{qubit} = 1) = {:.6}",
                state.qubit_probability(qubit)
            ));
        }
    }

    if !outcome.messages.is_empty() {
        output.println("");
        for message in &outcome.messages {
            output.println(format_args!("message: {message}"));
        }
    }

    if !outcome.outputs.is_empty() {
        output.println("");
        output.println(format_args!("output recording:"));
        for record in &outcome.outputs {
            output.println(format_args!("  {record}"));
        }
    }

    print_tally(output, "returned", "", &outcome.returns);
    print_tally(
        output,
        "measurement",
        if outcome.sampled {
            " (sampled from the final state)"
        } else {
            " (simulated per shot)"
        },
        &outcome.counts,
    );

    if !elapsed.is_zero() {
        output.println("");
        output.println(format_args!("simulated in {elapsed:.3?}"));
    }
    0
}

fn summary(program: &Program) -> String {
    format!(
        "{} qubits, {} results, {} gates, depth {}, profile {}",
        program.num_qubits,
        program.num_results,
        program.gate_count(),
        program.depth(),
        program.profile.name()
    )
}

fn print_tally(output: &mut Output, name: &str, note: &str, tally: &BTreeMap<String, u64>) {
    if tally.is_empty() {
        return;
    }

    let total: u64 = tally.values().sum();
    let width = tally.keys().map(String::len).max().unwrap_or(0);
    let mut rows: Vec<(&String, &u64)> = tally.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));

    output.println("");
    output.println(format_args!("{name} over {total} shots{note}:"));
    for (key, count) in rows {
        output.println(format_args!(
            "  {key:<width$}  {count:>8}   {:.4}",
            *count as f64 / total as f64
        ));
    }
}
