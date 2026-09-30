use std::collections::{BTreeMap, HashMap};
use std::f64::consts::FRAC_PI_2;
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal};
use std::panic;
use std::path::PathBuf;
use std::slice;
use std::thread;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::calibration::Calibration;
use crate::codegen;
use crate::cost;
use crate::diag::{Diagnostic, Severity, SourceFile, suggest};
use crate::draw;
use crate::equiv;
use crate::gridsynth;
use crate::ir::Program;
use crate::lower;
use crate::observable::{self, Observable, Sampled};
use crate::opt::{self, OptStats};
use crate::parse::parse_module;
use crate::progress::{self, Progress};
use crate::provider;
use crate::pulse;
use crate::qasm;
use crate::qasm2;
use crate::resources;
use crate::reuse::{self, Reused};
use crate::rotations;
use crate::route::{self, Coupling, Relabelled, RouteStats};
use crate::sema;
use crate::simulator::exec::{self, ExecConfig};
use crate::simulator::state::Rng;
use crate::simulator::{mps, state};
use crate::stim;
use crate::synth::{self, Cost};
use crate::transpile::{self, GateSet, TranspileStats};
use crate::verify;

const DIFF_QUBITS: usize = 20;
const COMPILER_STACK: usize = 256 << 20;
const SUBMIT_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_DRAWN: usize = 200_000;
const STEP: f64 = 1e-5;
const START: f64 = 0.1;
const MAX_STEPS: usize = 500;
const FLAT: f64 = 1e-6;
const NOISY_SHOTS: u64 = 1000;
const SPSA_STEPS: usize = 150;
const SPSA_SHIFT: f64 = 0.2;
const SPSA_FIRST: f64 = 0.2;
const SPSA_OFFSET: f64 = 15.0;
const SPSA_TRIALS: usize = 4;
const SHIFT: f64 = FRAC_PI_2;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Emit {
    Run,
    Ir,
    Qasm3,
    Qasm2,
    Stim,
    Pulse,
    Schedule,
    Ionq,
    Qir,
    Json,
    Circuit,
    Quantikz,
    Svg,
    Cost,
    Resources,
    Rotations,
    Check,
}

const EMITS: [(&str, Emit); 21] = [
    ("run", Emit::Run),
    ("ir", Emit::Ir),
    ("qasm", Emit::Qasm3),
    ("qasm3", Emit::Qasm3),
    ("qasm2", Emit::Qasm2),
    ("stim", Emit::Stim),
    ("pulse", Emit::Pulse),
    ("openpulse", Emit::Pulse),
    ("schedule", Emit::Schedule),
    ("ionq", Emit::Ionq),
    ("qir", Emit::Qir),
    ("llvm", Emit::Qir),
    ("json", Emit::Json),
    ("circuit", Emit::Circuit),
    ("quantikz", Emit::Quantikz),
    ("latex", Emit::Quantikz),
    ("svg", Emit::Svg),
    ("cost", Emit::Cost),
    ("resources", Emit::Resources),
    ("rotations", Emit::Rotations),
    ("check", Emit::Check),
];

impl Emit {
    pub fn parse(text: &str) -> Option<Emit> {
        EMITS
            .iter()
            .find(|(name, _)| *name == text)
            .map(|&(_, emit)| emit)
    }
}

fn unknown(what: &str, value: &str, candidates: impl IntoIterator<Item = &'static str>) -> String {
    match suggest(value, candidates) {
        Some(found) => format!("unknown {what} `{value}`, did you mean `{found}`?"),
        None => format!("unknown {what} `{value}`"),
    }
}

fn option_names() -> impl Iterator<Item = &'static str> {
    USAGE
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|word| word.starts_with('-'))
        .map(|word| word.trim_end_matches(','))
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
    pub reuse: bool,
    pub noisy: bool,
    pub mitigate: bool,
    pub zne: bool,
    pub bond: Option<usize>,
    pub epsilon: Option<f64>,
    pub budget: f64,
    pub bindings: HashMap<String, f64>,
    pub gradient: bool,
    pub minimize: bool,
    pub observable: Option<Observable>,
    pub coupling: Option<Coupling>,
    pub calibration: Option<Calibration>,
    pub color: Color,
    pub diff: bool,
    pub submit: bool,
    pub device: String,
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
            reuse: self.reuse,
            calibration: self.calibration.clone(),
            epsilon: self.epsilon,
            bindings: self.bindings.clone(),
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
            reuse: false,
            noisy: false,
            mitigate: false,
            zne: false,
            bond: None,
            epsilon: None,
            budget: 1e-3,
            bindings: HashMap::new(),
            gradient: false,
            minimize: false,
            observable: None,
            coupling: None,
            calibration: None,
            color: Color::Auto,
            diff: false,
            submit: false,
            device: "simulator".into(),
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
  qirc submit <input.ll> [options]
                    run on IonQ with the key in IONQ_API_KEY, on --target
                    (default simulator) with --shots
  qirc explain <code>
                    explain an error code such as QIR0300
  qirc surface [--distance 3,5,7] [--error p | --calibration f] [--rounds n] [--shots n]
                    simulate surface code memory and compare its logical error with
                    the rate --emit resources assumes
  qirc lsp          report diagnostics for .ll and .qasm files to an editor
                    over the language server protocol on stdin and stdout

