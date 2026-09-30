# qirc

A compiler and state vector simulator for [QIR](https://github.com/qir-alliance/qir-spec), the LLVM based intermediate representation used by Q#, PyQIR and other quantum toolchains.

qirc reads QIR `.ll` files and OpenQASM 2 and 3 programs, checks them against the QIR profile they declare, optimises the circuit, and then either simulates it or emits OpenQASM 3, QIR or JSON. It can also lower a circuit to a hardware gate set and route it onto a limited qubit connectivity map.

Try it in the browser at <https://angadbasandrai.github.io/qirc/>, and read the guide at <https://angadbasandrai.github.io/qirc/book/>.

```text
$ qirc tests/corpus/base_profile_bell.ll --shots 1000
source:  tests/corpus/base_profile_bell.ll
kernel:  AVX2 + FMA (4 x f64 lanes)
program: 2 qubits, 2 results, 2 gates, depth 2, profile base_profile

q0: H-*-M---
q1: --+---M-

State vector (2 qubits, 4 amplitudes):
  |00>  0.707107 + 0.000000i   p = 0.500000
  |11>  0.707107 + 0.000000i   p = 0.500000

  P(q0 = 1) = 0.500000
  P(q1 = 1) = 0.500000

output recording:
  TUPLE 2
  RESULT r0
  RESULT r1

returned over 1000 shots:
  (0, 0)       515   0.5150
  (1, 1)       485   0.4850
  each share is within ±0.0158 at one standard error

measurement over 1000 shots (sampled from the final state):
  00       515   0.5150
  11       485   0.4850
  each share is within ±0.0158 at one standard error

simulated in 851.600µs
```

## Installing

Each release on GitHub carries a prebuilt `qirc` for Linux (x86_64 and arm64), macOS (Intel and Apple silicon) and Windows, and Python wheels for the same platforms.

```text
cargo install qirc-compiler
pip install qirc
```

The crate is published as `qirc-compiler` because `qirc` was taken on crates.io. It installs the `qirc` command, and the library is still used as `qirc`.

## Building

Requires Rust 1.88 or newer.

```text
cargo build --release
./target/release/qirc --help
```

The only direct dependency is `num-complex`.

## Usage

```text
qirc <input.ll | input.qasm> [options]
qirc diff <a> [b] [options]
qirc submit <input> [options]
qirc lsp
qirc explain <code>
qirc surface [--distance 3,5,7] [--error p | --calibration f]

  --emit <kind>     run | ir | qasm3 | qasm2 | stim | pulse | schedule | ionq | qir | json
                    | circuit | quantikz | svg | cost | resources | rotations | check
                    (default: run)
  -O<n>             optimisation level 0 to 3                         (default: 1)
  --gates <set>     target gate set: rz-sx-cx, rz-ry-cz or a list such as rz,sx,cx
  --exclude <list>  leave gates out of the target set, such as h,t
  --resynth <n>     replace runs of gates by at most n gates, 1 to 6
  --cost <model>    what resynthesis minimises: gates, cx or ibm
  --epsilon <e>     approximate gates the target set cannot express to within e
  --budget <p>      total failure probability for --emit resources (default 0.001)
  --coupling <map>  route onto line:N, ring:N, grid:RxC, full:N or 0-1,1-2,...
  --calibration <f> route by device error rates and estimate success
  --relabel         remove swaps at the end of the program by permuting qubits
  --reuse           reset measured qubits and reuse them to need fewer qubits
  --noisy           simulate with the error rates from --calibration
  --bond <n>        cap matrix product state bonds at n (default 32 above 30 qubits)
  --target <name>   IonQ target for submit and --emit ionq (default simulator)
  --mitigate        undo the readout errors from --calibration in the counts
  --zne             extrapolate the --observable to zero noise from 1x, 2x and 3x noise
  --dd              fill idle windows with echo pulses, timed from --calibration
  --observable <p>  print the exact expectation of a Pauli sum such as 'Z0 Z1 + 0.5 X2'
  --bind <list>     values for OpenQASM 3 inputs, such as theta=0.3,phi=1.2
  --gradient        print how the --observable changes with every input
  --minimize        vary the inputs to minimise the --observable
  --shots <n>       sample n measurement outcomes
  --seed <n>        seed the random number generator
  --no-state        do not print the final state vector
  --verify-each     check the IR after lowering and after every pass
  -o <path>         write emitted output to a file
  --color <when>    auto, always or never
  -v                print pipeline timings and pass statistics
```

## Examples

| File | Shows |
| --- | --- |
| `examples/ghz.ll` | a 22 qubit GHZ state written as a loop |
| `examples/repeat_until_success.ll` | a loop that exits on a measurement |
| `examples/redundant.ll` | gates the optimiser removes |
| `examples/bad_profile.ll` | a Base Profile program that breaks its profile |
| `examples/small.ll` | qubits passed as entry point parameters |

## Pipeline

| Stage | Source | Produces |
| --- | --- | --- |
| Lex | `lex.rs` | tokens with byte spans |
| Parse | `parse.rs`, `qasm.rs` | LLVM IR AST, or IR straight from OpenQASM |
| Inline | `inline.rs` | AST with helper functions expanded |
| Lower | `lower.rs` | quantum IR |
| Validate | `sema.rs` | profile and range diagnostics |
| Optimise | `opt.rs` | rewritten IR |
| Target | `transpile.rs`, `synth.rs`, `kak.rs`, `route.rs` | basis gates on a coupling map |
| Emit | `codegen.rs`, `cost.rs` | QASM 3, QIR, JSON, circuit diagrams, cost reports |
| Execute | `simulator/` | state vector and shot counts |

`verify.rs` checks block structure, single assignment and reaching definitions over the dominator tree. `--verify-each` runs it after lowering and after every pass.

Errors point at the source:

```text
error[QIR0300]: the Base Profile forbids branching
  --> teleport.ll:11:1
   |
11 | entry:
   | ^^^^^^ this program has more than one basic block
   |
   = note: branching needs the Adaptive Profile
```

`qirc explain QIR0300` prints a longer explanation of any code, and a mistyped option, emit kind or gate name gets a suggestion, as in ``unknown emit kind `qsam3`, did you mean `qasm3`?``.

## OpenQASM input

A file whose first statement is `OPENQASM 2.0;` or `OPENQASM 3.0;` is read as OpenQASM instead of LLVM IR, so every command works on it: simulate it, optimise and route it, `qirc diff` it against a QIR program, or turn it into QIR with `--emit qir`.

```text
$ qirc bell.qasm --emit qir -O2 --gates rz-sx-cx
```

It reads `qubit`, `bit`, `qreg` and `creg` declarations, the standard gates of `stdgates.inc` and `qelib1.inc` including `u`, `u2`, `u3`, `cp`, `crz`, `cu`, `rzz`, `rxx`, `ryy`, `ccx` and `cswap`, gate definitions with parameters, gates applied to whole registers, all three ways of writing a measurement, `reset`, `barrier`, and `if` on a bit, its negation or a register compared with a number, with an `else`. Angles can use `pi`, arithmetic and functions such as `sin` and `sqrt`. `const int n = 5;` and other `int`, `uint`, `float` and `angle` values set once can size registers and appear in indices and angles. A `for` loop over a range such as `[0:n - 1]` or `[0:2:8]`, or over a set such as `{0, 3}`, is unrolled at compile time with its variable usable in indices like `q[i + 1]` and angles like `pi / 2 ** i`, and a `while` loop on a measured bit becomes a loop in the program, so a repeat until success reads as written. A QFT written with nested loops is proven equivalent by `qirc diff` to the same circuit written out gate by gate, and the `for` and `while` loops Qiskit's OpenQASM 3 exporter writes compile and run. Classical values that change, `bool`, `output`, `break` and subroutines are reported as unsupported. The classical bits are recorded as one array at the end, and the program uses the Adaptive Profile only if it branches, resets or reuses a measured qubit. Errors point at the source like any other diagnostic, and nesting, expression depth and the size of expanded gate definitions are bounded, so hostile input fails with a message.

Every circuit Qiskit exports for the benchmarks, in both versions, `qirc diff` finds identical to the same circuit built as QIR.

`--emit qasm2` writes OpenQASM 2 with `qelib1.inc` gates for tools that only read that version. OpenQASM 2 has no `else`, loops or classical variables, so it covers straight line programs and a branch on one measurement to a block of gates, which becomes `if(c1==1) x q[2];` with one single bit register per result, as teleportation needs. Anything else is refused with a pointer to `--emit qasm3`. Every corpus program that fits is accepted by Qiskit's OpenQASM 2 parser and reads back into qirc with the same outcome probabilities.

`--emit stim` writes a Clifford program as a [Stim](https://github.com/quantumlib/Stim) circuit for error correction tools. Rotations by quarter turns become `S`, `SQRT_X` or `SQRT_Y` gates, measurements become `M` and resets `R`, and an X, Y or Z gate that depends on one measurement becomes feedback such as `CX rec[-1] 2`. Stim numbers measurements in the order they happen, so after routing the records can come in a different order from the result numbers. With `--noisy` and a calibration the circuit carries the same noise the simulator uses: `DEPOLARIZE1` and `DEPOLARIZE2` after each gate, `M(p)` for readout errors, `X_ERROR` after a reset, and `PAULI_CHANNEL_1` for idle decoherence from T1 and T2. On 200 random programs, half with feedback and half with noise on `examples/line5.cal`, Stim's samples match qirc's to within 0.007 in total variation.

## Variational circuits

`input float theta;` or `input angle[32] theta;` declares a value that is given at compile time with `--bind theta=0.3`, so one file describes a whole family of circuits, and an input without a value is reported with the `--bind` that would fix it.

```text
$ qirc ansatz.qasm --observable "Z0 Z1 + 0.5 X0 + 0.5 X1" --minimize
minimising Z0 Z1 + 0.5 X0 + 0.5 X1 over theta, phi
...
minimum -1.414214 after 262 steps at theta = 1.570799, phi = -2.356194
```

`--gradient` prints how the `--observable` changes with each input at the bound values, from central differences of exact expectation values with a step of 1e-5, which agree with the analytic derivative to about 1e-9. The parameter shift rule is what hardware needs, where every value is sampled, but on an exact simulator it adds nothing and it breaks when an input feeds more than one gate or an expression. `--minimize` runs Adam from the bound values, or 0.1 for an input without one, until the gradient vanishes. On a two qubit ansatz of two `ry` rotations and a CNOT, it reaches -1.414214, the exact ground energy of `Z0 Z1 + 0.5 X0 + 0.5 X1`, in 262 steps and 0.7 seconds. From Python, every function takes `bind={"theta": 0.3}`.

With `--noisy` or `--zne` and a calibration, both run as they would on hardware, where every value is sampled with the calibration's errors, at least 1000 shots each. `--minimize` switches to SPSA, which moves every input at once by a random plus or minus step and measures only two values per step whatever the number of inputs, with step sizes that shrink as Spall recommends and a learning rate set from the first few slopes. It prints each tenth step with the standard error of its value and, at the end, the noisy value at the inputs found beside the exact value there without noise. `--gradient` uses the parameter shift rule, half the difference of the values a quarter turn either side, with its standard error beside the exact slope, and notes any input for which the rule does not hold. `--zne` extrapolates every value to zero noise first, at three times the shots.

```text
$ qirc ansatz.qasm --observable "Z0 Z1 + 0.5 X0 + 0.5 X1" --minimize --noisy --calibration noise.cal --seed 5
minimising Z0 Z1 + 0.5 X0 + 0.5 X1 over theta, phi by SPSA with the calibration's noise, 1000 shots per value
...
after 150 steps at theta = 1.579071, phi = -2.369776
  noisy value               -1.373005 ± 0.008590
  exact value without noise -1.414071
```

With a 3% CNOT error, SPSA finds inputs within 0.0002 of the true ground energy in 2 seconds, although the device can only show -1.373 there. With `--zne` the same run reports -1.427 ± 0.052 against an exact -1.405, so extrapolation recovers the energy within its error bar.

## Lowering

The frontend parses the subset of textual LLVM IR that QIR producers emit: typed and opaque pointers, `inttoptr` and `getelementptr` constant expressions, parameter attributes, attribute groups, metadata, `phi`, `switch`, varargs calls, packed structs and quoted identifiers.

Lowering first tries to evaluate the program's classical control flow at compile time. Loops over constant ranges are unrolled, `getelementptr` over a global array of qubit ids resolves to a qubit, helper functions are interpreted, and arithmetic folds to constants. Values that depend on a measurement are kept as instructions. The program only keeps its control flow graph when a branch actually depends on a measurement, as in teleportation or repeat until success loops.

Recursive functions are expanded too. Functions that call each other in a cycle are first merged, each absorbing the others' bodies, and a function whose calls to itself are then all tail calls becomes a loop before anything else runs, with a `phi` for each parameter that changes between calls, so recursion that stops on a measurement result, such as a repeat until success written as a function that calls itself again after a failure, compiles to the same loop a hand written version would. Any other recursion is interpreted at compile time when its depth is known, up to 10,000 calls deep; the compiler runs on its own thread with a large stack so that depth is safe in debug builds too. Recursion that is neither a tail call nor bounded at compile time is refused with an error at the call.

This is what lets Q# style output compile. A loop like

```llvm
header:
  %i = phi i64 [ 0, %entry ], [ %next, %body ]
  %more = icmp slt i64 %i, 3
  br i1 %more, label %body, label %measure
body:
  %ctrl.ptr = getelementptr [4 x %Qubit*], [4 x %Qubit*]* @qubits, i64 0, i64 %i
  ...
```

becomes

```text
h q0
cx q0, q1
cx q1, q2
cx q2, q3
```

## Profiles

| Profile | Branching | Reading results |
| --- | --- | --- |
| Base | no | no |
| Adaptive | yes | yes |
| Unrestricted | yes | yes |

The profile comes from the `qir_profiles` attribute on the entry point. Qubit and result counts come from `required_num_qubits` and `required_num_results`, and both older `num_required_*` spellings are accepted.

## Optimisation

| Level | Passes |
| --- | --- |
| `-O0` | none |
| `-O1` | identity removal, inverse cancellation, rotation merging, constant folding, dead code elimination |
| `-O2` | `-O1` plus peephole rewrites, phase folding, phase teleportation and CFG simplification, repeated until nothing changes, then window resynthesis |
| `-O3` | `-O2` plus single qubit gate fusion |

Cancellation, merging and fusion look past gates that commute with the pair, so `rz` on a control qubit cancels across a CNOT. Commutation is checked on the exact matrices of the gates involved.

Phase folding tracks which parity of path variables each qubit holds as CNOTs, X gates and swaps move values around, starting a new variable at every H. Two Z rotations (`rz`, `r1`, `t`, `s`, `z` and their inverses) that act on the same parity are merged into one at the place of the first, however far apart they are, and a rotation on a constant parity is a global phase and is dropped. Diagonal gates such as CZ leave the parities alone, and any other gate, a measurement or a reset ends the region. On Clifford+T adders this removes up to three quarters of the T gates that the other passes leave, for example 34 to 8 on a 3 bit VBE adder.

Phase teleportation, from Kissinger and van de Wetering, finds rotations that act on the same parity even when Hadamards separate them. Each straight run of Clifford gates and rotations becomes a ZX diagram in which every rotation that is not a multiple of a quarter turn is a variable, and the diagram is simplified with spider fusion, identity removal, local complementation, pivoting, and the fusion of phase gadgets with the same targets. Two variables that end up on the same spider or the same gadget can be moved onto one gate with their signs, so one rotation takes the sum and the other is removed. The diagram is only used to find these pairs: no circuit is extracted from it, so every other gate stays where it was. On eleven standard Clifford+T benchmark circuits this matches PyZX exactly, taking the 988 T gates left after phase folding down to 938, with `adder_8` going from 215 to 173 and `mod5_4` from 16 to 8, and `qirc diff` proves every result of up to 20 qubits equivalent.

Window resynthesis multiplies out every run of gates on one or two qubits and replaces the run with anything shorter that has the same matrix up to global phase. By default the replacement must be a single gate, so `t t` becomes `s` and three alternating CNOTs become `swap`. `--resynth n` allows replacements of up to `n` gates: single qubit runs use exact Euler angles, and two qubit runs use a meet in the middle search over the target gate set. Every replacement is checked against the original matrix before it is applied.

With `--resynth 2` or more, or any `--cost`, two qubit runs are also rebuilt from their KAK decomposition with the fewest CNOTs the matrix needs: none for a product of single qubit gates, one when it is locally a CNOT, two when one of its three interaction coordinates is zero, and three otherwise. The result can be longer than `n` gates and is kept only when it is cheaper than the run it replaces. In the same mode the window grows to 96 gates: the longest two qubit run gets one KAK attempt, and a three qubit run with more than 21 two qubit gates is rebuilt by quantum Shannon decomposition. That splits the matrix around its top qubit with a cosine sine decomposition into multiplexed rotations and four two qubit blocks, builds the first three blocks only up to a diagonal so each needs two CNOTs, carries each diagonal into the next block, and needs at most 21 CNOTs in total.

`--cost` sets what cheaper means. `gates` counts gates and breaks ties on two qubit gates, `cx` counts two qubit gates first, and `ibm` charges 10 for a two qubit gate, 1 for other single qubit gates and nothing for `rz`, `s`, `t` and other Z rotations, which IBM hardware applies as frame changes.

On `tests/corpus/pyqir_simple.ll`, `-O3` takes the circuit from 12 gates at depth 7 to 8 gates at depth 4.

## Targeting hardware

`--gates` rebuilds every gate from a target set: a preset such as `rz-sx-cx` for IBM style devices or `rz-ry-cz`, or any list of gates such as `rx,ry,cy`. `--exclude` removes gates from the full QIR set instead, and `--basis` is kept as a name for `--gates`. Multi qubit gates reduce to CNOTs and single qubit matrices first. The CNOT is then mapped onto whichever entangler the set has, and each single qubit matrix is rebuilt from Euler angles over two rotation axes, from one axis plus a fixed gate, or by an exact search over fixed gates such as `h,s,t`. A gate the set cannot express exactly is reported and left as written. A Toffoli that a later identical Toffoli undoes, with only gates in between that use its three qubits as controls or only add phases to them, is lowered together with its partner as a pair of relative phase Toffolis, each 3 CNOTs and 4 T gates instead of 6 and 7, because the phases the two halves add cancel. A six control X gate built from a ladder of Toffolis on ancillas drops from 63 T gates and 54 CNOTs to 39 and 30 on `h,s,t,cx`. On 600 random programs full of such pairs, with phases, rotations and conflicting gates in between, `qirc diff` finds every result equivalent.

`--epsilon e` makes a finite set such as `h,s,t,cx` usable for any angle. Every gate the set cannot express exactly is rebuilt from H, S and T to within e in operator norm, its Euler rotations sharing the precision. Each rotation is found as in Ross and Selinger's gridsynth: the candidates are exact Clifford+T unitaries whose top left entry lies in a thin region around the rotation, drawn from the grid of the ring Z[√2], and a candidate is kept once the norm equation for its other entry can be solved by factoring over Z[√2] and Z[ω]. The T count grows as about 3 log2(1/e), and qirc reaches e down to about 1e-10: below about 1e-7 the region is thinner than a double can resolve next to 1, so it is searched in double-double arithmetic, about 106 bits, and the norm equations are factored over 128 bit integers. A rotation takes milliseconds at 1e-8, about 0.4 seconds at 1e-9 and 0.5 seconds at 1e-10, with 102 T gates at 1e-10. A circuit with an `rz(0.3)` and a controlled `rz(0.45)` needs 82 T gates at 1e-2 and 156 at 1e-4.

```text
$ qirc tests/corpus/base_profile_bell.ll --exclude h,cx --emit ir
  x q0
  ry(-1.5707963267948966) q0
  x q1
  ry(-1.5707963267948966) q1
  cz q0, q1
  x q1
  ry(-1.5707963267948966) q1
```

`--coupling` inserts SWAPs so that every two qubit gate lands on a physical edge, and remaps measurements so results come back under their original labels. The router works on the dependency graph of each block: gates whose qubits are adjacent run as soon as they are ready, and otherwise it picks the SWAP that most shortens the blocked gates plus the next 20, weighted by half and fading with distance, with a small penalty on qubits that were just swapped. A SWAP on a pair that has just run a two qubit gate is preferred, because resynthesis can merge the two. The starting layout comes from routing the circuit forwards and backwards twice from up to sixteen starting points, with a little random tie breaking in all but the first, and the router keeps the result with the lowest estimated CNOT count, counting a SWAP that resynthesis can merge as one CNOT instead of three. After routing, the program is rebuilt in the gate set and resynthesised again. Gates on three or more qubits are split into one and two qubit gates before routing when no gate set is given.

`--relabel` removes each SWAP near the end of the program by renaming the qubits of everything after it, measurements included, so recorded results are unchanged and only the final state comes out permuted. It is on whenever the program is routed, `-v` prints the final layout, and `qirc diff` then compares outcome probabilities and recorded values but not final states.

`--reuse` reorders each run of quantum operations to finish the qubits it has started before touching new ones, then packs qubit lifetimes onto as few wires as it can and resets a wire before it is reused. Resetting a qubit that is no longer used cannot change what later measurements see, so the outcome distribution is unchanged, and a Base Profile program becomes Adaptive because it now resets qubits. Bernstein Vazirani on 9 qubits runs on 2. `qirc diff` compares outcome probabilities only, as after routing.

`--calibration file` reads device error rates, one per line as `cx a b error`, `single q error` or `readout q error`, and routes onto the coupling map its `cx` lines describe unless `--coupling` is also given. The router then measures distance by the error of each coupler, prefers SWAPs on good couplers and keeps the layout with the highest estimated success probability, and `--emit cost` adds a `success` column with the product of one minus the error of every operation on each path. `examples/line5.cal` describes a five qubit line with one poor coupler.

A calibration can also give durations in nanoseconds as `time cx a b ns`, `time single q ns` and `time readout q ns`, and relaxation and dephasing times in microseconds as `t1 q us` and `t2 q us`. With durations, `--emit cost` schedules every operation as early as its qubits allow and adds a `time us` column, and the success estimate also charges each qubit for the time it waits between operations, using the Pauli twirl of amplitude and phase damping: X and Y each with probability (1 - e^(-t/T1))/4 and Z with (1 - e^(-t/T2))/2 minus that. A qubit is only charged once it has been used, since the ground state does not decay. `--noisy` applies the same idle errors during simulation, keeping a clock per qubit in every shot.

`--mitigate` undoes readout errors in the measurement counts, for a noisy simulation or for counts from a device described by the same calibration. Each result bit's flip matrix is inverted and applied across the distribution, negative quasi probabilities are clipped and the rest is renormalised. On a 5 qubit GHZ state with readout errors of 5 to 15 percent, 59 percent of raw shots are correct and the mitigated distribution puts 0.499 and 0.492 on the two correct outcomes.

`--zne` with `--observable` and `--calibration` runs zero noise extrapolation. The program is simulated with the calibration's errors at one, two and three times their rates, with T1 and T2 shortened to match, the expectation value is averaged over the final state of every shot, and a quadratic through the three points is extended to zero noise, which is 3 E1 - 3 E2 + E3. For `Z0 Z4` on a 5 qubit GHZ state on `examples/line5.cal`, the exact value is 1, one times noise gives 0.938 and the extrapolation gives 0.993. In a program with branches or loops every block starts from the same layout, and a block that jumps elsewhere swaps its qubits back before the jump, so each successor sees the layout it expects.

A calibration line `detuning q kHz` gives a qubit a frequency offset, so it turns about Z by 2π times the offset for as long as it waits between operations. Unlike T1 and T2 this error is coherent, and `--noisy` applies it as a rotation. `--dd` removes it with dynamical decoupling: every idle window long enough for two X pulses gets them at a quarter and three quarters of the window, so the phase gathered before each pulse is undone after it, and the pulses carry the calibration's single qubit error. `--emit pulse` writes the pulses with their delays, and the success estimate in `--emit cost` counts the coherent error, or the pulses instead of it under `--dd`. On `examples/line5.cal` with 300 kHz of detuning on every qubit, a circuit that leaves one qubit waiting through three CNOTs comes out right in 42% of shots, and in 97% with `--dd`.

```text
$ qirc tests/corpus/qsharp_loop.ll --emit qasm3 --basis rz-sx-cx --coupling line:4
OPENQASM 3.0;
include "stdgates.inc";

qubit[4] q;
bit[4] c;

rz(3.141592653589793) q[0];
sx q[0];
rz(-1.5707963267948966) q[0];
sx q[0];
rz(3.141592653589793) q[0];
cx q[0], q[1];
cx q[1], q[2];
cx q[2], q[3];
c[0] = measure q[0];
c[1] = measure q[1];
c[2] = measure q[2];
c[3] = measure q[3];
```

## Python

`python/` wraps the compiler as a Python module built with PyO3. `pip install ./python` builds it from source.

```python
import qirc

source = open("tests/corpus/adaptive_teleport.ll").read()
qasm = qirc.compile(source, emit="qasm3", opt=3, gates="rz-sx-cx", coupling="line:3")
counts = qirc.run(source, shots=1000, seed=7)
assert qirc.diff(source, opt=3, gates="rz-sx-cx", resynth=4) == "equivalent"
worst = qirc.cost(source, gates="rz-sx-cx")["worst"]
```

`compile` returns the text of any emit kind, `run` returns measurement counts, `diff` returns `"equivalent"`, `"different"` or `"inconclusive"`, and `cost` returns the per path counts from `--emit cost` as dictionaries. Options are keywords with the command line values: `opt`, `gates`, `exclude`, `resynth`, `cost`, `coupling`, `calibration`, `epsilon` and `budget`, plus `relabel=True`, `reuse=True`, `noisy=True` and `dd=True`, and `bind` takes a dictionary of input values. `compile(..., emit="resources")` returns the resource estimate. The package ships type stubs, so editors complete every function and keyword. With a calibration, `cost` also reports the estimated `success` probability. Compile errors raise `qirc.CompileError` with the rendered diagnostics, and warnings go through the `warnings` module.

With `pip install "qirc[qiskit]"`, qirc is also a Qiskit optimization stage: `transpile(circuit, basis_gates=["rz", "sx", "x", "cx"], optimization_level=3, optimization_method="qirc")` runs Qiskit's optimization and then qirc's, keeping whichever circuit has fewer two qubit gates.

### From Q# and PyQIR

Q# and PyQIR both produce QIR text, which every qirc function accepts as it is.

```python
from qdk import qsharp
import qirc

qsharp.init(target_profile=qsharp.TargetProfile.Adaptive_RI)
qsharp.eval(open("Teleport.qs").read())
counts = qirc.run(str(qsharp.compile("Teleport()")), shots=1000)
```

```python
from pyqir import BasicQisBuilder, SimpleModule
import qirc

module = SimpleModule("bell", num_qubits=2, num_results=2)
qis = BasicQisBuilder(module.builder)
qis.h(module.qubits[0])
qis.cx(module.qubits[0], module.qubits[1])
qis.mz(module.qubits[0], module.results[0])
qis.mz(module.qubits[1], module.results[1])
counts = qirc.run(module.ir(), shots=1000)
```

`str(qsharp.compile(...))` is the module Q# would submit to hardware, so `qirc.compile(..., emit="qasm3")`, `qirc.diff` and `qirc.cost` work on it the same way. The older `import qsharp` package works too.

## Editors

`qirc lsp` is a language server: it reads QIR and OpenQASM files from an editor over stdin and stdout and answers with the same diagnostics the command line prints, codes and notes included, on every change. Any editor with a language client can start it as `qirc lsp` to show qirc's errors as you type. It skips a check when a newer edit to the same file is already waiting, and files over 1 MB are not checked, so typing stays responsive in large generated programs.

## Playground

Try it at <https://angadbasandrai.github.io/qirc/>.

`web/` builds qirc for the browser. The page has the full command line and every emit kind, controls for rotation precision, OpenQASM inputs and minimising an observable over them, a detuned device with and without echo pulses, an editor with QIR highlighting, line numbers and example programs, and it runs the compiler in a Web Worker so a long simulation can be stopped. Diagnostics keep their colours and link to the line they point at. Beside the text output it draws the compiled circuit block by block, with zoom, a fit to view button, qubit labels that stay in place while scrolling and the gate under the pointer named in a tooltip, lets a straight line circuit of up to 12 qubits be stepped through gate by gate with the state and every qubit's Bloch vector after each one, saves the drawing as a PNG, charts measurement counts or final state probabilities, and shows how qubits, gates, two qubit gates, T gates and depth changed from the source. A `.ll` file can be opened or dropped on the editor, and when the page is served on its own it can save the output and copy a link that carries the program, the command and every setting, so the link opens on the same output and view. Nothing is sent to a server.

```text
cargo build --release --target wasm32-unknown-unknown --manifest-path web/Cargo.toml
cp web/target/wasm32-unknown-unknown/release/qirc_web.wasm web/qirc.wasm
python -m http.server --directory web
```

Every push to `main` rebuilds the module and publishes the page with GitHub Pages.

The module exports `allocate`, `release` and `run`, which takes the source and the arguments as UTF-8 and returns the exit code, standard output and standard error. The playground simulates at most 20 qubits.

## Circuit diagrams

`--emit circuit` prints an ASCII diagram, `--emit quantikz` writes a LaTeX `quantikz` environment and `--emit svg` a standalone SVG image. The two drawings pack each operation into the earliest column its qubits allow, write angles as fractions of pi where they are, and mark where each block of a branching program starts, as a `\slice` in LaTeX and a dashed line in SVG. The LaTeX needs `\usetikzlibrary{quantikz2}` and compiles with pdflatex.

```text
$ qirc tests/corpus/base_profile_bell.ll --emit quantikz
\begin{quantikz}
\lstick{$q_{0}$} & \gate{H} & \ctrl{1} & \meter{} & \qw \\
\lstick{$q_{1}$} & \qw & \targ{} & \meter{} & \qw
\end{quantikz}
```

## Cost report

`--emit cost` counts what a compiled program costs without simulating it. Every path from the entry block to a return is listed with its T gates, two qubit gates, total gates, depth and the peak number of qubits in use at once, followed by the worst case over all paths. Depth schedules each gate, measurement and reset as early as its qubits allow. A qubit is in use from its first operation until its last, and a reset frees it. A path that jumps back to a block it already passed ends there, so a loop is counted once per pass. At most 256 paths are listed.

```text
$ qirc tests/corpus/adaptive_teleport.ll --emit cost -O2 --gates rz-sx-cx
path                                            t      cx   gates   depth    live
entry > then_x > join_x > then_z > join_z       0       2      18      13       3
entry > then_x > join_x > join_z                0       2      17      13       3
entry > join_x > then_z > join_z                0       2      17      13       3
entry > join_x > join_z                         0       2      16      13       3
worst case                                      0       2      18      13       3
```

## Resource estimation

`--emit resources` estimates what the compiled program would need on a fault tolerant machine built from surface codes.

```text
$ qirc adder_8.qasm --emit resources -O2
logical qubits    24
T gates           173
T depth           12, as layers of commuting Pauli product rotations
magic states      173
logical steps     173
code distance     15
distillation      1 level of 15-to-1, 11 factories of 11 tiles
physical qubits   82,800 (28,350 for data, 54,450 for factories)
runtime           1.104 ms, 2,760 cycles of 400 ns
error budget      0.001: 5.0e-4 logical, 5.0e-4 magic states
```

The program is lowered to Clifford gates, T gates and rotations, and the worst path's peak live qubits, T gates and rotations are counted, so phase teleportation's savings carry straight through. Each rotation costs about 3 log2(1/e) T gates, as gridsynth needs, with e its share of the budget. The logical qubits sit in a fast block of 2n + ⌈√(8n)⌉ + 1 tiles, where each T gate takes one logical step of d code cycles, and 15-to-1 distillation factories of 11 tiles, taking 11 steps per state, supply one magic state per step, with a second or third level when the first is not clean enough. The failure budget, 0.001 by default or `--budget`, is split evenly between logical errors, distillation and rotation synthesis, and the code distance is the smallest odd d for which every tile over every cycle stays within its share at a logical error rate of 0.03 (p/0.01)^((d+1)/2). The physical error rate p and the cycle time come from the calibration's CNOT errors and 4 CNOT times plus 2 readout times, or default to 0.001 and 400 ns, the gate based figures Azure's resource estimator also uses. A physical error rate at the threshold of 0.01 or above has no surface code estimate.

## Pauli product rotations

`--emit rotations` rewrites the program in the form Litinski's Game of Surface Codes starts from: a list of rotations about multi-qubit Pauli products followed by Pauli product measurements, with every Clifford gate gone. The program is lowered to Clifford gates, T gates and rotations as for a resource estimate, and qirc walks through it keeping the image of each qubit's X and Z under the Clifford gates seen so far. A T gate on a qubit becomes a pi/8 rotation about the current image of its Z, any other rotation keeps its angle, and a measurement measures the image of Z. A new rotation is moved back past every rotation and measurement it commutes with, and when it meets one about the same Pauli product the two angles add: pi/8 and -pi/8 cancel, and two pi/8 rotations make a Clifford, which is folded into the images of the gates that follow.

```text
$ qirc mod5_4.qasm --emit rotations
Pauli product rotations for main, 5 qubits

layer  step
    1  +pi/8     Z0 Z3 X4
    1  -pi/8     Z0 Z3
    1  +pi/8     Z2 Z3 X4
    1  -pi/8     Z2 Z3
    1  +pi/8     Z1 Z2 X4
    1  -pi/8     Z1 Z2
    1  +pi/8     Z0 Z1 X4
    1  -pi/8     Z0 Z1

8 pi/8 rotations in T depth 1, 0 other rotations, 0 measurements, 1 layer in all
10 rotations merged with an earlier one about the same Pauli product
a rotation by a about P applies exp(-i a P), and the Clifford gates are folded into the rotations and measurements
```

Each step goes in the layer after the last earlier step it does not commute with, so the steps within a layer commute and can run in any order, and the T depth counts the layers that hold pi/8 rotations. `--emit resources` reports it too. On the eleven Clifford+T benchmarks from the optimisation section, merging rotations this way reaches from `-O0` the same 938 T gates that phase teleportation and PyZX reach, circuit by circuit, with T depths from 1 for `mod5_4` and `gf2^4_mult` to 17 for `tof_10`, each in about a tenth of a second. The tests check on random five qubit circuits with arbitrary rotations that the rotations and measurements give the same outcome distribution as the circuit. A program with branches has no single list of rotations, and a reset after a qubit is used needs a correction that depends on a measurement, so both are refused. Qubits that are never measured are left with a Clifford gate at the end, which the list does not show.

## Surface code simulation

`qirc surface` checks the logical error rate that resource estimates assume by simulating it. For each distance it lays out a rotated surface code, measures its stabilizers for d rounds with the usual CNOT orders, which keep hook errors across the logical operator, and applies a Pauli error with probability p after every gate, reset and measurement, p coming from `--error` or from a calibration's CNOT errors. A Pauli frame simulator runs 64 shots at once in the bits of a machine word, the decoding graph is built by injecting every possible single fault and recording which checks it trips and whether it flips the logical qubit, and each shot is decoded by union find.

```text
$ qirc surface --error 0.001 --shots 200000
surface code memory, physical error 0.001 on every gate, reset and measurement

distance  rounds     shots  failures      per round          model
       3       3    200000       215       3.586e-4       3.000e-4
       5       5    200000        34       3.400e-5       3.000e-5
       7       7    200000         8       5.714e-6       3.000e-6
```

At an error rate of 0.001 the simulated logical error per round follows 0.03 (p/0.01)^((d+1)/2), the formula `--emit resources` uses, within about a factor of two. Near the threshold it does not: at 0.005 the rate falls from 7.0e-3 to only 2.9e-3 between distances 3 and 7, where the formula expects 1.9e-3, so estimates for such noisy devices are optimistic. Every single fault is corrected at distances 3 and 5, which the tests check fault by fault.

## Releasing

Pushing a tag such as `v0.1.0` runs `.github/workflows/release.yml`, which builds the binaries, the wheels and a source distribution and attaches them to a GitHub release. Publishing to PyPI runs when the repository variable `PUBLISH_PYPI` is `true` and PyPI trusts the workflow, and publishing to crates.io runs when `PUBLISH_CRATES` is `true` and the `CARGO_REGISTRY_TOKEN` secret is set.

## Benchmarks

`bench/compare.py` compiles the same circuits with Qiskit 2.5 (`transpile` at `optimization_level=3`), tket 2.18 (`FullPeepholeOptimise`, then a rebase and squash) and qirc (`-O3 --gates rz-sx-cx --resynth 4 --relabel`, with and without `--cost cx`), all to `rz`, `sx`, `x` and `cx`. The circuits are the straight line programs in `tests/corpus` and `examples` with measurements removed, the QFT, Grover search for the all ones state, and the CDKM, VBE and Draper adders from Qiskit. Each cell is two qubit gates / total gates / depth, and the input column counts each Toffoli and SWAP as one gate. Every all to all result up to 12 qubits is checked against the input by exact operator comparison, allowing for the qubit permutation each tool reports, and all of them pass.

All to all connectivity:

| circuit | qubits | input | Qiskit | tket | qirc | qirc `--cost cx` |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| redundant | 4 | 2 / 13 / 5 | 0 / 3 / 2 | 0 / 3 / 2 | 0 / 3 / 2 | 0 / 3 / 2 |
| small | 2 | 1 / 3 / 2 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 |
| pyqir_real | 3 | 1 / 4 / 2 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 |
| pyqir_simple | 3 | 2 / 12 / 7 | 6 / 27 / 18 | 6 / 36 / 22 | 6 / 24 / 14 | 6 / 24 / 14 |
| qsharp_loop | 4 | 3 / 4 / 4 | 3 / 6 / 6 | 3 / 6 / 6 | 3 / 6 / 6 | 3 / 6 / 6 |
| unrestricted_dynamic | 5 | 4 / 6 / 5 | 4 / 8 / 7 | 4 / 8 / 7 | 4 / 8 / 7 | 4 / 8 / 7 |
| qft_4 | 4 | 14 / 36 / 24 | 12 / 33 / 23 | 12 / 33 / 23 | 12 / 34 / 23 | 12 / 34 / 23 |
| qft_6 | 6 | 33 / 84 / 40 | 30 / 73 / 37 | 30 / 73 / 37 | 30 / 71 / 35 | 30 / 71 / 35 |
| qft_8 | 8 | 60 / 152 / 56 | 56 / 129 / 51 | 56 / 129 / 51 | 56 / 120 / 47 | 56 / 120 / 47 |
| qft_10 | 10 | 95 / 240 / 72 | 90 / 201 / 65 | 90 / 201 / 65 | 90 / 181 / 59 | 90 / 181 / 59 |
| grover_3 | 3 | 4 / 39 / 21 | 24 / 85 / 53 | 20 / 140 / 86 | 24 / 89 / 55 | 23 / 154 / 95 |
| grover_4 | 4 | 84 / 250 / 171 | 84 / 228 / 166 | 84 / 243 / 183 | 84 / 234 / 171 | 84 / 234 / 171 |
| grover_5 | 5 | 288 / 941 / 681 | 288 / 788 / 607 | 288 / 849 / 662 | 288 / 732 / 603 | 288 / 732 / 603 |
| adder_cdkm_4 | 9 | 24 / 24 / 21 | 64 / 151 / 114 | 47 / 130 / 88 | 44 / 107 / 84 | 44 / 107 / 84 |
| adder_vbe_3 | 8 | 13 / 13 / 11 | 37 / 87 / 54 | 37 / 87 / 56 | 25 / 63 / 41 | 25 / 85 / 55 |
| adder_draper_4 | 8 | 48 / 122 / 74 | 40 / 123 / 67 | 40 / 107 / 57 | 43 / 94 / 53 | 40 / 110 / 60 |
| total |  |  | 740 / 1952 / 1278 | 719 / 2055 / 1353 | 711 / 1776 / 1208 | 707 / 1879 / 1269 |

A line of qubits (`--coupling line:n`, Qiskit `CouplingMap.from_line`, tket `DefaultMappingPass`):

| circuit | qubits | input | Qiskit | tket | qirc | qirc `--cost cx` |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| redundant | 4 | 2 / 13 / 5 | 0 / 3 / 2 | 0 / 3 / 2 | 0 / 3 / 2 | 0 / 3 / 2 |
| small | 2 | 1 / 3 / 2 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 |
| pyqir_real | 3 | 1 / 4 / 2 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 |
| pyqir_simple | 3 | 2 / 12 / 7 | 7 / 37 / 24 | 9 / 39 / 25 | 7 / 25 / 15 | 7 / 26 / 16 |
| qsharp_loop | 4 | 3 / 4 / 4 | 3 / 6 / 6 | 3 / 6 / 6 | 3 / 6 / 6 | 3 / 6 / 6 |
| unrestricted_dynamic | 5 | 4 / 6 / 5 | 8 / 12 / 11 | 11 / 15 / 11 | 6 / 10 / 9 | 6 / 10 / 9 |
| qft_4 | 4 | 14 / 36 / 24 | 19 / 49 / 33 | 24 / 45 / 35 | 19 / 41 / 29 | 19 / 41 / 29 |
| qft_6 | 6 | 33 / 84 / 40 | 51 / 143 / 69 | 72 / 115 / 71 | 51 / 92 / 54 | 45 / 109 / 55 |
| qft_8 | 8 | 60 / 152 / 56 | 90 / 312 / 113 | 137 / 210 / 103 | 97 / 161 / 81 | 83 / 193 / 82 |
| qft_10 | 10 | 95 / 240 / 72 | 147 / 514 / 158 | 222 / 333 / 135 | 149 / 240 / 106 | 135 / 285 / 104 |
| grover_3 | 3 | 4 / 39 / 21 | 37 / 128 / 91 | 35 / 155 / 98 | 47 / 110 / 88 | 35 / 194 / 110 |
| grover_4 | 4 | 84 / 250 / 171 | 165 / 402 / 294 | 171 / 330 / 259 | 209 / 365 / 263 | 171 / 542 / 359 |
| grover_5 | 5 | 288 / 941 / 681 | 543 / 1409 / 893 | 639 / 1200 / 840 | 681 / 1123 / 867 | 597 / 1407 / 974 |
| adder_cdkm_4 | 9 | 24 / 24 / 21 | 93 / 251 / 184 | 86 / 169 / 134 | 78 / 141 / 123 | 66 / 163 / 127 |
| adder_vbe_3 | 8 | 13 / 13 / 11 | 66 / 160 / 101 | 79 / 129 / 91 | 52 / 90 / 64 | 44 / 151 / 85 |
| adder_draper_4 | 8 | 48 / 122 / 74 | 90 / 202 / 117 | 112 / 179 / 119 | 86 / 137 / 76 | 81 / 168 / 85 |
| total |  |  | 1321 / 3638 / 2104 | 1602 / 2938 / 1937 | 1487 / 2554 / 1791 | 1294 / 3308 / 2051 |

All three tools remove SWAP gates by relabelling qubits, which qirc does with `--relabel`. On all to all connectivity plain qirc needs 8 fewer CNOTs than tket and 29 fewer than Qiskit, and has the fewest gates and the lowest depth of the three by a clear margin, from phase folding and from lowering Toffolis that compute and later uncompute the same bit as relative phase Toffolis, which halves their CNOTs. Routed on a line, qirc with `--cost cx` needs 2% fewer CNOTs than Qiskit with fewer gates and lower depth, and 19% fewer than tket, and plain qirc has the fewest gates and the lowest depth.

Compile time over the whole set is about 0.3 s for Qiskit, 26 s for tket and 0.9 s for qirc, or 2.9 s routed with `--cost cx`, counting a process start per circuit. To run it, install `qiskit` and `pytket` and build qirc in release mode:

```text
python bench/compare.py --qirc target/release/qirc
python bench/compare.py --qirc target/release/qirc --line
```

## Checking a compile

`qirc diff a.ll` compiles `a.ll` twice, once at `-O0` and once with the given options, and compares them branch by branch. Every measurement splits the run into both outcomes, and each outcome must have the same probability, the same recorded values and the same final state up to global phase. `qirc diff a.ll b.ll` compares two different programs the same way.

```text
$ qirc diff tests/corpus/qsharp_teleport.ll -O3 --gates rz-sx-cx --resynth 4
equivalent: 8 outcomes agree in probability and final state
```

Branches below a probability of 1e-12 are pruned and the search stops after 20,000 runs, most probable branches first. When the unexplored probability is above 1e-9 the result is reported as inconclusive and the exit code is 2, so a loop that repeats until success is checked up to a stated remainder. With `--coupling` only the outcome probabilities are compared, because routing moves qubits.

## Simulator

The state vector is stored as separate real and imaginary arrays, so a single qubit gate is a 2x2 complex matrix applied to every pair of amplitudes at once. On x86_64 with AVX2 and FMA, four amplitudes are processed per instruction.

When the target qubit is 2 or higher, each pair's two halves are contiguous and load directly. When the target is qubit 0 or 1, both halves sit in the same register and are paired with a permute. Controls are a bit mask: a control above bit 1 is constant across a register, so whole registers are skipped, and a control on bit 0 or 1 is blended per lane. This covers Toffoli and controlled swap without a separate code path. CPUs without AVX2 use a scalar fallback. From 21 qubits each gate is split across the CPU's threads, a chunk of the amplitudes each, or a pair of chunks when the target qubit is above the chunk size. At 26 qubits, where the state is a gigabyte and every gate reads and writes all of it, 16 threads make a random circuit 1.5 times faster, which is as far as memory bandwidth allows without applying several gates in one pass.

Straight line programs are evolved once and sampled. Programs that branch on a measurement, reset a qubit, or act on a qubit after measuring it are simulated shot by shot with real collapse.

A program with more than 20 qubits whose gates are all Clifford (H, S, the Paulis, SX, CNOT, CZ, CY, swap, and rotations by multiples of a quarter turn) runs on a stabilizer tableau instead, up to 5,000 qubits, with branches, resets and measurements. The tableau is stored row by row as bit words, so combining two rows costs a few popcounts, and a straight line program is prepared once and each shot measures a copy. A 1,000 qubit GHZ state takes under a second for 1,000 shots. `qirc diff` uses the same tableau for such programs, following each measurement that is not already determined and comparing final states through the reduced row echelon form of their stabilizer groups, so it can check a compile of a large Clifford circuit exactly.

A program with more than 20 qubits whose gates only permute basis states (X, Y, Toffoli and other multi controlled X gates, swaps) or only add phases (Z, S, T, Rz, CZ and other controlled phases) runs on a plain bit vector instead, with no limit on its width, since such gates keep a basis state a basis state. Arithmetic like a ripple carry adder falls in this class even though its Toffolis are not Clifford: two 5000 bit numbers add on 10,002 qubits in 64 ms, and noise still applies, as a bit flip for X and Y errors and nothing for Z errors, which only change the phase. The run prints the circuit only when it has at most 200,000 cells of qubits by depth, and points to `--emit circuit` otherwise.

`--observable` prints the exact expectation value of a sum of Pauli strings, written like `Z0 Z1 + 0.5 X2 - 2*Y3`. Measurements at the end of the program are left out, so the value is taken on the state just before them, and a program that branches on mid circuit measurements is averaged exactly over every branch, weighted by its probability. On the teleportation example the teleported qubit gives 0.707107 for both `Z2` and `X2`, as `ry(pi/4)` should. A program without mid circuit measurements is not limited to 30 qubits: each Pauli string only depends on the gates in its backward light cone, so each is simulated on a small circuit over just the qubits of its cone. A 200 qubit layer of `ry` rotations and CNOT pairs gives `Z199 + X0 X1` exactly in 50 ms. A cone of permutation and phase gates is a basis state, where a string with an X or Y gives 0 and a string of Z gives the sign of the parity of its bits. A cone of Clifford gates is simulated on the stabilizer tableau instead, however wide it is: after basis changes and CNOTs fold the Pauli string onto one qubit, its value is 0 if measuring that qubit would be random and the sign of the deterministic outcome otherwise, so `Y0 Y1 X2 ... X999` on a 1000 qubit GHZ state gives -1. A cone that is neither Clifford nor small enough for a state vector is lowered to `rz`, `sx` and `cx` and the Pauli string is pushed backwards through it in the Heisenberg picture: a CNOT maps each string to one string, a rotation splits a string that anticommutes with it into a cosine and a sine part, and at the start only strings of I and Z count. This stays exact while few rotations touch the cone, so `rz(0.3)`, `t` and `rx(0.2)` on a 100 qubit GHZ state give `X0 ... X99 + Z0 Z99` = 1.446627 in 80 ms, and it gives up once a term expands past 65536 strings.

When the expansion gives up, the cone is cut instead. qirc splits its qubits into the fewest groups of at most 30 that as few two qubit gates as possible connect, peeling off one group at a time and moving qubits between groups while that removes connecting gates, and tries smaller groups when larger ones would need more memory than it allows, and writes each connecting gate as a sum of products of one qubit operators: a controlled gate as |0⟩⟨0| ⊗ I + |1⟩⟨1| ⊗ U, and a swap as half the sum of P ⊗ P over the four Paulis. Each half is simulated as a state vector once for every choice of those terms, branching at each cut so the gates before it run only once, and the expectation is recombined exactly from the overlaps between those states, one for each pair of choices, so the work grows with the square of the number of choices and a few cuts are practical. A 40 qubit circuit made of two 20 qubit halves of random rotations and CNOT chains, joined by two CNOTs, gives `Z0 Z39 + X10 + Z19 Z20` exactly in about 5 seconds, and followed by its own inverse, four cuts, it gives exactly 2. Terms whose light cones keep the same gates share one set of simulations. Three 20 qubit blocks joined by two CNOTs give an exact expectation on 60 qubits in about a second.

Cutting also samples. A straight line program above 30 qubits that measures at most 20 of them at the end, and cuts within the same limits, runs as circuit cutting before falling back to the matrix product state: each piece's states give a table of overlaps for every outcome of its measured qubits, and the tables recombine into the exact distribution over the measured bits, which the shots are drawn from. The 40 qubit circuit above, measuring 6 qubits, samples 4,000 shots in about 2 seconds.

A straight line program with more than 30 qubits whose gates are Clifford apart from up to 10 rotations by other angles, such as T gates, runs on a sum of stabilizer states. Each state is kept in CH form, which unlike a tableau tracks its global phase, so the terms of the sum can interfere. Lowered to `rz`, `sx` and `cx`, a CNOT or a quarter turn is a row operation on each term, a Hadamard folds the two basis strings it produces back into one state with a few CNOTs, CZs and an S, and a rotation by any other angle splits every term into two with cos(θ/2) I - i sin(θ/2) Z. Shots are drawn exactly by gate by gate sampling, which needs only amplitudes: every shot moves through the circuit with the state, a CNOT or X flips its bits, a phase changes nothing, and only an `sx` redraws one bit from the amplitudes of its two neighbours. Shots that agree so far share those amplitudes. Measurements and resets in the middle of a straight line program are deferred: a measurement becomes a CNOT onto a fresh qubit read at the end, and a reset swaps the qubit with a fresh one, which gives the same statistics exactly. A program that branches on a result goes to the matrix product state instead. A 40 qubit circuit with three T gates spread along a CNOT chain takes 3 seconds for 40,000 shots, and every qubit's frequency of 1 matches the exact value from the light cone expectation to within 0.0004.

Any other program with more than 30 qubits runs as a matrix product state: one small tensor per qubit, joined by bonds whose size grows with entanglement. A two qubit gate on neighbouring tensors is applied to their product and split back with a singular value decomposition, gates between distant qubits move one tensor next to the other with swaps, and the bonds are capped at 32 by default or at `--bond n`, dropping the smallest singular values. The run reports the weight it dropped, so an approximate answer says so. Without the cap the result is exact, which the tests check against the state vector amplitude by amplitude. A 60 qubit circuit of four layers of arbitrary rotations and CNOTs runs in 1.3 seconds and its marginals match exact light cone values to within sampling noise, and a 40 qubit circuit of twelve layers drops 0.09% of the weight at bond 32. `--bond` also forces this kernel on a small program.

The stabilizer tableau, the bit vector, the stabilizer sum, circuit cutting and the matrix product state are chosen in that order, the first that fits, and the run prints which. A matrix product state reports the weight it dropped as one minus the product of the fractions each truncation keeps.

## Pulse schedules

`--emit schedule` with a calibration lays the compiled program out in time: every gate starts as soon as its qubits are free, one qubit gates take the calibration's single qubit time, CNOTs its per edge time and measurements its readout time, and phase gates like `rz` take no time because hardware applies them as a frame change. `--emit pulse` writes the same schedule as OpenQASM 3 with OpenPulse calibrations: a port and a drive frame per qubit at the frequency from a `frequency q GHz` line, `rz` as `shift_phase` on that frame, `sx` and `x` as DRAG pulses with the amplitude and DRAG coefficient from an optional `drive q amplitude beta` line, and each CNOT as a cross resonance `gaussian_square` pulse from the control's port at the target's frequency, with its amplitude from an optional `cross a b amplitude` line. A qubit with a `resonator q GHz` line, and an optional amplitude after it, gets a readout tone on a measure frame and a `capture_v2` on an acquire frame as its `defcal measure`, lasting the calibration's readout time; other qubits keep the device's own measurement. Idle time becomes explicit `delay` statements. A single pulse per CNOT, without echo or rotary tones, is a starting point to calibrate against a device rather than a finished gate. `examples/line5.cal` has frequencies, so `qirc bell.ll --calibration examples/line5.cal --emit pulse` works as is, and the output is accepted by the reference OpenPulse parser from the OpenQASM project.

## Running on IonQ

`qirc submit program.ll` compiles the program and sends it to IonQ as a job in IonQ's native circuit format, with the API key from the `IONQ_API_KEY` environment variable, `--target` choosing the backend (`simulator` by default, or a QPU such as `qpu.aria-1`) and `--shots` the shot count. It waits for the job, then prints the counts per result like a local run. `--emit ionq` prints the job instead of sending it. IonQ measures every qubit once at the end, so the program must be straight line with its measurements last. qirc keeps its single dependency by calling the `curl` that ships with Windows, macOS and Linux, and passes the key to it on standard input, so it never appears in the process list. The submission, polling and result mapping are tested end to end against a local server that imitates IonQ's API; a real job needs your own key, which IonQ gives out free with access to its cloud simulator.

With `--calibration` and `--noisy`, every gate is followed by a random Pauli error with the probability the calibration gives for it, uniform over the nonidentity Paulis on its qubits, and every measurement result flips with the readout error of its qubit. Pauli errors are Clifford, so noisy Clifford programs still run on the tableau. For a 5 qubit GHZ state on `examples/line5.cal` the fraction of correct shots is 0.885 against a `--emit cost` estimate of 0.861, which counts every error as fatal.

## Testing

```text
cargo test
```

The tests include:

- `tests/corpus/`, a set of QIR modules in Base Profile, Adaptive Profile, PyQIR, Q# and unrestricted styles, all accepted by clang. `qsharp_bell`, `qsharp_ising`, `qsharp_teleport` and `qsharp_count` come straight from the Q# compiler with only its comment line removed, and `pyqir_real` is byte for byte what PyQIR 0.12 emits.
- A differential test that checks the AVX2 kernel against a naive reference simulator on random circuits.
- Random unitary, measurement and memory programs compared across every optimisation level.
- Round trips through the QIR emitter and back through the frontend.
- Decomposition checks against the exact Toffoli, controlled unitary and swap matrices for several gate sets.
- Random programs compiled for several gate sets, with and without resynthesis, checked branch by branch against `-O0`.
- KAK synthesis of random two qubit unitaries and of Clifford+T words, checked exactly and against the CNOT count of the word.
- Quantum Shannon decomposition of random and Clifford+T three qubit unitaries, checked exactly and against 21 CNOTs.
- Relabelled and routed random circuits compared state by state through the reported final layout, and noise aware routing checked to avoid a poor coupler.
- The stabilizer tableau checked against the state vector on random Clifford circuits, measurement by measurement, and `qirc diff` on 24 qubit Clifford circuits both for compiled programs and for programs with a single extra gate.
- Phase folding, qubit reuse and noisy simulation, each checked against an exact or statistical expectation.
- OpenQASM input on hand checked programs, error cases and deeply nested or exponentially expanding input.
- `scripts/sweep.sh`, run in CI, which proves every corpus program equivalent to itself under sixteen combinations of optimisation, gate sets, resynthesis, routing, relabelling, reuse and calibration, and 600 randomly damaged programs run through every emit kind without a crash.
- Phase teleportation on random Clifford+T circuits and on phase gadgets, circuit cutting against the state vector for two and three pieces, expectations and distributions, rotation synthesis against its precision and T count bound, and resource estimates, decoupling and variational inputs against exact values.

## Limitations

- A qubit index that depends on a measurement cannot be resolved, because qubits are assigned at compile time.
- Recursion that is not a tail call is only expanded when its depth is known at compile time, up to 10,000 calls deep, or 32 in the browser.
- Above 30 qubits a circuit without structure to exploit runs as a matrix product state, which is exact only while its entanglement fits the bond cap and otherwise reports the weight it dropped.
- OpenQASM 3 input reads classical values that are set once. Values that change, `bool`, `break` and subroutines are refused.
- `--epsilon` reaches a precision of about 1e-10 for each rotation, and some angles take several seconds there, since the candidates are enumerated one coordinate at a time rather than by Ross and Selinger's grid operators.
- Circuit cutting pays off only while few gates join the pieces, because the work grows with the square of the product of the terms of every cut, and cut sampling measures at most 20 qubits.
- Resource estimates follow one layout, a fast block with 15-to-1 factories and one error rate for every operation, so they are a first estimate rather than a compiled fault tolerant schedule.
- `qirc submit` talks to IonQ only, and has been tested against a local imitation of its API rather than a live account.
- OpenQASM 3 output writes branches as `if` and `else`, loops as a `while` over blocks, classical values as typed variables and recorded values as `output` variables. A floating point remainder, a pointer cast or a value recorded inside a loop is refused rather than approximated, and `--emit qir` keeps them.
- QIR output decomposes a controlled gate that has no QIR function of its own, and refuses one with three or more controls.

## License

MIT, see [LICENSE](https://github.com/AngadBasandrai/qirc/blob/main/LICENSE).
