# Optimisation

| Level | Passes |
| --- | --- |
| `-O0` | none |
| `-O1` | identity removal, inverse cancellation, rotation merging, constant folding, dead code elimination |
| `-O2` | `-O1` plus peephole rewrites, phase folding, phase teleportation and CFG simplification, repeated until nothing changes, then window resynthesis |
| `-O3` | `-O2` plus single qubit gate fusion |

Cancellation, merging and fusion look past gates that commute with the pair, so `rz` on a control qubit cancels across a CNOT. Commutation is checked on the exact matrices of the gates involved.

Phase folding tracks which parity of path variables each qubit holds as CNOTs, X gates and swaps move values around, starting a new variable at every H. Two Z rotations (`rz`, `r1`, `t`, `s`, `z` and their inverses) that act on the same parity are merged into one at the place of the first, however far apart they are, and a rotation on a constant parity is a global phase and is dropped. Diagonal gates such as CZ leave the parities alone, and any other gate, a measurement or a reset ends the region. On Clifford+T adders this removes up to three quarters of the T gates that the other passes leave, for example 34 to 8 on a 3 bit VBE adder.

Phase teleportation, from Kissinger and van de Wetering, finds rotations that act on the same parity even when Hadamards separate them. Each straight run of Clifford gates and rotations becomes a ZX diagram in which every rotation that is not a multiple of a quarter turn is a variable, and the diagram is simplified with spider fusion, identity removal, local complementation, pivoting, and the fusion of phase gadgets with the same targets. Two variables that end up on the same spider or the same gadget can be moved onto one gate with their signs, so one rotation takes the sum and the other is removed. The diagram is only used to find these pairs: no circuit is extracted from it, so every other gate stays where it was. On eleven standard Clifford+T benchmark circuits this matches PyZX exactly, taking the 988 T gates left after phase folding down to 938, with `adder_8` going from 215 to 173 and `mod5_4` from 16 to 8, and `qirc diff` proves every result of up to 20 qubits equivalent.

Window resynthesis multiplies out every run of gates on one or two qubits and replaces the run with anything shorter that has the same matrix up to global phase. By default the replacement must be a single gate, so `t t` becomes `s` and three alternating CNOTs become `swap`. `--resynth n` allows replacements of up to `n` gates: single qubit runs use exact Euler angles, and two qubit runs use a meet in the middle search over the target gate set. Every replacement is checked against the original matrix before it is applied.

With `--resynth 2` or more, or any `--cost`, two qubit runs are also rebuilt from their KAK decomposition with the fewest CNOTs the matrix needs: none for a product of single qubit gates, one when it is locally a CNOT, two when one of its three interaction coordinates is zero, and three otherwise. The result can be longer than `n` gates and is kept only when it is cheaper than the run it replaces. In the same mode the window grows to 96 gates: the longest two qubit run gets one KAK attempt, and a three qubit run with more than 21 two qubit gates is rebuilt by quantum Shannon decomposition. That splits the matrix around its top qubit with a cosine sine decomposition into multiplexed rotations and four two qubit blocks, builds the first three blocks only up to a diagonal so each needs two CNOTs, carries each diagonal into the next block, and needs at most 21 CNOTs in total.

`--cost` sets what cheaper means. `gates` counts gates and breaks ties on two qubit gates, `cx` counts two qubit gates first, and `ibm` charges 10 for a two qubit gate, 1 for other single qubit gates and nothing for `rz`, `s`, `t` and other Z rotations, which IBM hardware applies as frame changes.

On `tests/corpus/pyqir_simple.ll`, `-O3` takes the circuit from 12 gates at depth 7 to 8 gates at depth 4.
