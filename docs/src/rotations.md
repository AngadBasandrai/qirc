# Pauli product rotations

`--emit rotations` rewrites the program in the form Litinski's Game of Surface Codes starts from: a list of rotations about multi-qubit Pauli products followed by Pauli product measurements, with every Clifford gate gone. The program is lowered to Clifford gates, T gates and rotations as for a resource estimate, and qirc walks through it keeping the image of each qubit's X and Z under the Clifford gates seen so far. A T gate on a qubit becomes a pi/8 rotation about the current image of its Z, any other rotation keeps its angle, and a measurement measures the image of Z. A new rotation is moved back past every rotation and measurement it commutes with, and when it meets one about the same Pauli product the two angles add: pi/8 and -pi/8 cancel, and two pi/8 rotations make a Clifford, which is folded into the images of the gates that follow.

```text
$ qirc mod5_4.qasm --emit rotations
Pauli product rotations for main, 5 qubits

layer  step
    1  +pi/8     Z0 Z3 X4
    1  -pi/8     Z0 Z3
    1  +pi/8     Z2 Z3 X4
    1  -pi/8     Z2 Z3
    1  +pi/8     Z1 Z2 X4
    1  -pi/8     Z1 Z2
    1  +pi/8     Z0 Z1 X4
    1  -pi/8     Z0 Z1

8 pi/8 rotations in T depth 1, 0 other rotations, 0 measurements, 1 layer in all
10 rotations merged with an earlier one about the same Pauli product
a rotation by a about P applies exp(-i a P), and the Clifford gates are folded into the rotations and measurements
```

Each step goes in the layer after the last earlier step it does not commute with, so the steps within a layer commute and can run in any order, and the T depth counts the layers that hold pi/8 rotations. `--emit resources` reports it too. On the eleven Clifford+T benchmarks from the optimisation section, merging rotations this way reaches from `-O0` the same 938 T gates that phase teleportation and PyZX reach, circuit by circuit, with T depths from 1 for `mod5_4` and `gf2^4_mult` to 17 for `tof_10`, each in about a tenth of a second. The tests check on random five qubit circuits with arbitrary rotations that the rotations and measurements give the same outcome distribution as the circuit. A program with branches has no single list of rotations, and a reset after a qubit is used needs a correction that depends on a measurement, so both are refused. Qubits that are never measured are left with a Clifford gate at the end, which the list does not show.
