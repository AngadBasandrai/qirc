# Surface code simulation

`qirc surface` checks the logical error rate that resource estimates assume by simulating it. For each distance it lays out a rotated surface code, measures its stabilizers for d rounds with the usual CNOT orders, which keep hook errors across the logical operator, and applies a Pauli error with probability p after every gate, reset and measurement, p coming from `--error` or from a calibration's CNOT errors. A Pauli frame simulator runs 64 shots at once in the bits of a machine word, the decoding graph is built by injecting every possible single fault and recording which checks it trips and whether it flips the logical qubit, and each shot is decoded by union find.

```text
$ qirc surface --error 0.001 --shots 200000
surface code memory, physical error 0.001 on every gate, reset and measurement

distance  rounds     shots  failures      per round          model
       3       3    200000       215       3.586e-4       3.000e-4
       5       5    200000        34       3.400e-5       3.000e-5
       7       7    200000         8       5.714e-6       3.000e-6
```

At an error rate of 0.001 the simulated logical error per round follows 0.03 (p/0.01)^((d+1)/2), the formula `--emit resources` uses, within about a factor of two. Near the threshold it does not: at 0.005 the rate falls from 7.0e-3 to only 2.9e-3 between distances 3 and 7, where the formula expects 1.9e-3, so estimates for such noisy devices are optimistic. Every single fault is corrected at distances 3 and 5, which the tests check fault by fault.
