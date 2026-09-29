# Q# and PyQIR

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
