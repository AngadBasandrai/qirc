# Output formats

`--emit` chooses what qirc writes, and `-o` writes it to a file. The [command line reference](reference/cli.md) lists every kind.

## OpenQASM and QIR

`--emit qasm3` writes OpenQASM 3 that keeps the program's control flow: branches become `if` and `else`, loops a `while` over blocks, classical values typed variables and recorded values `output` variables. `--emit qir` writes QIR back out as LLVM IR in the profile the program needs, so qirc can sit between a QIR producer and any QIR consumer.

`--emit qasm2` writes OpenQASM 2 with `qelib1.inc` gates for tools that only read that version. OpenQASM 2 has no `else`, loops or classical variables, so it covers straight line programs and a branch on one measurement to a block of gates, which becomes `if(c1==1) x q[2];` with one single bit register per result, as teleportation needs. Anything else is refused with a pointer to `--emit qasm3`. Every corpus program that fits is accepted by Qiskit's OpenQASM 2 parser and reads back into qirc with the same outcome probabilities.

## Stim

`--emit stim` writes a Clifford program as a [Stim](https://github.com/quantumlib/Stim) circuit for error correction tools. Rotations by quarter turns become `S`, `SQRT_X` or `SQRT_Y` gates, measurements become `M` and resets `R`, and an X, Y or Z gate that depends on one measurement becomes feedback such as `CX rec[-1] 2`. Stim numbers measurements in the order they happen, so after routing the records can come in a different order from the result numbers. With `--noisy` and a calibration the circuit carries the same noise the simulator uses: `DEPOLARIZE1` and `DEPOLARIZE2` after each gate, `M(p)` for readout errors, `X_ERROR` after a reset, and `PAULI_CHANNEL_1` for idle decoherence from T1 and T2. On 200 random programs, half with feedback and half with noise on `examples/line5.cal`, Stim's samples match qirc's to within 0.007 in total variation.

## Circuit diagrams

`--emit circuit` prints an ASCII diagram, `--emit quantikz` writes a LaTeX `quantikz` environment and `--emit svg` a standalone SVG image. The two drawings pack each operation into the earliest column its qubits allow, write angles as fractions of pi where they are, and mark where each block of a branching program starts, as a `\slice` in LaTeX and a dashed line in SVG. The LaTeX needs `\usetikzlibrary{quantikz2}` and compiles with pdflatex.

```text
$ qirc tests/corpus/base_profile_bell.ll --emit quantikz
\begin{quantikz}
\lstick{$q_{0}$} & \gate{H} & \ctrl{1} & \meter{} & \qw \\
\lstick{$q_{1}$} & \qw & \targ{} & \meter{} & \qw
\end{quantikz}
```
