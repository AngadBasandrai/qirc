# Benchmarks

Measured on 2026-09-30 by the documentation build, with Qiskit 2.5.2 and tket 2.18.4, so these numbers always match the published compiler.

`bench/compare.py` compiles the same circuits with Qiskit (`transpile` at `optimization_level=3`), tket (`FullPeepholeOptimise`, then a rebase and squash) and qirc, all to `rz`, `sx`, `x` and `cx`. The circuits are the straight line programs in `tests/corpus` and `examples` with measurements removed, the QFT, Grover search for the all ones state, and the CDKM, VBE and Draper adders from Qiskit. Each cell is two qubit gates / total gates / depth, and the input column counts each Toffoli and SWAP as one gate. Every result up to 12 qubits is checked against the input by exact operator comparison, allowing for the qubit permutation each tool reports.

## All to all connectivity

| circuit | qubits | input | Qiskit | tket | qirc -O3 --gates rz-sx-cx --resynth 4 --relabel | qirc -O3 --gates rz-sx-cx --resynth 4 --relabel --cost cx |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| redundant | 4 | 2 / 13 / 5 | 0 / 3 / 2 | 0 / 3 / 2 | 0 / 3 / 2 | 0 / 3 / 2 |
| small | 2 | 1 / 3 / 2 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 |
| pyqir_real | 3 | 1 / 4 / 2 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 |
| pyqir_simple | 3 | 2 / 12 / 7 | 6 / 27 / 18 | 6 / 36 / 22 | 6 / 24 / 14 | 6 / 24 / 14 |
| qsharp_loop | 4 | 3 / 4 / 4 | 3 / 6 / 6 | 3 / 6 / 6 | 3 / 6 / 6 | 3 / 6 / 6 |
| unrestricted_dynamic | 5 | 4 / 6 / 5 | 4 / 8 / 7 | 4 / 8 / 7 | 4 / 8 / 7 | 4 / 8 / 7 |
| qft_4 | 4 | 14 / 36 / 24 | 12 / 33 / 23 | 12 / 33 / 23 | 12 / 34 / 23 | 12 / 34 / 23 |
| qft_6 | 6 | 33 / 84 / 40 | 30 / 73 / 37 | 30 / 73 / 37 | 30 / 71 / 35 | 30 / 71 / 35 |
| qft_8 | 8 | 60 / 152 / 56 | 56 / 129 / 51 | 56 / 129 / 51 | 56 / 120 / 47 | 56 / 120 / 47 |
| qft_10 | 10 | 95 / 240 / 72 | 90 / 201 / 65 | 90 / 201 / 65 | 90 / 181 / 59 | 90 / 181 / 59 |
| grover_3 | 3 | 4 / 39 / 21 | 24 / 85 / 53 | 20 / 140 / 86 | 24 / 89 / 55 | 23 / 154 / 95 |
| grover_4 | 4 | 84 / 250 / 171 | 84 / 228 / 166 | 84 / 243 / 183 | 84 / 234 / 171 | 84 / 234 / 171 |
| grover_5 | 5 | 288 / 941 / 681 | 288 / 788 / 607 | 288 / 849 / 662 | 288 / 732 / 603 | 288 / 732 / 603 |
| adder_cdkm_4 | 9 | 24 / 24 / 21 | 64 / 151 / 114 | 47 / 130 / 88 | 44 / 107 / 84 | 44 / 107 / 84 |
| adder_vbe_3 | 8 | 13 / 13 / 11 | 37 / 87 / 54 | 37 / 87 / 56 | 25 / 63 / 41 | 25 / 85 / 55 |
| adder_draper_4 | 8 | 48 / 122 / 74 | 40 / 123 / 67 | 40 / 107 / 57 | 43 / 94 / 53 | 40 / 110 / 60 |
| total |  |  | 740 / 1952 / 1278 | 719 / 2055 / 1353 | 711 / 1776 / 1208 | 707 / 1879 / 1269 |