options:
  --emit <kind>     run | ir | qasm3 | qasm2 | stim | pulse | schedule | ionq | qir | json
                    | circuit | quantikz | svg | cost | resources | rotations | check
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
  --epsilon <e>     approximate rotations a gate set like h,s,t,cx cannot express,
                    each to within e in operator norm, down to about 1e-10
  --bind <list>     values for OpenQASM 3 inputs, such as theta=0.3,phi=1.2
  --gradient        print how the --observable changes with every input
  --minimize        vary the inputs to minimise the --observable, starting from --bind;
                    with --noisy or --zne it runs SPSA on sampled noisy values
  --budget <p>      total failure probability for --emit resources      (default: 0.001)
  --cost <model>    what resynthesis minimises: gates | cx | ibm       (default: gates)
  --relabel         remove swaps at the end of the program by permuting qubits
  --reuse           reset measured qubits and reuse them to need fewer qubits
  --noisy           simulate with the error rates from --calibration
  --target <name>   IonQ target for submit and --emit ionq, such as qpu.aria-1
  --bond <n>        simulate as a matrix product state with bonds up to n (default 32
                    above 30 qubits)
  --dd              fill idle windows with echo pulses that cancel detuning, using the
                    gate times from --calibration
  --mitigate        undo the readout errors from --calibration in the counts
  --zne             extrapolate the --observable to zero noise from 1x, 2x and 3x noise
  --observable <p>  print the exact expectation of a Pauli sum such as 'Z0 Z1 + 0.5 X2'
  --calibration <f> device error rates: lines of cx a b e, single q e, readout q e
  --coupling <map>  route onto hardware: line:N | ring:N | grid:RxC | full:N
                    or an explicit edge list such as 0-1,1-2,2-3
  --verify-each     run the IR verifier after lowering and after every pass
  -v, --verbose     report pipeline statistics
  -h, --help        show this message
";

pub fn parse_args(args: &[String]) -> Result<Options, String> {
    parse_args_with(args, |path| {
        fs::read_to_string(path).map_err(|error| format!("cannot read calibration {path}: {error}"))
    })
}

