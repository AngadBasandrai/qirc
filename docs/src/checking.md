# Checking a compile

`qirc diff a.ll` compiles `a.ll` twice, once at `-O0` and once with the given options, and compares them branch by branch. Every measurement splits the run into both outcomes, and each outcome must have the same probability, the same recorded values and the same final state up to global phase. `qirc diff a.ll b.ll` compares two different programs the same way.

```text
$ qirc diff tests/corpus/qsharp_teleport.ll -O3 --gates rz-sx-cx --resynth 4
equivalent: 8 outcomes agree in probability and final state
```

Branches below a probability of 1e-12 are pruned and the search stops after 20,000 runs, most probable branches first. When the unexplored probability is above 1e-9 the result is reported as inconclusive and the exit code is 2, so a loop that repeats until success is checked up to a stated remainder. With `--coupling` only the outcome probabilities are compared, because routing moves qubits.

## Cost report

`--emit cost` counts what a compiled program costs without simulating it. Every path from the entry block to a return is listed with its T gates, two qubit gates, total gates, depth and the peak number of qubits in use at once, followed by the worst case over all paths. Depth schedules each gate, measurement and reset as early as its qubits allow. A qubit is in use from its first operation until its last, and a reset frees it. A path that jumps back to a block it already passed ends there, so a loop is counted once per pass. At most 256 paths are listed.

```text
$ qirc tests/corpus/adaptive_teleport.ll --emit cost -O2 --gates rz-sx-cx
path                                            t      cx   gates   depth    live
entry > then_x > join_x > then_z > join_z       0       2      18      13       3
entry > then_x > join_x > join_z                0       2      17      13       3
entry > join_x > then_z > join_z                0       2      17      13       3
entry > join_x > join_z                         0       2      16      13       3
worst case                                      0       2      18      13       3
```
