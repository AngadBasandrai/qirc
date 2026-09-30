use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use compiler::cost::{self, Tally};
use compiler::diag::{Severity, SourceFile};
use compiler::driver::{self, Compilation, Options, Output};
use compiler::observable::{self, Observable};
use compiler::simulator::exec::{self, ExecConfig};
use compiler::simulator::state;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

create_exception!(qirc, CompileError, PyException);

const FLAGS: [(&str, &str); 14] = [
    ("opt", "-O"),
    ("gates", "--gates"),
    ("exclude", "--exclude"),
    ("resynth", "--resynth"),
    ("cost", "--cost"),
    ("coupling", "--coupling"),
    ("relabel", "--relabel"),
    ("calibration", "--calibration"),
    ("reuse", "--reuse"),
    ("noisy", "--noisy"),
    ("epsilon", "--epsilon"),
    ("budget", "--budget"),
    ("dd", "--dd"),
    ("bind", "--bind"),
];

fn flags(options: Option<&Bound<'_, PyDict>>) -> PyResult<Vec<String>> {
    let mut args = vec!["--color".to_string(), "never".to_string()];
    let Some(options) = options else {
        return Ok(args);
    };
    for (key, value) in options.iter() {
        let key: String = key.extract()?;
        if key == "name" {
            continue;
        }
        let Some(&(_, flag)) = FLAGS.iter().find(|(name, _)| *name == key) else {
            return Err(PyTypeError::new_err(format!(
                "unexpected keyword argument `{key}`, expected one of name, opt, gates, exclude, resynth, cost, coupling, relabel, calibration, reuse, noisy, epsilon, budget, dd or bind"
            )));
        };
        if value.is_none() {
            continue;
        }
        if let Ok(on) = value.extract::<bool>() {
            if on {
                args.push(flag.to_string());
            }
            continue;
        }
        if let Ok(bound) = value.cast::<PyDict>() {
            let pairs = bound
                .iter()
                .map(|(name, number)| Ok(format!("{}={}", name.str()?, number.extract::<f64>()?)))
                .collect::<PyResult<Vec<String>>>()?;
            args.push(flag.to_string());
            args.push(pairs.join(","));
            continue;
        }
        let value = value.str()?.to_string();
        if flag == "-O" {
            args.push(format!("-O{value}"));
        } else {
            args.push(flag.to_string());
            args.push(value);
        }
    }
    Ok(args)
}

fn prepare(
    source: &str,
    mut args: Vec<String>,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<(Options, SourceFile)> {
    let name: String = match options.map(|o| o.get_item("name")).transpose()?.flatten() {
        Some(value) => value.extract()?,
        None => "program.ll".into(),
    };
    args.push(name.clone());
    args.extend(flags(options)?);
    let options = driver::parse_args(&args).map_err(PyValueError::new_err)?;
    Ok((options, SourceFile::new(name, source)))
}

fn warn(py: Python<'_>, text: &str) -> PyResult<()> {
    let text = text.trim_end();
    if !text.is_empty() {
        py.import("warnings")?.call_method1("warn", (text,))?;
    }
    Ok(())
}

fn compiled(py: Python<'_>, file: &SourceFile, options: &Options) -> PyResult<Compilation> {
    let target = options.target();
    let compilation =
        py.detach(|| driver::compile_for(&file.text, options.opt_level, false, &target));
    if let Some(violation) = compilation.stats.violations.first() {
        return Err(PyRuntimeError::new_err(format!(
            "internal error: verifier: {violation}"
        )));
    }
    let rendered = |severity: Severity| {
        compilation
            .diagnostics
            .iter()
            .filter(|d| d.severity == severity)
            .map(|d| d.render(file))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let errors = rendered(Severity::Error);
    if !errors.is_empty() {
        return Err(CompileError::new_err(errors.trim_end().to_string()));
    }
    warn(py, &rendered(Severity::Warning))?;
    Ok(compilation)
}

fn tally<'py>(py: Python<'py>, t: &Tally) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("t", t.t)?;
    dict.set_item("cx", t.cx)?;
    dict.set_item("gates", t.gates)?;
    dict.set_item("depth", t.depth)?;
    dict.set_item("live", t.live)?;
    dict.set_item("success", t.success)?;
    Ok(dict)
}

