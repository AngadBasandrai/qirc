# Resource estimation

`--emit resources` estimates what the compiled program would need on a fault tolerant machine built from surface codes.

```text
$ qirc adder_8.qasm --emit resources -O2
logical qubits    24
T gates           173
T depth           12, as layers of commuting Pauli product rotations
magic states      173
logical steps     173
code distance     15
distillation      1 level of 15-to-1, 11 factories of 11 tiles
physical qubits   82,800 (28,350 for data, 54,450 for factories)
runtime           1.104 ms, 2,760 cycles of 400 ns
error budget      0.001: 5.0e-4 logical, 5.0e-4 magic states
```

The program is lowered to Clifford gates, T gates and rotations, and the worst path's peak live qubits, T gates and rotations are counted, so phase teleportation's savings carry straight through. Each rotation costs about 3 log2(1/e) T gates, as gridsynth needs, with e its share of the budget. The logical qubits sit in a fast block of 2n + ⌈√(8n)⌉ + 1 tiles, where each T gate takes one logical step of d code cycles, and 15-to-1 distillation factories of 11 tiles, taking 11 steps per state, supply one magic state per step, with a second or third level when the first is not clean enough. The failure budget, 0.001 by default or `--budget`, is split evenly between logical errors, distillation and rotation synthesis, and the code distance is the smallest odd d for which every tile over every cycle stays within its share at a logical error rate of 0.03 (p/0.01)^((d+1)/2). The physical error rate p and the cycle time come from the calibration's CNOT errors and 4 CNOT times plus 2 readout times, or default to 0.001 and 400 ns, the gate based figures Azure's resource estimator also uses. A physical error rate at the threshold of 0.01 or above has no surface code estimate.
