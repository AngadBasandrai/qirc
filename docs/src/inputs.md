# Input programs

qirc reads QIR as textual LLVM IR, the `.ll` files that Q#, PyQIR and LLVM based tools write, and OpenQASM 2 and 3.

## QIR

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

## OpenQASM

A file whose first statement is `OPENQASM 2.0;` or `OPENQASM 3.0;` is read as OpenQASM instead of LLVM IR, so every command works on it: simulate it, optimise and route it, `qirc diff` it against a QIR program, or turn it into QIR with `--emit qir`.

```text
$ qirc bell.qasm --emit qir -O2 --gates rz-sx-cx
```

It reads `qubit`, `bit`, `qreg` and `creg` declarations, the standard gates of `stdgates.inc` and `qelib1.inc` including `u`, `u2`, `u3`, `cp`, `crz`, `cu`, `rzz`, `rxx`, `ryy`, `ccx` and `cswap`, gate definitions with parameters, gates applied to whole registers, all three ways of writing a measurement, `reset`, `barrier`, and `if` on a bit, its negation or a register compared with a number, with an `else`. Angles can use `pi`, arithmetic and functions such as `sin` and `sqrt`. `const int n = 5;` and other `int`, `uint`, `float` and `angle` values set once can size registers and appear in indices and angles. A `for` loop over a range such as `[0:n - 1]` or `[0:2:8]`, or over a set such as `{0, 3}`, is unrolled at compile time with its variable usable in indices like `q[i + 1]` and angles like `pi / 2 ** i`, and a `while` loop on a measured bit becomes a loop in the program, so a repeat until success reads as written. A QFT written with nested loops is proven equivalent by `qirc diff` to the same circuit written out gate by gate, and the `for` and `while` loops Qiskit's OpenQASM 3 exporter writes compile and run. Classical values that change, `bool`, `output`, `break` and subroutines are reported as unsupported. The classical bits are recorded as one array at the end, and the program uses the Adaptive Profile only if it branches, resets or reuses a measured qubit. Errors point at the source like any other diagnostic, and nesting, expression depth and the size of expanded gate definitions are bounded, so hostile input fails with a message.

Every circuit Qiskit exports for the benchmarks, in both versions, `qirc diff` finds identical to the same circuit built as QIR.