#[pyfunction]
#[pyo3(signature = (source, *, emit = "qir", **options))]
fn compile(
    py: Python<'_>,
    source: &str,
    emit: &str,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<String> {
    let (options, file) = prepare(source, vec!["--emit".into(), emit.into()], options)?;
    let (code, output) = py.detach(|| {
        let mut output = Output::default();
        let code = driver::run_source(&options, &file, None, &mut output);
        (code, output)
    });
    if code != 0 {
        return Err(CompileError::new_err(output.stderr.trim_end().to_string()));
    }
    warn(py, &output.stderr)?;
    Ok(output.stdout)
}

#[pyfunction]
#[pyo3(signature = (source, *, shots = 1000, seed = None, **options))]
fn run(
    py: Python<'_>,
    source: &str,
    shots: u64,
    seed: Option<u64>,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<BTreeMap<String, u64>> {
    if shots == 0 {
        return Err(PyValueError::new_err("shots must be at least 1"));
    }
    let (options, file) = prepare(source, Vec::new(), options)?;
    let compilation = compiled(py, &file, &options)?;
    let program = compilation.program;
    if program.num_qubits as usize > state::MAX_QUBITS && !exec::scalable(&program) {
        return Err(PyValueError::new_err(format!(
            "this program needs {} qubits, the simulator supports at most {}",
            program.num_qubits,
            state::MAX_QUBITS
        )));
    }
    let seed = seed.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
    });
    let config = ExecConfig {
        shots,
        seed,
        keep_state: false,
    };
    let outcome = py.detach(|| match (&options.calibration, options.noisy) {
        (Some(calibration), true) => exec::execute_noisy(&program, config, calibration),
        _ => exec::execute(&program, config),
    });
    if outcome.aborted {
        return Err(PyRuntimeError::new_err(
            "execution did not terminate within the step limit",
        ));
    }
    Ok(outcome.counts)
}

#[pyfunction]
#[pyo3(signature = (source, other = None, *, **options))]
fn diff(
    py: Python<'_>,
    source: &str,
    other: Option<&str>,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<String> {
    let (options, file) = prepare(source, vec!["diff".into()], options)?;
    let other = other.map(|text| SourceFile::new("other.ll", text));
    let output = py.detach(|| {
        let mut output = Output::default();
        driver::run_source(&options, &file, other.as_ref(), &mut output);
        output
    });
    let verdict = output.stdout.split(':').next().unwrap_or_default();
    if ["equivalent", "different", "inconclusive"].contains(&verdict) {
        return Ok(verdict.to_string());
    }
    Err(CompileError::new_err(output.stderr.trim_end().to_string()))
}

#[pyfunction]
#[pyo3(signature = (source, observable, *, zne = false, shots = 4000, seed = 1, **options))]
fn expectation(
    py: Python<'_>,
    source: &str,
    observable: &str,
    zne: bool,
    shots: u64,
    seed: u64,
    options: Option<&Bound<'_, PyDict>>,
) -> PyResult<f64> {
    let observable = Observable::parse(observable).map_err(PyValueError::new_err)?;
    let (options, file) = prepare(source, Vec::new(), options)?;
    let compilation = compiled(py, &file, &options)?;
    let program = compilation.program;
    py.detach(|| match (&options.calibration, zne, options.noisy) {
        (Some(c), true, _) => {
            observable::extrapolate(&program, &observable, c, shots, seed)
                .map(|(_, (zero, _))| zero.clamp(-observable.bound(), observable.bound()))
        }
        (Some(c), false, true) => {
            observable::noisy_expectation(&program, &observable, c, shots, seed).map(|(value, _)| value)
        }
        _ => observable::expectation(&program, &observable),
    })
    .map_err(PyValueError::new_err)
}

#[pyfunction(name = "cost")]
#[pyo3(signature = (source, *, **options))]
fn cost_report<'py>(
    py: Python<'py>,
    source: &str,
    options: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyDict>> {
    let (options, file) = prepare(source, Vec::new(), options)?;
    let compilation = compiled(py, &file, &options)?;
    let report = cost::analyse(&compilation.program, options.calibration.as_ref());

    let paths = PyList::empty(py);
    for path in &report.paths {
        let entry = tally(py, &path.tally)?;
        entry.set_item("blocks", &path.labels)?;
        entry.set_item("again", &path.again)?;
        paths.append(entry)?;
    }
    let result = PyDict::new(py);
    result.set_item("paths", paths)?;
    result.set_item("worst", tally(py, &report.worst)?)?;
    result.set_item("truncated", report.truncated)?;
    Ok(result)
}

#[pymodule]
#[pyo3(name = "_qirc")]
fn qirc(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("CompileError", m.py().get_type::<CompileError>())?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(compile, m)?)?;
    m.add_function(wrap_pyfunction!(run, m)?)?;
    m.add_function(wrap_pyfunction!(diff, m)?)?;
    m.add_function(wrap_pyfunction!(cost_report, m)?)?;
    m.add_function(wrap_pyfunction!(expectation, m)?)?;
    Ok(())
}
