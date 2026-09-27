# qirc for Python

Python bindings for [qirc](https://github.com/AngadBasandrai/qirc), a compiler and state vector simulator for QIR, the LLVM based intermediate representation used by Q#, PyQIR and other quantum toolchains.

```
pip install qirc
```

```python
import qirc

source = open("bell.ll").read()

qasm = qirc.compile(source, emit="qasm3", opt=3, gates="rz-sx-cx", coupling="line:4")
counts = qirc.run(source, shots=1000, seed=7)
verdict = qirc.diff(source, opt=3, gates="rz-sx-cx", resynth=4)
report = qirc.cost(source, gates="rz-sx-cx")
```

`compile` returns the emitted text for any `emit` kind: `qir`, `qasm3`, `json`, `ir`, `circuit`, `cost`, `check` or `run`. `run` returns the measurement counts as a dictionary. `diff` compares the compiled program, or a second program, with the original at `-O0` branch by branch and returns `"equivalent"`, `"different"` or `"inconclusive"`. `cost` returns the T count, two qubit gates, gates, depth and peak live qubits for every path and for the worst case.

Every function takes the same compiler options as keywords: `opt` (0 to 3), `gates`, `exclude`, `resynth`, `cost`, `coupling` and `calibration` with the values the command line accepts, and `relabel=True`. A program that does not compile raises `qirc.CompileError` with the rendered diagnostics, and warnings are reported through the `warnings` module.
