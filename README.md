# qirc

A compiler and state vector simulator for [QIR](https://github.com/qir-alliance/qir-spec), the LLVM based intermediate representation used by Q#, PyQIR and other quantum toolchains.

qirc reads `.ll` files, checks them against the QIR profile they declare, optimises the circuit, and then either simulates it or emits OpenQASM 3, QIR or JSON. It can also lower a circuit to a hardware gate set and route it onto a limited qubit connectivity map.

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

  --emit <kind>     run | ir | qasm3 | qir | json | circuit | check   (default: run)
  -O<n>             optimisation level 0 to 3                         (default: 1)
  --gates <set>     target gate set: rz-sx-cx, rz-ry-cz or a list such as rz,sx,cx
  --exclude <list>  leave gates out of the target set, such as h,t
  --resynth <n>     replace runs of gates by at most n gates, 1 to 6
  --coupling <map>  route onto line:N, ring:N, grid:RxC, full:N or 0-1,1-2,...
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
| Target | `transpile.rs`, `route.rs` | basis gates on a coupling map |
| Emit | `codegen.rs` | QASM 3, QIR, JSON, circuit diagrams |
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

`--coupling` inserts SWAPs so that every two qubit gate lands on a physical edge, and remaps measurements so results come back under their original labels.

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

## Checking a compile

`qirc diff a.ll` compiles `a.ll` twice, once at `-O0` and once with the given options, and compares them branch by branch. Every measurement splits the run into both outcomes, and each outcome must have the same probability, the same recorded values and the same final state up to global phase. `qirc diff a.ll b.ll` compares two different programs the same way.

```
$ qirc diff tests/corpus/qsharp_teleport.ll -O3 --gates rz-sx-cx --resynth 4
equivalent: 8 outcomes agree in probability and final state
```

Branches below a probability of 1e-12 are pruned and the search stops after 4096 paths, so loops that repeat until success are checked up to a stated remainder. With `--coupling` only the outcome probabilities are compared, because routing moves qubits.

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

## Limitations

- A qubit index that depends on a measurement cannot be resolved, because qubits are assigned at compile time.
- Recursive functions are rejected.
- Routing requires a straight line program.
- OpenQASM 3 output writes branches as `if` and `else`, loops as a `while` over blocks, classical values as typed variables and recorded values as `output` variables. A floating point remainder, a pointer cast or a value recorded inside a loop is refused rather than approximated, and `--emit qir` keeps them.
- QIR output decomposes a controlled gate that has no QIR function of its own, and refuses one with three or more controls.

## License

MIT, see [LICENSE](LICENSE).
