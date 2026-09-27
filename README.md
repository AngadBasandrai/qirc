# qirc

A compiler and state vector simulator for [QIR](https://github.com/qir-alliance/qir-spec), the LLVM based intermediate representation used by Q#, PyQIR and other quantum toolchains.

qirc reads `.ll` files, checks them against the QIR profile they declare, optimises the circuit, and then either simulates it or emits OpenQASM 3, QIR or JSON. It can also lower a circuit to a hardware gate set and route it onto a limited qubit connectivity map.

Try it in the browser at https://angadbasandrai.github.io/qirc/.

```
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

measurement over 1000 shots (sampled from the final state):
  00       515   0.5150
  11       485   0.4850

simulated in 851.600µs
```

## Installing

Each release on GitHub carries a prebuilt `qirc` for Linux (x86_64 and arm64), macOS (Intel and Apple silicon) and Windows, and Python wheels for the same platforms.

```
cargo install qirc-compiler
pip install qirc
```

The crate is published as `qirc-compiler` because `qirc` was taken on crates.io. It installs the `qirc` command, and the library is still used as `qirc`.

## Building

Requires Rust 1.88 or newer.

```
cargo build --release
./target/release/qirc --help
```

The only direct dependency is `num-complex`.

## Usage

```
qirc <input.ll> [options]
qirc diff <a.ll> [b.ll] [options]

  --emit <kind>     run | ir | qasm3 | qir | json | circuit | cost | check
                    (default: run)
  -O<n>             optimisation level 0 to 3                         (default: 1)
  --gates <set>     target gate set: rz-sx-cx, rz-ry-cz or a list such as rz,sx,cx
  --exclude <list>  leave gates out of the target set, such as h,t
  --resynth <n>     replace runs of gates by at most n gates, 1 to 6
  --cost <model>    what resynthesis minimises: gates, cx or ibm
  --coupling <map>  route onto line:N, ring:N, grid:RxC, full:N or 0-1,1-2,...
  --calibration <f> route by device error rates and estimate success
  --relabel         remove swaps at the end of the program by permuting qubits
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
| Parse | `parse.rs` | LLVM IR AST |
| Inline | `inline.rs` | AST with helper functions expanded |
| Lower | `lower.rs` | quantum IR |
| Validate | `sema.rs` | profile and range diagnostics |
| Optimise | `opt.rs` | rewritten IR |
| Target | `transpile.rs`, `synth.rs`, `kak.rs`, `route.rs` | basis gates on a coupling map |
| Emit | `codegen.rs`, `cost.rs` | QASM 3, QIR, JSON, circuit diagrams, cost reports |
| Execute | `simulator/` | state vector and shot counts |

`verify.rs` checks block structure, single assignment and reaching definitions over the dominator tree. `--verify-each` runs it after lowering and after every pass.

Errors point at the source:

```
error[QIR0300]: the Base Profile forbids branching
  --> teleport.ll:11:1
   |
11 | entry:
   | ^^^^^^ this program has more than one basic block
   |
   = note: branching needs the Adaptive Profile
```

## Lowering

The frontend parses the subset of textual LLVM IR that QIR producers emit: typed and opaque pointers, `inttoptr` and `getelementptr` constant expressions, parameter attributes, attribute groups, metadata, `phi`, `switch`, varargs calls, packed structs and quoted identifiers.

Lowering first tries to evaluate the program's classical control flow at compile time. Loops over constant ranges are unrolled, `getelementptr` over a global array of qubit ids resolves to a qubit, helper functions are interpreted, and arithmetic folds to constants. Values that depend on a measurement are kept as instructions. The program only keeps its control flow graph when a branch actually depends on a measurement, as in teleportation or repeat until success loops.

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

```
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
| `-O2` | `-O1` plus peephole rewrites and CFG simplification, repeated until nothing changes, then window resynthesis |
| `-O3` | `-O2` plus single qubit gate fusion |

Cancellation, merging and fusion look past gates that commute with the pair, so `rz` on a control qubit cancels across a CNOT. Commutation is checked on the exact matrices of the gates involved.

Window resynthesis multiplies out every run of gates on one or two qubits and replaces the run with anything shorter that has the same matrix up to global phase. By default the replacement must be a single gate, so `t t` becomes `s` and three alternating CNOTs become `swap`. `--resynth n` allows replacements of up to `n` gates: single qubit runs use exact Euler angles, and two qubit runs use a meet in the middle search over the target gate set. Every replacement is checked against the original matrix before it is applied.

With `--resynth 2` or more, or any `--cost`, two qubit runs are also rebuilt from their KAK decomposition with the fewest CNOTs the matrix needs: none for a product of single qubit gates, one when it is locally a CNOT, two when one of its three interaction coordinates is zero, and three otherwise. The result can be longer than `n` gates and is kept only when it is cheaper than the run it replaces. In the same mode the window grows to 96 gates: the longest two qubit run gets one KAK attempt, and a three qubit run with more than 21 two qubit gates is rebuilt by quantum Shannon decomposition. That splits the matrix around its top qubit with a cosine sine decomposition into multiplexed rotations and four two qubit blocks, builds the first three blocks only up to a diagonal so each needs two CNOTs, carries each diagonal into the next block, and needs at most 21 CNOTs in total.

`--cost` sets what cheaper means. `gates` counts gates and breaks ties on two qubit gates, `cx` counts two qubit gates first, and `ibm` charges 10 for a two qubit gate, 1 for other single qubit gates and nothing for `rz`, `s`, `t` and other Z rotations, which IBM hardware applies as frame changes.

On `tests/corpus/pyqir_simple.ll`, `-O3` takes the circuit from 12 gates at depth 7 to 8 gates at depth 4.

## Targeting hardware

`--gates` rebuilds every gate from a target set: a preset such as `rz-sx-cx` for IBM style devices or `rz-ry-cz`, or any list of gates such as `rx,ry,cy`. `--exclude` removes gates from the full QIR set instead, and `--basis` is kept as a name for `--gates`. Multi qubit gates reduce to CNOTs and single qubit matrices first. The CNOT is then mapped onto whichever entangler the set has, and each single qubit matrix is rebuilt from Euler angles over two rotation axes, from one axis plus a fixed gate, or by an exact search over fixed gates such as `h,s,t`. A gate the set cannot express exactly is reported and left as written.

```
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

`--calibration file` reads device error rates, one per line as `cx a b error`, `single q error` or `readout q error`, and routes onto the coupling map its `cx` lines describe unless `--coupling` is also given. The router then measures distance by the error of each coupler, prefers SWAPs on good couplers and keeps the layout with the highest estimated success probability, and `--emit cost` adds a `success` column with the product of one minus the error of every operation on each path. `examples/line5.cal` describes a five qubit line with one poor coupler. In a program with branches or loops every block starts from the same layout, and a block that jumps elsewhere swaps its qubits back before the jump, so each successor sees the layout it expects.

```
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

`compile` returns the text of any emit kind, `run` returns measurement counts, `diff` returns `"equivalent"`, `"different"` or `"inconclusive"`, and `cost` returns the per path counts from `--emit cost` as dictionaries. Options are keywords with the command line values: `opt`, `gates`, `exclude`, `resynth`, `cost`, `coupling` and `calibration`, plus `relabel=True`. With a calibration, `cost` also reports the estimated `success` probability. Compile errors raise `qirc.CompileError` with the rendered diagnostics, and warnings go through the `warnings` module.

## Playground

Try it at https://angadbasandrai.github.io/qirc/.

`web/` builds qirc for the browser. The page has the full command line and every emit kind, an editor with QIR highlighting, line numbers and example programs, and it runs the compiler in a Web Worker so a long simulation can be stopped. Diagnostics keep their colours and link to the line they point at. Beside the text output it draws the compiled circuit block by block, charts measurement counts or final state probabilities, and shows how qubits, gates, two qubit gates, T gates and depth changed from the source. A `.ll` file can be opened or dropped on the editor, and when the page is served on its own it can save the output and copy a link that carries the program and command. Nothing is sent to a server.

```
cargo build --release --target wasm32-unknown-unknown --manifest-path web/Cargo.toml
cp web/target/wasm32-unknown-unknown/release/qirc_web.wasm web/qirc.wasm
python -m http.server --directory web
```

Every push to `main` rebuilds the module and publishes the page with GitHub Pages.

The module exports `allocate`, `release` and `run`, which takes the source and the arguments as UTF-8 and returns the exit code, standard output and standard error. The playground simulates at most 20 qubits.

## Cost report

`--emit cost` counts what a compiled program costs without simulating it. Every path from the entry block to a return is listed with its T gates, two qubit gates, total gates, depth and the peak number of qubits in use at once, followed by the worst case over all paths. Depth schedules each gate, measurement and reset as early as its qubits allow. A qubit is in use from its first operation until its last, and a reset frees it. A path that jumps back to a block it already passed ends there, so a loop is counted once per pass. At most 256 paths are listed.

```
$ qirc tests/corpus/adaptive_teleport.ll --emit cost -O2 --gates rz-sx-cx
path                                            t      cx   gates   depth    live
entry > then_x > join_x > then_z > join_z       0       2      18      13       3
entry > then_x > join_x > join_z                0       2      17      13       3
entry > join_x > then_z > join_z                0       2      17      13       3
entry > join_x > join_z                         0       2      16      13       3
worst case                                      0       2      18      13       3
```

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
| qft_4 | 4 | 14 / 36 / 24 | 12 / 33 / 23 | 12 / 33 / 23 | 12 / 35 / 24 | 12 / 35 / 24 |
| qft_6 | 6 | 33 / 84 / 40 | 30 / 73 / 37 | 30 / 73 / 37 | 30 / 77 / 38 | 30 / 77 / 38 |
| qft_8 | 8 | 60 / 152 / 56 | 56 / 129 / 51 | 56 / 129 / 51 | 56 / 135 / 52 | 56 / 135 / 52 |
| qft_10 | 10 | 95 / 240 / 72 | 90 / 201 / 65 | 90 / 201 / 65 | 90 / 209 / 66 | 90 / 209 / 66 |
| grover_3 | 3 | 4 / 39 / 21 | 24 / 85 / 53 | 20 / 140 / 86 | 24 / 89 / 55 | 23 / 154 / 95 |
| grover_4 | 4 | 84 / 250 / 171 | 84 / 228 / 166 | 84 / 243 / 183 | 84 / 234 / 171 | 84 / 234 / 171 |
| grover_5 | 5 | 288 / 941 / 681 | 288 / 788 / 607 | 288 / 849 / 662 | 288 / 788 / 603 | 288 / 788 / 603 |
| adder_cdkm_4 | 9 | 24 / 24 / 21 | 64 / 151 / 114 | 47 / 130 / 88 | 50 / 121 / 88 | 50 / 121 / 88 |
| adder_vbe_3 | 8 | 13 / 13 / 11 | 37 / 87 / 54 | 37 / 87 / 56 | 37 / 94 / 56 | 37 / 127 / 76 |
| adder_draper_4 | 8 | 48 / 122 / 74 | 40 / 123 / 67 | 40 / 107 / 57 | 44 / 114 / 62 | 40 / 128 / 66 |
| total |  |  | 740 / 1952 / 1278 | 719 / 2055 / 1353 | 730 / 1947 / 1252 | 725 / 2059 / 1316 |

A line of qubits (`--coupling line:n`, Qiskit `CouplingMap.from_line`, tket `DefaultMappingPass`):

| circuit | qubits | input | Qiskit | tket | qirc | qirc `--cost cx` |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| redundant | 4 | 2 / 13 / 5 | 0 / 3 / 2 | 0 / 3 / 2 | 0 / 3 / 2 | 0 / 3 / 2 |
| small | 2 | 1 / 3 / 2 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 |
| pyqir_real | 3 | 1 / 4 / 2 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 |
| pyqir_simple | 3 | 2 / 12 / 7 | 7 / 37 / 24 | 9 / 39 / 25 | 7 / 25 / 15 | 7 / 26 / 16 |
| qsharp_loop | 4 | 3 / 4 / 4 | 3 / 6 / 6 | 3 / 6 / 6 | 3 / 6 / 6 | 3 / 6 / 6 |
| unrestricted_dynamic | 5 | 4 / 6 / 5 | 8 / 12 / 11 | 11 / 15 / 11 | 6 / 10 / 9 | 6 / 10 / 9 |
| qft_4 | 4 | 14 / 36 / 24 | 19 / 49 / 33 | 24 / 45 / 35 | 19 / 42 / 29 | 19 / 42 / 29 |
| qft_6 | 6 | 33 / 84 / 40 | 51 / 143 / 69 | 72 / 115 / 71 | 53 / 100 / 56 | 45 / 122 / 64 |
| qft_8 | 8 | 60 / 152 / 56 | 90 / 312 / 113 | 137 / 210 / 103 | 101 / 180 / 81 | 83 / 221 / 83 |
| qft_10 | 10 | 95 / 240 / 72 | 147 / 514 / 158 | 222 / 333 / 135 | 159 / 278 / 107 | 135 / 364 / 109 |
| grover_3 | 3 | 4 / 39 / 21 | 37 / 128 / 91 | 35 / 155 / 98 | 47 / 110 / 88 | 35 / 194 / 110 |
| grover_4 | 4 | 84 / 250 / 171 | 165 / 402 / 294 | 171 / 330 / 259 | 209 / 365 / 263 | 171 / 542 / 359 |
| grover_5 | 5 | 288 / 941 / 681 | 543 / 1409 / 893 | 639 / 1200 / 840 | 698 / 1196 / 916 | 594 / 1528 / 1033 |
| adder_cdkm_4 | 9 | 24 / 24 / 21 | 93 / 251 / 184 | 86 / 169 / 134 | 88 / 159 / 134 | 76 / 176 / 134 |
| adder_vbe_3 | 8 | 13 / 13 / 11 | 66 / 160 / 101 | 79 / 129 / 91 | 80 / 137 / 95 | 70 / 202 / 120 |
| adder_draper_4 | 8 | 48 / 122 / 74 | 90 / 202 / 117 | 112 / 179 / 119 | 91 / 161 / 90 | 81 / 193 / 93 |
| total |  |  | 1321 / 3638 / 2104 | 1602 / 2938 / 1937 | 1563 / 2782 / 1899 | 1327 / 3639 / 2175 |

All three tools remove SWAP gates by relabelling qubits, which qirc does with `--relabel`. On all to all connectivity qirc needs 11 more CNOTs than tket and 10 fewer than Qiskit in plain mode, with the fewest gates and the lowest depth of the three. Routed on a line, qirc with `--cost cx` is within half a percent of Qiskit on CNOTs and total gates and needs 17% fewer CNOTs than tket, and plain qirc has the fewest gates and the lowest depth.

Compile time over the whole set is about 0.2 s for Qiskit, 20 s for tket and 0.7 s for qirc, or 3.2 s routed with `--cost cx`, counting a process start per circuit. To run it, install `qiskit` and `pytket` and build qirc in release mode:

```
python bench/compare.py --qirc target/release/qirc
python bench/compare.py --qirc target/release/qirc --line
```

## Checking a compile

`qirc diff a.ll` compiles `a.ll` twice, once at `-O0` and once with the given options, and compares them branch by branch. Every measurement splits the run into both outcomes, and each outcome must have the same probability, the same recorded values and the same final state up to global phase. `qirc diff a.ll b.ll` compares two different programs the same way.

```
$ qirc diff tests/corpus/qsharp_teleport.ll -O3 --gates rz-sx-cx --resynth 4
equivalent: 8 outcomes agree in probability and final state
```

Branches below a probability of 1e-12 are pruned and the search stops after 20,000 runs, most probable branches first. When the unexplored probability is above 1e-9 the result is reported as inconclusive and the exit code is 2, so a loop that repeats until success is checked up to a stated remainder. With `--coupling` only the outcome probabilities are compared, because routing moves qubits.

## Simulator

The state vector is stored as separate real and imaginary arrays, so a single qubit gate is a 2x2 complex matrix applied to every pair of amplitudes at once. On x86_64 with AVX2 and FMA, four amplitudes are processed per instruction.

When the target qubit is 2 or higher, each pair's two halves are contiguous and load directly. When the target is qubit 0 or 1, both halves sit in the same register and are paired with a permute. Controls are a bit mask: a control above bit 1 is constant across a register, so whole registers are skipped, and a control on bit 0 or 1 is blended per lane. This covers Toffoli and controlled swap without a separate code path. CPUs without AVX2 use a scalar fallback.

Straight line programs are evolved once and sampled. Programs that branch on a measurement, reset a qubit, or act on a qubit after measuring it are simulated shot by shot with real collapse.

The simulator is limited to 30 qubits. Larger programs can still be compiled and emitted.

## Testing

```
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

## Limitations

- A qubit index that depends on a measurement cannot be resolved, because qubits are assigned at compile time.
- Recursive functions are rejected.
- OpenQASM 3 output writes branches as `if` and `else`, loops as a `while` over blocks, classical values as typed variables and recorded values as `output` variables. A floating point remainder, a pointer cast or a value recorded inside a loop is refused rather than approximated, and `--emit qir` keeps them.
- QIR output decomposes a controlled gate that has no QIR function of its own, and refuses one with three or more controls.

## License

MIT, see [LICENSE](LICENSE).
