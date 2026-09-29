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
energy = qirc.expectation(source, "Z0 Z1 + 0.5 X0")
```

`compile` returns the emitted text for any `emit` kind: `qir`, `qasm3`, `qasm2`, `stim`, `json`, `ir`, `circuit`, `quantikz`, `svg`, `cost`, `check` or `run`. `run` returns the measurement counts as a dictionary. `diff` compares the compiled program, or a second program, with the original at `-O0` branch by branch and returns `"equivalent"`, `"different"` or `"inconclusive"`. `cost` returns the T count, two qubit gates, gates, depth and peak live qubits for every path and for the worst case. `expectation` returns the exact expectation value of a sum of Pauli strings on the final state, or with a `calibration` the noisy value averaged over `shots` when `noisy=True`, or the zero noise extrapolation when `zne=True`.

Every function takes the same compiler options as keywords: `opt` (0 to 3), `gates`, `exclude`, `resynth`, `cost`, `coupling` and `calibration` with the values the command line accepts, and `relabel=True`, `reuse=True` or `noisy=True`. A program that does not compile raises `qirc.CompileError` with the rendered diagnostics, and warnings are reported through the `warnings` module.

## From Q# and PyQIR

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

## Qiskit

```
pip install "qirc[qiskit]"
```

```python
from qiskit import transpile

compiled = transpile(circuit, basis_gates=["rz", "sx", "x", "cx"], optimization_level=3, optimization_method="qirc")
```

`optimization_method="qirc"` runs Qiskit's own optimization stage and then passes the circuit through qirc at the same level, keeping qirc's result only when it has fewer two qubit gates, or as many and fewer gates overall. Circuits with control flow are left to Qiskit. `qirc.qiskit.optimize(circuit, basis, level)` applies the same step to a single circuit.