pub fn parse_args_with(
    args: &[String],
    read: impl Fn(&str) -> Result<String, String>,
) -> Result<Options, String> {
    let mut options = Options::default();
    let mut input: Option<PathBuf> = None;
    let mut excluded: Option<&str> = None;
    let mut decouple = false;
    let mut args = args.iter();
    match args.as_slice().first().map(String::as_str) {
        Some("diff") => {
            args.next();
            options.diff = true;
        }
        Some("submit") => {
            args.next();
            options.submit = true;
        }
        _ => {}
    }

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err(String::new()),

            "--emit" => {
                let value = value(&mut args, "--emit needs a kind")?;
                options.emit = Emit::parse(value)
                    .ok_or_else(|| unknown("emit kind", value, EMITS.map(|(name, _)| name)))?;
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

            "--bind" => {
                let list = value(&mut args, "--bind needs values such as theta=0.3")?;
                for pair in list.split(',') {
                    let parsed = pair
                        .split_once('=')
                        .and_then(|(name, number)| {
                            Some((name.trim(), number.trim().parse::<f64>().ok()?))
                        })
                        .filter(|(name, number)| !name.is_empty() && number.is_finite());
                    let Some((name, number)) = parsed else {
                        return Err(format!("`{pair}` is not a binding like theta=0.3"));
                    };
                    options.bindings.insert(name.to_string(), number);
                }
            }
            "--gradient" => options.gradient = true,
            "--minimize" | "--minimise" => options.minimize = true,

            "--budget" => {
                let value = value(&mut args, "--budget needs an error probability")?;
                options.budget = match value.parse::<f64>() {
                    Ok(b) if b > 0.0 && b < 1.0 => b,
                    _ => return Err(format!("budget `{value}` must be above 0 and below 1")),
                };
            }

            "--epsilon" => {
                let value = value(&mut args, "--epsilon needs a precision")?;
                options.epsilon = match value.parse::<f64>() {
                    Ok(e) if e > 0.0 && e < 0.5 => Some(e),
                    _ => return Err(format!("precision `{value}` must be above 0 and below 0.5")),
                };
            }

            "--cost" => {
                let value = value(&mut args, "--cost needs a model")?;
                options.cost = Cost::parse(value).ok_or_else(|| {
                    format!("unknown cost model `{value}`, expected gates, cx or ibm")
                })?;
            }

            "--relabel" => options.relabel = true,

            "--reuse" => options.reuse = true,

            "--noisy" => options.noisy = true,

            "--target" => {
                options.device = value(
                    &mut args,
                    "--target needs a provider target such as simulator",
                )?
                .to_string();
            }

            "--bond" => {
                let value = value(&mut args, "--bond needs a bond dimension")?;
                options.bond = Some(
                    value
                        .parse()
                        .ok()
                        .filter(|&b: &usize| b > 0)
                        .ok_or_else(|| format!("invalid bond dimension `{value}`"))?,
                );
            }

            "--mitigate" => options.mitigate = true,
            "--dd" => decouple = true,

            "--zne" => options.zne = true,

            "--observable" => {
                let value = value(
                    &mut args,
                    "--observable needs a Pauli sum such as \"Z0 Z1 + 0.5 X2\"",
                )?;
                options.observable = Some(Observable::parse(value)?);
            }

            "--calibration" => {
                let path = value(&mut args, "--calibration needs a file")?;
                let text = read(path)?;
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

            arg if arg.starts_with('-') => return Err(unknown("option", arg, option_names())),

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
    if (options.noisy || options.mitigate || options.zne) && options.calibration.is_none() {
        return Err("--noisy, --mitigate and --zne need --calibration for its error rates".into());
    }
    if options.zne && options.observable.is_none() {
        return Err("--zne needs --observable for the value to extrapolate".into());
    }
    if (options.gradient || options.minimize) && options.observable.is_none() {
        return Err("--gradient and --minimize need --observable for the value to vary".into());
    }
    if decouple {
        match &mut options.calibration {
            Some(calibration) if calibration.timed() => calibration.decouple = true,
            _ => {
                return Err("--dd needs --calibration with gate times to find idle windows".into());
            }
        }
    }
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
    pub reuse: bool,
    pub calibration: Option<Calibration>,
    pub epsilon: Option<f64>,
    pub bindings: HashMap<String, f64>,
}

pub struct Compilation {
    pub program: Program,
    pub stats: OptStats,
    pub transpiled: Option<TranspileStats>,
    pub routed: Option<RouteStats>,
    pub relabelled: Option<Relabelled>,
    pub reused: Option<Reused>,
    pub diagnostics: Vec<Diagnostic>,
    pub parse_time: Duration,
    pub lower_time: Duration,
    pub opt_time: Duration,
}

pub fn compile(source: &str, opt_level: u8) -> Compilation {
    compile_for(source, opt_level, false, &Target::default())
}

pub fn compile_for(source: &str, opt_level: u8, verify_each: bool, target: &Target) -> Compilation {
    if cfg!(target_arch = "wasm32") {
        return compile_now(source, opt_level, verify_each, target);
    }
    thread::scope(|scope| {
        thread::Builder::new()
            .stack_size(COMPILER_STACK)
            .spawn_scoped(scope, || {
                compile_now(source, opt_level, verify_each, target)
            })
            .map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|panic| panic::resume_unwind(panic))
            })
            .unwrap_or_else(|_| compile_now(source, opt_level, verify_each, target))
    })
}

