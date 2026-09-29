# qirc

qirc is a compiler and simulator for [QIR](https://github.com/qir-alliance/qir-spec), the LLVM based intermediate representation that Q#, PyQIR and other quantum toolchains produce, and for OpenQASM 2 and 3.

It reads a program, checks it against the QIR profile it declares, optimises it, and then does one of three things with it:

- **runs it** on the fastest simulator that fits: a state vector up to 30 qubits, and above that a stabilizer tableau, a bit vector, a sum of stabilizer states or a matrix product state, picked from the gates the program uses;
- **emits it** as OpenQASM 3 or 2, QIR, Stim, OpenPulse, JSON, an IonQ job or a circuit drawing, optionally rebuilt for a hardware gate set and routed onto a coupling map;
- **checks it**, proving that the compiled program behaves exactly like the original branch by branch with `qirc diff`, or counting what each path costs with `--emit cost`.

It is one Rust binary with a single dependency, and the same compiler runs in Python, in any editor through a language server, and in the browser.

```text
$ qirc bell.ll --shots 1000
program: 2 qubits, 2 results, 2 gates, depth 2, profile base_profile

q0: H-*-M---
q1: --+---M-

measurement over 1000 shots (sampled from the final state):
  00       515   0.5150
  11       485   0.4850
```

## Where to go next

- [Getting started](getting-started.md) installs qirc and walks through a first program.
- The [playground](https://angadbasandrai.github.io/qirc/) runs the whole compiler in your browser, with nothing to install.
- The [command line reference](reference/cli.md) lists every option.
- The [Python](python.md) and [Qiskit](qiskit.md) chapters cover using qirc from notebooks and transpiler pipelines.
