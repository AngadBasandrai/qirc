# Targeting hardware

`--gates` rebuilds every gate from a target set: a preset such as `rz-sx-cx` for IBM style devices or `rz-ry-cz`, or any list of gates such as `rx,ry,cy`. `--exclude` removes gates from the full QIR set instead, and `--basis` is kept as a name for `--gates`. Multi qubit gates reduce to CNOTs and single qubit matrices first. The CNOT is then mapped onto whichever entangler the set has, and each single qubit matrix is rebuilt from Euler angles over two rotation axes, from one axis plus a fixed gate, or by an exact search over fixed gates such as `h,s,t`. A gate the set cannot express exactly is reported and left as written. A Toffoli that a later identical Toffoli undoes, with only gates in between that use its three qubits as controls or only add phases to them, is lowered together with its partner as a pair of relative phase Toffolis, each 3 CNOTs and 4 T gates instead of 6 and 7, because the phases the two halves add cancel. A six control X gate built from a ladder of Toffolis on ancillas drops from 63 T gates and 54 CNOTs to 39 and 30 on `h,s,t,cx`. On 600 random programs full of such pairs, with phases, rotations and conflicting gates in between, `qirc diff` finds every result equivalent.

`--epsilon e` makes a finite set such as `h,s,t,cx` usable for any angle. Every gate the set cannot express exactly is rebuilt from H, S and T to within e in operator norm, its Euler rotations sharing the precision. Each rotation is found as in Ross and Selinger's gridsynth: the candidates are exact Clifford+T unitaries whose top left entry lies in a thin region around the rotation, drawn from the grid of the ring Z[√2], and a candidate is kept once the norm equation for its other entry can be solved by factoring over Z[√2] and Z[ω]. The T count grows as about 3 log2(1/e), and qirc reaches e down to about 1e-10: below about 1e-7 the region is thinner than a double can resolve next to 1, so it is searched in double-double arithmetic, about 106 bits, and the norm equations are factored over 128 bit integers. A rotation takes milliseconds at 1e-8, about 0.4 seconds at 1e-9 and 0.5 seconds at 1e-10, with 102 T gates at 1e-10. A circuit with an `rz(0.3)` and a controlled `rz(0.45)` needs 82 T gates at 1e-2 and 156 at 1e-4.

```text
$ qirc tests/corpus/base_profile_bell.ll --exclude h,cx --emit ir
  x q0
  ry(-1.5707963267948966) q0
  x q1
  ry(-1.5707963267948966) q1
  cz q0, q1
  x q1
  ry(-1.5707963267948966) q1
```

`--coupling` inserts SWAPs so that every two qubit gate lands on a physical edge, and remaps measurements so results come back under their original labels. The router works on the dependency graph of each block: gates whose qubits are adjacent run as soon as they are ready, and otherwise it picks the SWAP that most shortens the blocked gates plus the next 20, weighted by half and fading with distance, with a small penalty on qubits that were just swapped. A SWAP on a pair that has just run a two qubit gate is preferred, because resynthesis can merge the two. The starting layout comes from routing the circuit forwards and backwards twice from up to sixteen starting points, with a little random tie breaking in all but the first, and the router keeps the result with the lowest estimated CNOT count, counting a SWAP that resynthesis can merge as one CNOT instead of three. After routing, the program is rebuilt in the gate set and resynthesised again. Gates on three or more qubits are split into one and two qubit gates before routing when no gate set is given.

In a program with branches or loops every block starts from the same layout, and a block that jumps elsewhere swaps its qubits back before the jump, so each successor sees the layout it expects.

```text
$ qirc tests/corpus/qsharp_loop.ll --emit qasm3 --basis rz-sx-cx --coupling line:4
OPENQASM 3.0;
include "stdgates.inc";

qubit[4] q;
bit[4] c;

rz(3.141592653589793) q[0];
sx q[0];
rz(-1.5707963267948966) q[0];
sx q[0];
rz(3.141592653589793) q[0];
cx q[0], q[1];
cx q[1], q[2];
cx q[2], q[3];
c[0] = measure q[0];
c[1] = measure q[1];
c[2] = measure q[2];
c[3] = measure q[3];
```

`--relabel` removes each SWAP near the end of the program by renaming the qubits of everything after it, measurements included, so recorded results are unchanged and only the final state comes out permuted. It is on whenever the program is routed, `-v` prints the final layout, and `qirc diff` then compares outcome probabilities and recorded values but not final states.

`--reuse` reorders each run of quantum operations to finish the qubits it has started before touching new ones, then packs qubit lifetimes onto as few wires as it can and resets a wire before it is reused. Resetting a qubit that is no longer used cannot change what later measurements see, so the outcome distribution is unchanged, and a Base Profile program becomes Adaptive because it now resets qubits. Bernstein Vazirani on 9 qubits runs on 2. `qirc diff` compares outcome probabilities only, as after routing.

A calibration line `detuning q kHz` gives a qubit a frequency offset, so it turns about Z by 2π times the offset for as long as it waits between operations. Unlike T1 and T2 this error is coherent, and `--noisy` applies it as a rotation. `--dd` removes it with dynamical decoupling: every idle window long enough for two X pulses gets them at a quarter and three quarters of the window, so the phase gathered before each pulse is undone after it, and the pulses carry the calibration's single qubit error. `--emit pulse` writes the pulses with their delays, and the success estimate in `--emit cost` counts the coherent error, or the pulses instead of it under `--dd`. On `examples/line5.cal` with 300 kHz of detuning on every qubit, a circuit that leaves one qubit waiting through three CNOTs comes out right in 42% of shots, and in 97% with `--dd`.

A [calibration](reference/calibration.md) routes by the error rate of each coupler instead of by distance, see [Noise and calibration](noise.md).
