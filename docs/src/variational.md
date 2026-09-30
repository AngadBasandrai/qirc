# Variational circuits

`input float theta;` or `input angle[32] theta;` declares a value that is given at compile time with `--bind theta=0.3`, so one file describes a whole family of circuits, and an input without a value is reported with the `--bind` that would fix it.

```text
$ qirc ansatz.qasm --observable "Z0 Z1 + 0.5 X0 + 0.5 X1" --minimize
minimising Z0 Z1 + 0.5 X0 + 0.5 X1 over theta, phi
...
minimum -1.414214 after 262 steps at theta = 1.570799, phi = -2.356194
```

`--gradient` prints how the `--observable` changes with each input at the bound values, from central differences of exact expectation values with a step of 1e-5, which agree with the analytic derivative to about 1e-9. The parameter shift rule is what hardware needs, where every value is sampled, but on an exact simulator it adds nothing and it breaks when an input feeds more than one gate or an expression. `--minimize` runs Adam from the bound values, or 0.1 for an input without one, until the gradient vanishes. On a two qubit ansatz of two `ry` rotations and a CNOT, it reaches -1.414214, the exact ground energy of `Z0 Z1 + 0.5 X0 + 0.5 X1`, in 262 steps and 0.7 seconds. From Python, every function takes `bind={"theta": 0.3}`.

With `--noisy` or `--zne` and a calibration, both run as they would on hardware, where every value is sampled with the calibration's errors, at least 1000 shots each. `--minimize` switches to SPSA, which moves every input at once by a random plus or minus step and measures only two values per step whatever the number of inputs, with step sizes that shrink as Spall recommends and a learning rate set from the first few slopes. It prints each tenth step with the standard error of its value and, at the end, the noisy value at the inputs found beside the exact value there without noise. `--gradient` uses the parameter shift rule, half the difference of the values a quarter turn either side, with its standard error beside the exact slope, and notes any input for which the rule does not hold. `--zne` extrapolates every value to zero noise first, at three times the shots.

```text
$ qirc ansatz.qasm --observable "Z0 Z1 + 0.5 X0 + 0.5 X1" --minimize --noisy --calibration noise.cal --seed 5
minimising Z0 Z1 + 0.5 X0 + 0.5 X1 over theta, phi by SPSA with the calibration's noise, 1000 shots per value
...
after 150 steps at theta = 1.579071, phi = -2.369776
  noisy value               -1.373005 ± 0.008590
  exact value without noise -1.414071
```

With a 3% CNOT error, SPSA finds inputs within 0.0002 of the true ground energy in 2 seconds, although the device can only show -1.373 there. With `--zne` the same run reports -1.427 ± 0.052 against an exact -1.405, so extrapolation recovers the energy within its error bar.
