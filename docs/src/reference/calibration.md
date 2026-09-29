# Calibration files

A calibration describes a device: which qubit pairs are coupled, how often each operation fails, how long it takes, how fast qubits decay and what frequencies drive them. `--calibration file` reads one, and it feeds routing, `--emit cost`, `--noisy`, `--mitigate`, `--zne`, `--emit schedule` and `--emit pulse`.

The file has one fact per line. Blank lines and lines starting with `#` are ignored, and qubits are numbered from 0.

| Line | Meaning | Used by |
| --- | --- | --- |
| `cx a b error` | qubits `a` and `b` are coupled, and a two qubit gate on them fails with probability `error` | routing, cost, noise |
| `single q error` | a one qubit gate on `q` fails with probability `error` | cost, noise |
| `readout q error` | a measurement of `q` reports the wrong bit with probability `error` | cost, noise, `--mitigate` |
| `time cx a b ns` | a two qubit gate on `a` and `b` takes `ns` nanoseconds | cost, noise, schedules |
| `time single q ns` | a one qubit gate on `q` takes `ns` nanoseconds | cost, noise, schedules |
| `time readout q ns` | a measurement or reset of `q` takes `ns` nanoseconds | cost, noise, schedules |
| `t1 q us` | relaxation time of `q` in microseconds | idle noise |
| `t2 q us` | dephasing time of `q` in microseconds | idle noise |
| `frequency q GHz` | drive frequency of `q` | pulses |
| `drive q amplitude beta` | DRAG amplitude and coefficient of the `sx` pulse on `q`, 0.2 and 0 by default | pulses |
| `cross a b amplitude` | amplitude of the cross resonance pulse from `a` to `b`, 0.1 by default | pulses |
| `resonator q GHz [amplitude]` | readout resonator of `q`, with a tone amplitude of 0.1 by default | pulses |

Error rates must be at least 0 and below 1. Unless `--coupling` is also given, the `cx` lines are the coupling map, and the router measures distance by their error rates.

## Timing

Gates that only add phases, such as `rz`, `s`, `t` and `z`, take no time: hardware applies them as a frame change. A swap costs three CNOTs on its edge, and a gate on three or more qubits is charged two CNOTs for every pair it touches.

With durations and T1 and T2, a qubit that waits between operations picks up the Pauli twirl of amplitude and phase damping: X and Y each with probability (1 - e^(-t/T1))/4, and Z with (1 - e^(-t/T2))/2 minus that. A qubit is only charged once it has been used, since the ground state does not decay.

## Example

`examples/line5.cal` in the repository is a five qubit line with one poor coupler:

```text
# a five qubit line with one poor coupler, error rates per operation
cx 0 1 0.041
cx 1 2 0.006
cx 2 3 0.007
cx 3 4 0.005
single 0 0.0004
readout 0 0.031
time cx 0 1 420
time single 0 35
time readout 0 800
t1 0 45
t2 0 30
frequency 0 5.02
resonator 0 7.12
```

The file itself has these lines for every qubit.
