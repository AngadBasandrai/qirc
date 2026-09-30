# Diagnostics

Every error and warning carries a code. The command line prints it with the source line it points at, the [language server](../editors.md) sends it to the editor, and Python raises it inside `qirc.CompileError`. `qirc explain <code>` prints a longer explanation of any of them, and a mistyped option, emit kind or gate name gets a suggestion of the nearest valid one.

## Reading the source

| Code | Meaning |
| --- | --- |
| `QIR0001` | a character that cannot start any LLVM IR token |
| `QIR0002` | a `@`, `%` or `!` with no name after it |
| `QIR0003` | a string literal that never closes |
| `QIR0100` | a syntax error in LLVM IR or OpenQASM, such as a missing token, an unknown gate or an index out of range |

## Lowering

| Code | Meaning |
| --- | --- |
| `QIR0200` | an operation qirc cannot lower, such as a qubit that cannot be resolved at compile time or recursion with no bound |
| `QIR0201` | the module has no entry point |
| `QIR0202` | the program uses more qubits or results than qirc can address |

## Checking the program

| Code | Meaning |
| --- | --- |
| `QIR0300` | the Base Profile forbids branching |
| `QIR0301` | the Base Profile forbids reading a measurement result |
| `QIR0302` | a qubit outside the declared register |
| `QIR0303` | a gate uses the same qubit twice |
| `QIR0304` | a gate has the wrong number of parameters |
| `QIR0305` | a gate has the wrong number of targets |
| `QIR0306` | a result outside the declared results |
| `QIR0307` | a branch to a block that does not exist |
| `QIR0308` | a result read before it is measured |

The profile comes from the `qir_profiles` attribute on the entry point, and the declared counts from `required_num_qubits` and `required_num_results`. A Base Profile program that needs branching can declare `adaptive_profile` instead.

## Targeting

| Code | Meaning |
| --- | --- |
| `QIR0400` | the program cannot be routed onto the coupling map, usually because it needs more qubits than the map has or the map is not connected |
| `QIR0401` | a warning that the gate set has no exact form for some gates, which are left as written; `--epsilon` approximates them instead |
| `QIR0402` | `--epsilon` cannot approximate a rotation, because the set lacks `h` or `t` or the precision is below about 1e-10 |