fn compile_now(source: &str, opt_level: u8, verify_each: bool, target: &Target) -> Compilation {
    let mut diagnostics = Vec::new();

    let (mut program, parse_time, lower_time) = if qasm::is_qasm(source) {
        let ((program, errors), time) = timed(|| qasm::lower_with(source, &target.bindings));
        diagnostics.extend(errors);
        (program, time, Duration::ZERO)
    } else {
        let ((module, parse_errors), parse_time) = timed(|| parse_module(source));
        diagnostics.extend(parse_errors);
        if !target.bindings.is_empty() {
            diagnostics.push(
                Diagnostic::error("--bind sets OpenQASM 3 `input` values, and this program is QIR")
                    .with_code("QIR0100"),
            );
        }
        let (lowered, lower_time) = timed(|| lower::lower(&module));
        diagnostics.extend(lowered.diagnostics);
        (lowered.program, parse_time, lower_time)
    };
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

    let reused = if target.reuse {
        reuse::reuse(&mut program)
    } else {
        None
    };

    let coupling = target
        .coupling
        .clone()
        .or_else(|| target.calibration.as_ref().map(Calibration::coupling));
    let relabel = target.relabel || coupling.is_some();
    let mut relabelled = relabel.then(|| route::elide_swaps(&mut program));

    let mut transpiled = target.gates.as_ref().map(|set| match target.epsilon {
        Some(_) => transpile::transpile(&mut program, &set.clone().with_rotations()),
        None => transpile::transpile(&mut program, set),
    });
    if let (Some(set), Some(epsilon), Some(stats)) = (&target.gates, target.epsilon, &transpiled) {
        let before = stats.gates_before;
        match gridsynth::approximate(&mut program, set, epsilon) {
            Ok(_) => {
                let mut again = transpile::transpile(&mut program, set);
                again.gates_before = before;
                transpiled = Some(again);
            }
            Err(message) => {
                diagnostics.push(Diagnostic::error(message).with_code("QIR0402"));
            }
        }
    }
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
    let mut replaced = synth::resynthesize(&mut program, set, limit, target.cost);
    if opt_level > 0 {
        replaced += opt::drop_before_measure(&mut program);
    }
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
        if opt_level > 0 {
            opt::drop_before_measure(&mut program);
        }
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
        reused,
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
        if let Some(code) = compilation.diagnostics.iter().find_map(|d| d.code) {
            output.eprintln(format_args!(
                "for more on an error, run `qirc explain {code}`"
            ));
        }
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
        if program.num_qubits as usize > DIFF_QUBITS && !exec::stabilizer(program) {
            output.eprintln(format_args!(
                "error: diff follows every branch and supports at most {DIFF_QUBITS} qubits unless the program only uses Clifford gates"
            ));
            return 1;
        }
    }

    let states = options.coupling.is_none()
        && options.calibration.is_none()
        && !options.relabel
        && !options.reuse;
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

fn bound(options: &Options, source: &str, values: &[(String, f64)]) -> Result<Program, String> {
    let mut target = options.target();
    target.bindings = values.iter().cloned().collect();
    let compilation = compile_for(source, options.opt_level, false, &target);
    if let Some(error) = compilation
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
    {
        return Err(error.message.clone());
    }
    Ok(compilation.program)
}

fn energy(options: &Options, source: &str, values: &[(String, f64)]) -> Result<f64, String> {
    let observable = options.observable.as_ref().ok_or("no observable to vary")?;
    observable::expectation(&bound(options, source, values)?, observable)
}

struct Noisy<'a> {
    options: &'a Options,
    source: &'a str,
    observable: &'a Observable,
    calibration: &'a Calibration,
    shots: u64,
    seed: u64,
}

fn around((plus, plus_error): Sampled, (minus, minus_error): Sampled) -> (Sampled, Sampled) {
    let error = plus_error.hypot(minus_error) / 2.0;
    (((plus + minus) / 2.0, error), ((plus - minus) / 2.0, error))
}

fn moved(values: &[(String, f64)], i: usize, delta: f64) -> Vec<(String, f64)> {
    let mut moved = values.to_vec();
    moved[i].1 += delta;
    moved
}

