# qirc

A compiler and state vector simulator for [QIR](https://github.com/qir-alliance/qir-spec), the LLVM based intermediate representation used by Q#, PyQIR and other quantum toolchains.

qirc reads `.ll` files, checks them against the QIR profile they declare, optimises the circuit, and then either simulates it or emits OpenQASM 3, QIR or JSON. It can also lower a circuit to a hardware gate set and route it onto a limited qubit connectivity map.

```
$ qirc tests/corpus/base_profile_bell.ll --shots 1000
kernel:  AVX2 + FMA (4 x f64 lanes)
program: 2 qubits, 2 results, 2 gates, depth 2, profile base_profile

q0: H-*-M---
q1: --+---M-

State vector (2 qubits, 4 amplitudes):
  |00>  0.707107 + 0.000000i   p = 0.500000
  |11>  0.707107 + 0.000000i   p = 0.500000

measurement over 1000 shots (sampled from the final state):
  00       526   0.5260
  11       474   0.4740
```

## Building

Requires Rust 1.88 or newer.

```
cargo build --release
./target/release/qirc --help
```

There are no dependencies beyond `num-complex`.

## Usage

```
qirc <input.ll> [options]

  --emit <kind>     run | ir | qasm3 | qir | json | circuit | check   (default: run)
  -O<n>             optimisation level 0 to 3                         (default: 1)
  --basis <name>    decompose into rz-sx-cx or rz-ry-cz
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
| `-O2` | `-O1` plus peephole rewrites and CFG simplification, repeated until nothing changes |
| `-O3` | `-O2` plus single qubit gate fusion |

On `tests/corpus/pyqir_simple.ll`, `-O3` takes the circuit from 12 gates at depth 7 to 8 gates at depth 4.

## Targeting hardware

`--basis` decomposes every gate into a native set: `rz-sx-cx` for IBM style devices, or `rz-ry-cz`. Controlled rotations use the ABC decomposition, and Toffoli gates use the standard 6 CNOT construction.

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
```

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

- `tests/corpus/`, a set of QIR modules in Base Profile, Adaptive Profile, PyQIR, Q# and unrestricted styles, all accepted by clang.
- A differential test that checks the AVX2 kernel against a naive reference simulator on random circuits.
- Random unitary, measurement and memory programs compared across every optimisation level.
- Round trips through the QIR emitter and back through the frontend.
- Decomposition checks against the exact Toffoli, controlled unitary and swap matrices.

## Limitations

- A qubit index that depends on a measurement cannot be resolved, because qubits are assigned at compile time.
- Recursive functions are rejected.
- Routing requires a straight line program.
- OpenQASM 3 output does not include classical control flow.

## License

MIT, see [LICENSE](LICENSE).
