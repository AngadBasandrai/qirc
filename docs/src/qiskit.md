# Qiskit

```text
pip install "qirc[qiskit]"
```

```python
from qiskit import transpile

compiled = transpile(circuit, basis_gates=["rz", "sx", "x", "cx"], optimization_level=3, optimization_method="qirc")
```

`optimization_method="qirc"` runs Qiskit's own optimization stage and then passes the circuit through qirc at the same level, keeping qirc's result only when it has fewer two qubit gates, or as many and fewer gates overall. Circuits with control flow are left to Qiskit. `qirc.qiskit.optimize(circuit, basis, level)` applies the same step to a single circuit.