impl<'a> Noisy<'a> {
    fn new(options: &'a Options, source: &'a str) -> Result<Noisy<'a>, String> {
        Ok(Noisy {
            options,
            source,
            observable: options.observable.as_ref().ok_or("no observable to vary")?,
            calibration: options
                .calibration
                .as_ref()
                .ok_or("no calibration for the noise")?,
            shots: options.shots.max(NOISY_SHOTS),
            seed: options.seed.unwrap_or_else(clock_seed),
        })
    }

    fn value(&mut self, values: &[(String, f64)]) -> Result<Sampled, String> {
        let program = bound(self.options, self.source, values)?;
        self.seed = self.seed.wrapping_add(1);
        if self.options.zne {
            observable::extrapolate(
                &program,
                self.observable,
                self.calibration,
                self.shots,
                self.seed,
            )
            .map(|(_, zero)| zero)
        } else {
            observable::noisy_expectation(
                &program,
                self.observable,
                self.calibration,
                self.shots,
                self.seed,
            )
        }
    }

    fn compared(&self, (value, error): Sampled, exact: f64) -> String {
        let kind = if self.options.zne {
            "zero noise extrapolation"
        } else {
            "noisy value"
        };
        format!(
            "  {kind:<26}{value:.6} ± {error:.6}\n  {:<26}{exact:.6}",
            "exact value without noise"
        )
    }

    fn kick(
        &mut self,
        values: &[(String, f64)],
        shift: f64,
        rng: &mut Rng,
    ) -> Result<(Vec<f64>, Sampled), String> {
        let signs: Vec<f64> = values
            .iter()
            .map(|_| if rng.next_u64() & 1 == 1 { 1.0 } else { -1.0 })
            .collect();
        let moved = |direction: f64| {
            values
                .iter()
                .zip(&signs)
                .map(|((name, value), sign)| (name.clone(), value + direction * shift * sign))
                .collect::<Vec<_>>()
        };
        let (mean, (difference, _)) = around(self.value(&moved(1.0))?, self.value(&moved(-1.0))?);
        let slope = signs
            .iter()
            .map(|sign| difference / (shift * sign))
            .collect();
        Ok((slope, mean))
    }
}

fn noisy_minimize(
    options: &Options,
    source: &str,
    values: &mut [(String, f64)],
    output: &mut Output,
) -> Result<(), String> {
    let mut noisy = Noisy::new(options, source)?;
    let mut rng = Rng::new(noisy.seed);
    let names: Vec<&str> = values.iter().map(|(name, _)| name.as_str()).collect();
    output.println(format_args!(
        "minimising {} over {} by SPSA with the calibration's noise, {} shots per value{}\n",
        noisy.observable,
        names.join(", "),
        noisy.shots,
        if options.zne {
            ", each extrapolated to zero noise"
        } else {
            ""
        }
    ));
    let header: String = names.iter().map(|name| format!("{name:>12}")).collect();
    output.println(format_args!("{:>6}{:>24}{header}", "step", "value"));
    let row =
        |output: &mut Output, step: usize, (value, error): Sampled, values: &[(String, f64)]| {
            let columns: String = values.iter().map(|(_, v)| format!("{v:>12.6}")).collect();
            let shown = format!("{value:.6} ± {error:.6}");
            output.println(format_args!("{step:>6}{shown:>24}{columns}"));
        };
    let mut total = 0.0;
    for _ in 0..SPSA_TRIALS {
        let (slope, _) = noisy.kick(values, SPSA_SHIFT, &mut rng)?;
        total += slope.iter().map(|s| s.abs()).sum::<f64>() / slope.len() as f64;
    }
    let size = total / SPSA_TRIALS as f64;
    let rate = SPSA_FIRST * (SPSA_OFFSET + 1.0).powf(0.602) / size.max(1e-3);
    let mut progress = Progress::new("minimising, steps", SPSA_STEPS as u64);
    let mut steps = 0;
    while steps < SPSA_STEPS {
        if progress::stopped() {
            output.eprintln(format_args!(
                "stopped by Ctrl+C after {steps} steps, so this is where SPSA had got to"
            ));
            break;
        }
        progress.tick(steps as u64);
        let k = steps as f64;
        let shift = SPSA_SHIFT / (k + 1.0).powf(0.101);
        let (slope, value) = noisy.kick(values, shift, &mut rng)?;
        if steps % 10 == 0 {
            row(output, steps, value, values);
        }
        let gain = rate / (k + 1.0 + SPSA_OFFSET).powf(0.602);
        for ((_, value), s) in values.iter_mut().zip(&slope) {
            *value -= gain * s;
        }
        steps += 1;
    }
    let found = noisy.value(values)?;
    let exact = energy(options, source, values)?;
    output.println(format_args!(
        "\nafter {steps} steps at {}\n{}",
        listed(values),
        noisy.compared(found, exact)
    ));
    output
        .println("each row's value is the mean of the two values SPSA measures around that point");
    Ok(())
}

