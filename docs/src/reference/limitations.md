# Limitations

- A qubit index that depends on a measurement cannot be resolved, because qubits are assigned at compile time.
- Recursion that is not a tail call is only expanded when its depth is known at compile time, up to 10,000 calls deep, or 32 in the browser.
- The state vector holds up to 30 qubits, or 20 in the browser. Above that a circuit without structure to exploit runs as a matrix product state, which is exact only while its entanglement fits the bond cap and otherwise reports the weight it dropped.
- OpenQASM 3 input reads classical values that are set once. Values that change, `bool`, `break` and subroutines are refused.
- OpenQASM 3 output writes branches as `if` and `else`, loops as a `while` over blocks, classical values as typed variables and recorded values as `output` variables. A floating point remainder, a pointer cast or a value recorded inside a loop is refused rather than approximated, and `--emit qir` keeps them.
- QIR output decomposes a controlled gate that has no QIR function of its own, and refuses one with three or more controls.
- `qirc submit` talks to IonQ only, and has been tested against a local imitation of its API rather than a live account.
- A single cross resonance pulse per CNOT, without echo or rotary tones, is a starting point to calibrate against a device rather than a finished gate.
