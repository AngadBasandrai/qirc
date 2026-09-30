# Command line

```text
qirc <input.ll | input.qasm> [options]
qirc diff <a> [b] [options]
qirc submit <input> [options]
qirc lsp
qirc explain <code>
qirc surface [--distance 3,5,7] [--error p | --calibration f] [--rounds n] [--shots n] [--seed n]
```

A file whose first statement is `OPENQASM 2.0;` or `OPENQASM 3.0;` is read as OpenQASM, and anything else as QIR in textual LLVM IR.

## Commands

| Command | Does |
| --- | --- |
| `qirc <input>` | compiles the input and runs it, or emits what `--emit` asks for |
| `qirc diff <a> [b]` | checks that `b`, or `a` compiled with the options, behaves exactly like `a` at `-O0`, see [Checking a compile](../checking.md) |
| `qirc submit <input>` | runs the program on IonQ, see [Running on IonQ](../ionq.md) |
| `qirc lsp` | serves diagnostics to an editor over the language server protocol, see [Editors](../editors.md) |
| `qirc surface` | simulates surface code memory and compares its logical error with the resource model, see [Surface code simulation](../surface.md) |
| `qirc explain <code>` | explains an error code such as `QIR0300`, see [Diagnostics](diagnostics.md) |

## Options

| Option | Meaning | Default |
| --- | --- | --- |
| `--emit <kind>` | what to produce, from the table below | `run` |
| `-O<n>` | optimisation level 0 to 3, see [Optimisation](../optimisation.md) | 1 |
| `--shots <n>` | sample `n` measurement outcomes | |
| `--seed <n>` | seed the random number generator | from the clock |
| `--no-state` | do not print the final state vector | |
| `-o <path>` | write the emitted output to a file | |
| `--color <when>` | `auto`, `always` or `never` | `auto` |
| `--gates <set>` | target gate set: `rz-sx-cx`, `rz-ry-cz` or a list such as `rz,sx,cx`; `--basis` is the same | |
| `--exclude <list>` | leave gates out of the full QIR set, such as `h,t` | |
| `--resynth <n>` | replace runs of gates by at most `n` gates, 1 to 6 | 1 from `-O2` |
| `--cost <model>` | what resynthesis minimises: `gates`, `cx` or `ibm` | `gates` |
| `--epsilon <e>` | approximate every gate the target set cannot express to within `e`, see [Targeting hardware](../targeting.md) | |
| `--budget <p>` | total failure probability for `--emit resources` | 0.001 |
| `--coupling <map>` | route onto `line:N`, `ring:N`, `grid:RxC`, `full:N` or an edge list such as `0-1,1-2,2-3` | |
| `--calibration <file>` | read a [calibration](calibration.md) | |
| `--relabel` | remove swaps near the end by renaming qubits | on when routing |
| `--reuse` | reset measured qubits and reuse them | |
| `--noisy` | simulate with the calibration's errors | |
| `--mitigate` | undo the calibration's readout errors in the counts | |
| `--zne` | extrapolate `--observable` to zero noise | |
| `--dd` | fill idle windows with echo pulses, timed from the calibration | |
| `--observable <p>` | print the exact expectation of a Pauli sum such as `'Z0 Z1 + 0.5 X2'` | |
| `--bind <list>` | values for OpenQASM 3 inputs, such as `theta=0.3,phi=1.2`, see [Variational circuits](../variational.md) | |
| `--gradient` | print how the `--observable` changes with every input, by the parameter shift rule under `--noisy` or `--zne` | |
| `--minimize` | vary the inputs to minimise the `--observable`, by SPSA under `--noisy` or `--zne` | |
| `--bond <n>` | simulate as a matrix product state with bonds up to `n` | 32 above 30 qubits |
| `--target <name>` | IonQ backend for `submit` and `--emit ionq`, such as `qpu.aria-1` | `simulator` |
| `--verify-each` | run the IR verifier after lowering and after every pass | |
| `-v`, `--verbose` | print pipeline timings and pass statistics | |
| `-h`, `--help` | show the usage | |

## Emit kinds

| Kind | Produces |
| --- | --- |
| `run` | simulates the program and prints the state, counts and recorded output |
| `ir` | qirc's own intermediate representation, block by block |
| `qasm3` | OpenQASM 3 |
| `qasm2` | OpenQASM 2 with `qelib1.inc` gates, for straight line programs and simple feedback |
| `qir` | QIR as textual LLVM IR, also spelled `llvm` |
| `json` | the program as JSON |
| `stim` | a [Stim](https://github.com/quantumlib/Stim) circuit, for Clifford programs |
| `circuit` | an ASCII circuit diagram |
| `quantikz` | a LaTeX `quantikz` diagram, also spelled `latex` |
| `svg` | an SVG circuit diagram |
| `cost` | T gates, two qubit gates, gates, depth and live qubits for every path |
| `resources` | a surface code resource estimate, see [Resource estimation](../resources.md) |
| `rotations` | the program as Pauli product rotations and measurements with its T depth, see [Pauli product rotations](../rotations.md) |
| `schedule` | the program laid out in time from the calibration's durations |
| `pulse` | OpenQASM 3 with OpenPulse calibrations, also spelled `openpulse` |
| `ionq` | the job `qirc submit` would send to IonQ |
| `check` | nothing, only the diagnostics |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | success, or `diff` found the programs equivalent |
| 1 | the program has errors, or `diff` found them different |
| 2 | bad arguments, or `diff` was inconclusive |