fn noisy_gradient(
    options: &Options,
    source: &str,
    values: &[(String, f64)],
    output: &mut Output,
) -> Result<(), String> {
    let mut noisy = Noisy::new(options, source)?;
    let here = noisy.value(values)?;
    let exact = energy(options, source, values)?;
    let slopes = slopes(options, source, values)?;
    output.println(format_args!(
        "expectation of {} at {}, {} shots per value\n{}\n\ngradient by the parameter shift rule:{:>18}{:>14}",
        noisy.observable,
        listed(values),
        noisy.shots,
        noisy.compared(here, exact),
        "noisy",
        "exact"
    ));
    let mut shifted = Vec::new();
    for (i, slope) in slopes.into_iter().enumerate() {
        let (up, down) = (moved(values, i, SHIFT), moved(values, i, -SHIFT));
        let (_, (difference, error)) = around(noisy.value(&up)?, noisy.value(&down)?);
        let rule = (energy(options, source, &up)? - energy(options, source, &down)?) / 2.0;
        if (rule - slope).abs() > 1e-6 {
            shifted.push(values[i].0.as_str());
        }
        let noisy_slope = format!("{difference:.6} ± {error:.6}");
        output.println(format_args!(
            "  d/d{:<10}{noisy_slope:>40}{slope:>14.6}",
            values[i].0
        ));
    }
    if !shifted.is_empty() {
        output.println(format_args!(
            "note: the shift rule is exact only for an input that is the angle of one rotation, so the noisy slope of {} measures something else",
            shifted.join(", ")
        ));
    }
    Ok(())
}

fn slopes(options: &Options, source: &str, values: &[(String, f64)]) -> Result<Vec<f64>, String> {
    (0..values.len())
        .map(|i| {
            let shifted = |delta: f64| energy(options, source, &moved(values, i, delta));
            Ok((shifted(STEP)? - shifted(-STEP)?) / (2.0 * STEP))
        })
        .collect()
}

