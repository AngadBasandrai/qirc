# Getting started

## Installing

```text
cargo install qirc-compiler
pip install qirc
```

The first installs the `qirc` command, the second the Python package. Every [release](https://github.com/AngadBasandrai/qirc/releases) also carries prebuilt binaries for Linux (x86_64 and arm64), macOS (Intel and Apple silicon) and Windows. The crate is called `qirc-compiler` because `qirc` was taken on crates.io; the command and the library are both still `qirc`.

To build from source you need Rust 1.88 or newer:

```text
git clone https://github.com/AngadBasandrai/qirc
cd qirc
cargo build --release
```

## A first program

Save this as `bell.qasm`:

```text
OPENQASM 3.0;
include "stdgates.inc";
qubit[2] q;
bit[2] c;
h q[0];
cx q[0], q[1];
c = measure q;
```

Running a file simulates it. With no options qirc prints the circuit, the final state and the outcome distribution:

```text
$ qirc bell.qasm --shots 1000 --seed 7
```

`--emit` turns it into something else. Here it becomes QIR for an IBM style gate set:

```text
$ qirc bell.qasm --emit qir -O2 --gates rz-sx-cx
```

and here OpenQASM 3 routed onto a line of three qubits:

```text
$ qirc bell.qasm --emit qasm3 --gates rz-sx-cx --coupling line:3
```

`qirc diff` proves the compiled program still does the same thing:

```text
$ qirc diff bell.qasm -O3 --gates rz-sx-cx --resynth 4
equivalent: 2 outcomes agree in probability and final state
```

and `--emit cost` counts what it costs:

```text
$ qirc bell.qasm --emit cost --gates rz-sx-cx
```

QIR files work the same way. The repository's `tests/corpus/` holds programs from the Q# compiler, PyQIR and hand written QIR in every profile to try.

## When something is wrong

Errors point at the source, with a code you can look up in the [diagnostics reference](reference/diagnostics.md):

```text
error[QIR0300]: the Base Profile forbids branching
  --> teleport.ll:11:1
   |
11 | entry:
   | ^^^^^^ this program has more than one basic block
   |
   = note: branching needs the Adaptive Profile
```

The exit code is 0 on success, 1 on an error and 2 for bad arguments or an inconclusive `diff`.