compile time: Qiskit 0.17 s, tket 17.35 s, qirc -O3 --gates rz-sx-cx --resynth 4 --relabel 0.74 s, qirc -O3 --gates rz-sx-cx --resynth 4 --relabel --cost cx 0.87 s

## A line of qubits

Routed with `--coupling line:n` for qirc, `CouplingMap.from_line` for Qiskit and `DefaultMappingPass` for tket.

| circuit | qubits | input | Qiskit | tket | qirc -O3 --gates rz-sx-cx --resynth 4 --relabel | qirc -O3 --gates rz-sx-cx --resynth 4 --relabel --cost cx |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| redundant | 4 | 2 / 13 / 5 | 0 / 3 / 2 | 0 / 3 / 2 | 0 / 3 / 2 | 0 / 3 / 2 |
| small | 2 | 1 / 3 / 2 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 |
| pyqir_real | 3 | 1 / 4 / 2 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 | 1 / 5 / 4 |
| pyqir_simple | 3 | 2 / 12 / 7 | 7 / 37 / 24 | 9 / 39 / 25 | 7 / 25 / 15 | 7 / 26 / 16 |
| qsharp_loop | 4 | 3 / 4 / 4 | 3 / 6 / 6 | 3 / 6 / 6 | 3 / 6 / 6 | 3 / 6 / 6 |
| unrestricted_dynamic | 5 | 4 / 6 / 5 | 8 / 12 / 11 | 11 / 15 / 11 | 6 / 10 / 9 | 6 / 10 / 9 |
| qft_4 | 4 | 14 / 36 / 24 | 19 / 49 / 33 | 24 / 45 / 35 | 19 / 41 / 29 | 19 / 41 / 29 |
| qft_6 | 6 | 33 / 84 / 40 | 51 / 143 / 69 | 72 / 115 / 71 | 51 / 92 / 54 | 45 / 109 / 55 |
| qft_8 | 8 | 60 / 152 / 56 | 90 / 312 / 113 | 137 / 210 / 103 | 97 / 161 / 81 | 83 / 193 / 82 |
| qft_10 | 10 | 95 / 240 / 72 | 147 / 514 / 158 | 222 / 333 / 135 | 149 / 240 / 106 | 135 / 285 / 104 |
| grover_3 | 3 | 4 / 39 / 21 | 37 / 128 / 91 | 35 / 155 / 98 | 47 / 110 / 88 | 35 / 194 / 110 |
| grover_4 | 4 | 84 / 250 / 171 | 165 / 402 / 294 | 171 / 330 / 259 | 209 / 365 / 263 | 171 / 542 / 359 |
| grover_5 | 5 | 288 / 941 / 681 | 543 / 1409 / 893 | 639 / 1200 / 840 | 681 / 1123 / 867 | 597 / 1407 / 974 |
| adder_cdkm_4 | 9 | 24 / 24 / 21 | 93 / 251 / 184 | 86 / 169 / 134 | 78 / 141 / 123 | 66 / 163 / 127 |
| adder_vbe_3 | 8 | 13 / 13 / 11 | 66 / 160 / 101 | 79 / 129 / 91 | 52 / 90 / 64 | 44 / 151 / 85 |
| adder_draper_4 | 8 | 48 / 122 / 74 | 90 / 202 / 117 | 112 / 179 / 119 | 86 / 137 / 76 | 81 / 168 / 85 |
| total |  |  | 1321 / 3638 / 2104 | 1602 / 2938 / 1937 | 1487 / 2554 / 1791 | 1294 / 3308 / 2051 |

compile time: Qiskit 0.24 s, tket 17.88 s, qirc -O3 --gates rz-sx-cx --resynth 4 --relabel 1.34 s, qirc -O3 --gates rz-sx-cx --resynth 4 --relabel --cost cx 2.63 s

To run it yourself, install `qiskit` and `pytket`, build qirc in release mode and run `python bench/compare.py`, adding `--line` for the routed table.
