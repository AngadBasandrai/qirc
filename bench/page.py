import argparse
import subprocess
import sys
from datetime import date
from pathlib import Path

import pytket
import qiskit

ROOT = Path(__file__).resolve().parent.parent


def tables(binary, *extra):
    command = [sys.executable, str(ROOT / "bench" / "compare.py"), "--qirc", binary, *extra]
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout.strip()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--qirc", default=str(ROOT / "target" / "release" / "qirc"))
    parser.add_argument("--out", default=str(ROOT / "docs" / "src" / "benchmarks.md"))
    options = parser.parse_args()
    page = f"""# Benchmarks

Measured on {date.today().isoformat()} by the documentation build, with Qiskit {qiskit.__version__} and tket {pytket.__version__}, so these numbers always match the published compiler.

`bench/compare.py` compiles the same circuits with Qiskit (`transpile` at `optimization_level=3`), tket (`FullPeepholeOptimise`, then a rebase and squash) and qirc, all to `rz`, `sx`, `x` and `cx`. The circuits are the straight line programs in `tests/corpus` and `examples` with measurements removed, the QFT, Grover search for the all ones state, and the CDKM, VBE and Draper adders from Qiskit. Each cell is two qubit gates / total gates / depth, and the input column counts each Toffoli and SWAP as one gate. Every result up to 12 qubits is checked against the input by exact operator comparison, allowing for the qubit permutation each tool reports.

## All to all connectivity

{tables(options.qirc)}

## A line of qubits

Routed with `--coupling line:n` for qirc, `CouplingMap.from_line` for Qiskit and `DefaultMappingPass` for tket.

{tables(options.qirc, "--line")}

To run it yourself, install `qiskit` and `pytket`, build qirc in release mode and run `python bench/compare.py`, adding `--line` for the routed table.
"""
    Path(options.out).write_text(page, encoding="utf-8")


if __name__ == "__main__":
    main()
