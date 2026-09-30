# Noise and calibration

A [calibration file](reference/calibration.md) describes a device. qirc uses it to route around poor couplers, to estimate the chance a program succeeds, to simulate the device's noise, and to undo or extrapolate away that noise.

## Routing and success estimates

`--calibration file` reads device error rates, one per line as `cx a b error`, `single q error` or `readout q error`, and routes onto the coupling map its `cx` lines describe unless `--coupling` is also given. The router then measures distance by the error of each coupler, prefers SWAPs on good couplers and keeps the layout with the highest estimated success probability, and `--emit cost` adds a `success` column with the product of one minus the error of every operation on each path. `examples/line5.cal` describes a five qubit line with one poor coupler.

A calibration can also give durations in nanoseconds as `time cx a b ns`, `time single q ns` and `time readout q ns`, and relaxation and dephasing times in microseconds as `t1 q us` and `t2 q us`. With durations, `--emit cost` schedules every operation as early as its qubits allow and adds a `time us` column, and the success estimate also charges each qubit for the time it waits between operations, using the Pauli twirl of amplitude and phase damping: X and Y each with probability (1 - e^(-t/T1))/4 and Z with (1 - e^(-t/T2))/2 minus that. A qubit is only charged once it has been used, since the ground state does not decay. `--noisy` applies the same idle errors during simulation, keeping a clock per qubit in every shot.

## Noisy simulation

With `--calibration` and `--noisy`, every gate is followed by a random Pauli error with the probability the calibration gives for it, uniform over the nonidentity Paulis on its qubits, and every measurement result flips with the readout error of its qubit. Pauli errors are Clifford, so noisy Clifford programs still run on the tableau. For a 5 qubit GHZ state on `examples/line5.cal` the fraction of correct shots is 0.885 against a `--emit cost` estimate of 0.861, which counts every error as fatal.

## Readout mitigation

`--mitigate` undoes readout errors in the measurement counts, for a noisy simulation or for counts from a device described by the same calibration. Each result bit's flip matrix is inverted and applied across the distribution, negative quasi probabilities are clipped and the rest is renormalised. On a 5 qubit GHZ state with readout errors of 5 to 15 percent, 59 percent of raw shots are correct and the mitigated distribution puts 0.499 and 0.492 on the two correct outcomes.

## Zero noise extrapolation

`--zne` with `--observable` and `--calibration` runs zero noise extrapolation. The program is simulated with the calibration's errors at one, two and three times their rates, with T1 and T2 shortened to match, the expectation value is averaged over the final state of every shot, and a quadratic through the three points is extended to zero noise, which is 3 E1 - 3 E2 + E3. For `Z0 Z4` on a 5 qubit GHZ state on `examples/line5.cal`, the exact value is 1, one times noise gives 0.938 and the extrapolation gives 0.993.

## Detuning and dynamical decoupling