fn listed(values: &[(String, f64)]) -> String {
    values
        .iter()
        .map(|(name, value)| format!("{name} = {value:.6}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn minimize(
    options: &Options,
    source: &str,
    values: &mut [(String, f64)],
    output: &mut Output,
) -> Result<(), String> {
    let names: Vec<&str> = values.iter().map(|(name, _)| name.as_str()).collect();
    output.println(format_args!(
        "minimising {} over {}\n",
        options
            .observable
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        names.join(", ")
    ));
    let header: String = names.iter().map(|name| format!("{name:>12}")).collect();
    output.println(format_args!("{:>6}{:>14}{header}", "step", "value"));
    let row = |output: &mut Output, step: usize, value: f64, values: &[(String, f64)]| {
        let columns: String = values.iter().map(|(_, v)| format!("{v:>12.6}")).collect();
        output.println(format_args!("{step:>6}{value:>14.6}{columns}"));
    };
    let (mut first, mut second) = (vec![0.0; values.len()], vec![0.0; values.len()]);
    let mut value = energy(options, source, values)?;
    let mut steps = 0;
    let mut progress = Progress::new("minimising, steps", MAX_STEPS as u64);
    let mut interrupted = false;
    while steps < MAX_STEPS {
        if progress::stopped() {
            interrupted = true;
            break;
        }
        progress.tick(steps as u64);
        let gradient = slopes(options, source, values)?;
        if steps % 25 == 0 {
            row(output, steps, value, values);
        }
        if gradient.iter().map(|g| g * g).sum::<f64>().sqrt() < FLAT {
            break;
        }
        steps += 1;
        for (i, g) in gradient.iter().enumerate() {
            first[i] = 0.9 * first[i] + 0.1 * g;
            second[i] = 0.999 * second[i] + 0.001 * g * g;
            let unbiased = first[i] / (1.0 - 0.9f64.powi(steps as i32));
            let scale = second[i] / (1.0 - 0.999f64.powi(steps as i32));
            values[i].1 -= 0.1 * unbiased / (scale.sqrt() + 1e-12);
        }
        value = energy(options, source, values)?;
    }
    row(output, steps, value, values);
    if interrupted {
        output.eprintln(format_args!(
            "stopped by Ctrl+C after {steps} steps, so this is the lowest value reached so far"
        ));
    }
    output.println(format_args!(
        "\nminimum {value:.6} after {steps} steps at {}",
        listed(values)
    ));
    Ok(())
}

fn variational(options: &Options, file: &SourceFile, output: &mut Output) -> i32 {
    let names = qasm::inputs(&file.text);
    if names.is_empty() {
        output.eprintln("error: --gradient and --minimize vary OpenQASM 3 `input` values, and this program declares none");
        return 1;
    }
    let mut values: Vec<(String, f64)> = names
        .into_iter()
        .map(|name| {
            let start = options.bindings.get(&name).copied().unwrap_or(START);
            (name, start)
        })
        .collect();
    let noisy = options.noisy || options.zne;
    let result = if options.minimize && noisy {
        noisy_minimize(options, &file.text, &mut values, output)
    } else if options.minimize {
        minimize(options, &file.text, &mut values, output)
    } else if noisy {
        noisy_gradient(options, &file.text, &values, output)
    } else {
        energy(options, &file.text, &values).and_then(|value| {
            let gradient = slopes(options, &file.text, &values)?;
            output.println(format_args!(
                "expectation of {}: {value:.6} at {}\n\ngradient:",
                options
                    .observable
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                listed(&values)
            ));
            for ((name, _), slope) in values.iter().zip(gradient) {
                output.println(format_args!("  d/d{name:<10}{slope:>12.6}"));
            }
            Ok(())
        })
    };
    match result {
        Ok(()) => 0,
        Err(reason) => {
            output.eprintln(format_args!("error: {reason}"));
            1
        }
    }
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
    if options.gradient || options.minimize {
        return variational(options, file, output);
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

    if options.submit {
        let shots = options.shots.max(1);
        let mut progress = |line: &str| output.eprintln(line);
        return match provider::submit(
            program,
            &options.device,
            shots,
            SUBMIT_TIMEOUT,
            &mut progress,
        ) {
            Ok(counts) => {
                let note = format!(" on IonQ {}", options.device);
                print_tally(output, "measurement", &note, &counts);
                0
            }
            Err(reason) => {
                output.eprintln(format_args!("error: {reason}"));
                1
            }
        };
    }

    if options.verbose {
        output.eprintln(format_args!(
            "parse {:?}, lower {:?}, optimise {:?}",
            compilation.parse_time, compilation.lower_time, compilation.opt_time
        ));
        output.eprint(format_args!("{}", compilation.stats));
        if let Some(stats) = &compilation.transpiled {
            output.eprintln(format_args!("{stats}"));
        }
        if let Some(stats) = &compilation.reused {
            output.eprintln(stats);
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
        Emit::Qasm2 => match qasm2::emit(program) {
            Ok(text) => text,
            Err(reason) => {
                output.eprintln(format_args!("error: cannot emit OpenQASM 2: {reason}"));
                return 1;
            }
        },
        Emit::Stim => {
            let noise = options.calibration.as_ref().filter(|_| options.noisy);
            if let Some(calibration) = noise {
                if calibration.coherent() {
                    output.eprintln("warning: Stim has no coherent errors, so the calibration's detuning is left out");
                }
                if calibration.decouple {
                    output.eprintln(
                        "warning: Stim has no timing, so the echo pulses from --dd are left out",
                    );
                }
            }
            match stim::emit(program, noise) {
                Ok(text) => text,
                Err(reason) => {
                    output.eprintln(format_args!("error: cannot emit Stim: {reason}"));
                    return 1;
                }
            }
        }
        Emit::Ionq => match provider::ionq(program, &options.device, options.shots.max(1)) {
            Ok(text) => text + "\n",
            Err(reason) => {
                output.eprintln(format_args!("error: cannot write an IonQ job: {reason}"));
                return 1;
            }
        },
        Emit::Pulse | Emit::Schedule => {
            let Some(calibration) = &options.calibration else {
                output.eprintln(format_args!(
                    "error: a pulse schedule needs --calibration for gate times and frequencies"
                ));
                return 1;
            };
            let emitted = if options.emit == Emit::Pulse {
                pulse::emit(program, calibration)
            } else {
                pulse::schedule(program, calibration)
            };
            match emitted {
                Ok(text) => text,
                Err(reason) => {
                    output.eprintln(format_args!("error: cannot schedule pulses: {reason}"));
                    return 1;
                }
            }
        }
        Emit::Qir => match codegen::emit_qir(program) {
            Ok(text) => text,
            Err(reason) => {
                output.eprintln(format_args!("error: cannot emit QIR: {reason}"));
                return 1;
            }
        },
        Emit::Json => codegen::emit_json(program),
        Emit::Circuit => codegen::emit_circuit(program),
        Emit::Quantikz => draw::quantikz(program),
        Emit::Svg => draw::svg(program),
        Emit::Cost => cost::analyse(program, options.calibration.as_ref()).to_string(),
        Emit::Resources => {
            match resources::estimate(program, options.calibration.as_ref(), options.budget) {
                Ok(estimate) => estimate.to_string(),
                Err(reason) => {
                    output.eprintln(format_args!("error: cannot estimate resources: {reason}"));
                    return 1;
                }
            }
        }
        Emit::Rotations => match rotations::compile(program) {
            Ok(rotations) => rotations.to_string(),
            Err(reason) => {
                output.eprintln(format_args!(
                    "error: cannot compile to Pauli product rotations: {reason}"
                ));
                return 1;
            }
        },
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
    if qubits > state::MAX_QUBITS && !exec::scalable(program) {
        if let Some(observable) = &options.observable {
            return if expectation(output, program, observable) {
                0
            } else {
                1
            };
        }
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
    let kernel = match options.bond {
        Some(_) => "matrix product state",
        None => exec::kernel(program),
    };
    output.println(format_args!("kernel:  {kernel}"));
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
    let depth = program.depth();
    if program.num_qubits as usize * depth <= MAX_DRAWN {
        output.print(format_args!("{}", codegen::emit_circuit(program)));
    } else {
        output.println(format_args!(
            "circuit: {} qubits by depth {depth} is too large to draw here, use --emit circuit",
            program.num_qubits
        ));
    }

    let config = ExecConfig {
        shots: options.shots,
        seed,
        keep_state: options.show_state,
    };
    let (outcome, elapsed) = timed(
        || match (&options.calibration, options.noisy, options.bond) {
            (Some(calibration), true, _) => exec::execute_noisy(program, config, calibration),
            (_, _, Some(bond)) => exec::execute_bonded(program, config, bond)
                .unwrap_or_else(|| exec::execute(program, config)),
            _ => exec::execute(program, config),
        },
    );
    if options.noisy {
        output.println("noise:   calibration error rates applied to every gate and measurement");
    }
    if outcome.discarded > 0.0 {
        output.println(format_args!(
            "truncation: bonds were capped at {}, discarding {:.2e} of the weight, so the counts are approximate",
            options.bond.unwrap_or(mps::DEFAULT_BOND),
            outcome.discarded
        ));
    }

    if let Some(done) = outcome.stopped {
        output.eprintln(format_args!(
            "stopped by Ctrl+C after {done} of {} shots, so the counts cover only the shots that finished",
            config.shots.max(1)
        ));
    }
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

    if let (true, Some(calibration)) = (options.mitigate, &options.calibration) {
        match calibration.mitigate(program, &outcome.counts) {
            Some(rows) if !rows.is_empty() => {
                let width = rows.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
                output.println("");
                output.println("mitigated measurement, readout errors undone:");
                for (key, share) in rows {
                    output.println(format_args!("  {key:<width$}  {share:.4}"));
                }
            }
            _ => output.eprintln(format_args!(
                "note: readout mitigation needs between 1 and 20 measured results"
            )),
        }
    }

    if let Some(observable) = &options.observable {
        output.println("");
        expectation(output, program, observable);
        if let (true, Some(calibration)) = (options.zne, &options.calibration) {
            let shots = options.shots.max(1000);
            match observable::extrapolate(program, observable, calibration, shots, seed) {
                Ok((noisy, (zero, spread))) => {
                    let [one, two, three] =
                        noisy.map(|(value, error)| format!("{value:.6} ± {error:.6}"));
                    output.println(format_args!(
                        "noisy expectation at 1x, 2x and 3x noise over {shots} shots: {one}, {two}, {three}"
                    ));
                    output.println(format_args!(
                        "zero noise extrapolation: {:.6} ± {spread:.6}",
                        zero + 0.0
                    ));
                    let bound = observable.bound();
                    if zero.abs() > bound {
                        output.println(format_args!(
                            "  beyond the largest possible value {bound}, so read it as {:.6}",
                            zero.clamp(-bound, bound)
                        ));
                    }
                }
                Err(reason) => output.eprintln(format_args!("error: cannot extrapolate: {reason}")),
            }
        }
    }

    if !elapsed.is_zero() {
        output.println("");
        output.println(format_args!("simulated in {elapsed:.3?}"));
    }
    0
}

fn expectation(output: &mut Output, program: &Program, observable: &Observable) -> bool {
    match observable::expectation(program, observable) {
        Ok(value) => {
            output.println(format_args!(
                "expectation of {observable}: {:.6}",
                value + 0.0
            ));
            true
        }
        Err(reason) => {
            output.eprintln(format_args!(
                "error: cannot compute the expectation: {reason}"
            ));
            false
        }
    }
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
    let mut widest: f64 = 0.0;
    for (key, count) in rows {
        let share = *count as f64 / total as f64;
        widest = widest.max((share * (1.0 - share) / total as f64).sqrt());
        output.println(format_args!("  {key:<width$}  {count:>8}   {share:.4}"));
    }
    if widest > 0.0 {
        output.println(format_args!(
            "  each share is within ±{widest:.4} at one standard error"
        ));
    }
}
