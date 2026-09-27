import argparse
import json
import re
import math
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from pytket import Circuit, OpType
from pytket.architecture import Architecture
from pytket.passes import (
    AutoRebase,
    AutoSquash,
    DefaultMappingPass,
    FullPeepholeOptimise,
    RemoveRedundancies,
    SequencePass,
)
from qiskit import QuantumCircuit, transpile
from qiskit.circuit.library import PermutationGate, grover_operator
from qiskit.quantum_info import Operator
from qiskit.synthesis import adder_qft_d00, adder_ripple_c04, adder_ripple_v95, synth_qft_full
from qiskit.transpiler import CouplingMap

ROOT = Path(__file__).resolve().parent.parent
INPUT_BASIS = ["h", "x", "y", "z", "s", "sdg", "t", "tdg", "rx", "ry", "rz", "cx", "cz", "swap", "ccx"]
TARGET_BASIS = ["rz", "sx", "x", "cx"]
CHECK_QUBITS = 12

QIR_NAMES = {
    "h": "h__body",
    "x": "x__body",
    "y": "y__body",
    "z": "z__body",
    "s": "s__body",
    "sdg": "s__adj",
    "t": "t__body",
    "tdg": "t__adj",
    "rx": "rx__body",
    "ry": "ry__body",
    "rz": "rz__body",
    "cx": "cnot__body",
    "cz": "cz__body",
    "swap": "swap__body",
    "ccx": "ccx__body",
}

TKET_NAMES = {
    OpType.Rz: "rz",
    OpType.SX: "sx",
    OpType.X: "x",
    OpType.CX: "cx",
    OpType.H: "h",
    OpType.Y: "y",
    OpType.Z: "z",
    OpType.S: "s",
    OpType.Sdg: "sdg",
    OpType.T: "t",
    OpType.Tdg: "tdg",
    OpType.Rx: "rx",
    OpType.Ry: "ry",
    OpType.CZ: "cz",
    OpType.SWAP: "swap",
    OpType.CCX: "ccx",
}
TKET_TYPES = {name: kind for kind, name in TKET_NAMES.items()}


def unrolled(circuit):
    return transpile(circuit, basis_gates=INPUT_BASIS, optimization_level=0)


def qft(n):
    return unrolled(synth_qft_full(n))


def grover(n):
    oracle = QuantumCircuit(n)
    oracle.h(n - 1)
    oracle.mcx(list(range(n - 1)), n - 1)
    oracle.h(n - 1)
    step = grover_operator(oracle)
    circuit = QuantumCircuit(n)
    circuit.h(range(n))
    for _ in range(math.floor(math.pi / 4 * math.sqrt(2**n))):
        circuit.compose(step, inplace=True)
    return unrolled(circuit)


def adder(synthesis, n):
    return unrolled(synthesis(n, kind="fixed"))


def ops_of_qiskit(circuit):
    ops = []
    for instruction in circuit.data:
        name = instruction.operation.name
        if name in ("measure", "barrier", "reset"):
            continue
        qubits = [circuit.find_bit(q).index for q in instruction.qubits]
        ops.append((name, qubits, [float(p) for p in instruction.operation.params]))
    return ops


def ops_of_tket(circuit):
    ops = []
    for command in circuit.get_commands():
        name = TKET_NAMES[command.op.type]
        params = [float(p) * math.pi for p in command.op.params]
        ops.append((name, [q.index[0] for q in command.qubits], params))
    return ops


def ops_of_qirc(program):
    ops = []
    for block in program["blocks"]:
        for op in block["ops"]:
            if op["op"] != "gate":
                continue
            name = "c" * len(op["controls"]) + op["name"]
            ops.append((name, op["controls"] + op["targets"], [float(p) for p in op["params"]]))
    return ops


def circuit_of(ops, qubits):
    circuit = QuantumCircuit(qubits)
    for name, wires, params in ops:
        getattr(circuit, name)(*params, *wires)
    return circuit


def tket_of(ops, qubits):
    circuit = Circuit(qubits)
    for name, wires, params in ops:
        circuit.add_gate(TKET_TYPES[name], [p / math.pi for p in params], wires)
    return circuit


def qir_of(ops, qubits):
    body = []
    for name, wires, params in ops:
        args = [f"double {p!r}" for p in params]
        args += [f"%Qubit* inttoptr (i64 {w} to %Qubit*)" for w in wires]
        body.append(f"  call void @__quantum__qis__{QIR_NAMES[name]}({', '.join(args)})")
    used = sorted({(name, len(params), len(wires)) for name, wires, params in ops})
    declarations = [
        f"declare void @__quantum__qis__{QIR_NAMES[name]}({', '.join(['double'] * count + ['%Qubit*'] * width)})"
        for name, count, width in used
    ]
    return "\n".join(
        [
            "%Qubit = type opaque",
            "",
            "define void @main() #0 {",
            "entry:",
            *body,
            "  ret void",
            "}",
            "",
            *declarations,
            "",
            f'attributes #0 = {{ "entry_point" "qir_profiles"="base_profile" "required_num_qubits"="{qubits}" }}',
            "",
        ]
    )


def metrics(ops):
    frontier = {}
    for _, wires, _ in ops:
        level = max(frontier.get(w, 0) for w in wires) + 1
        for w in wires:
            frontier[w] = level
    return {
        "cx": sum(1 for _, wires, _ in ops if len(wires) > 1),
        "gates": len(ops),
        "depth": max(frontier.values(), default=0),
    }


def run_qiskit(circuit, line):
    coupling = CouplingMap.from_line(circuit.num_qubits) if line else None
    started = time.perf_counter()
    out = transpile(
        circuit,
        basis_gates=TARGET_BASIS,
        coupling_map=coupling,
        optimization_level=3,
        seed_transpiler=11,
    )
    elapsed = time.perf_counter() - started
    ok = Operator.from_circuit(out).equiv(Operator(circuit)) if checkable(circuit, line) else None
    return ops_of_qiskit(out), elapsed, ok


def run_tket(circuit, line):
    tk = tket_of(ops_of_qiskit(circuit), circuit.num_qubits)
    passes = [FullPeepholeOptimise()]
    if line:
        n = circuit.num_qubits
        passes.append(DefaultMappingPass(Architecture([(i, i + 1) for i in range(n - 1)])))
    passes += [
        AutoRebase({OpType.CX, OpType.Rz, OpType.SX, OpType.X}),
        AutoSquash({OpType.Rz, OpType.SX}),
        RemoveRedundancies(),
    ]
    started = time.perf_counter()
    SequencePass(passes).apply(tk)
    elapsed = time.perf_counter() - started
    ops = ops_of_tket(tk)
    moved = {a.index[0]: b.index[0] for a, b in tk.implicit_qubit_permutation().items()}
    pattern = [moved.get(q, q) for q in range(circuit.num_qubits)]
    ok = permuted(circuit, ops, pattern) if checkable(circuit, line) else None
    return ops, elapsed, ok


def run_qirc(circuit, line, binary, flags):
    ops = ops_of_qiskit(circuit)
    n = circuit.num_qubits
    with tempfile.TemporaryDirectory() as folder:
        source = Path(folder) / "input.ll"
        source.write_text(qir_of(ops, n))
        command = [str(binary), str(source), "--emit", "json", "-v", *flags]
        if line:
            command += ["--coupling", f"line:{n}"]
        started = time.perf_counter()
        done = subprocess.run(command, capture_output=True, text=True)
        elapsed = time.perf_counter() - started
    if done.returncode != 0:
        raise RuntimeError(done.stderr)
    ops = ops_of_qirc(json.loads(done.stdout))
    layout = re.search(r"relabelling removed \d+ swap\(s\), final layout \[([\d, ]*)\]", done.stderr)
    pattern = [int(q) for q in layout.group(1).split(",")] if layout else list(range(n))
    ok = permuted(circuit, ops, pattern) if checkable(circuit, line) else None
    return ops, elapsed, ok


def permuted(original, ops, pattern):
    target = Operator(original)
    for order in (pattern, [pattern.index(q) for q in range(len(pattern))]):
        candidate = circuit_of(ops, original.num_qubits)
        candidate.append(PermutationGate(order), range(original.num_qubits))
        if Operator(candidate).equiv(target):
            return True
    return False


def checkable(circuit, line):
    return not line and circuit.num_qubits <= CHECK_QUBITS


def corpus(binary):
    found = []
    tracked = subprocess.run(
        ["git", "ls-files", "tests/corpus/*.ll", "examples/*.ll"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    ).stdout.split()
    for path in sorted(ROOT / name for name in tracked):
        done = subprocess.run(
            [str(binary), str(path), "--emit", "json", "-O0"], capture_output=True, text=True
        )
        if done.returncode != 0:
            continue
        program = json.loads(done.stdout)
        ops = [op for block in program["blocks"] for op in block["ops"]]
        straight = len(program["blocks"]) == 1 and all(op["op"] in ("gate", "measure") for op in ops)
        gates = ops_of_qirc(program)
        if not straight or not gates or program["qubits"] > 16:
            continue
        if any(name not in QIR_NAMES for name, _, _ in gates):
            continue
        found.append((path.stem, circuit_of(gates, program["qubits"])))
    return found


def cell(ops, ok):
    m = metrics(ops)
    mark = " (wrong)" if ok is False else ""
    return f"{m['cx']} / {m['gates']} / {m['depth']}{mark}"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--qirc", default=str(ROOT / "target" / "release" / "qirc"))
    parser.add_argument("--line", action="store_true")
    parser.add_argument("--flags", action="append")
    options = parser.parse_args()
    binary = Path(options.qirc)
    configs = options.flags or [
        "-O3 --gates rz-sx-cx --resynth 4 --relabel",
        "-O3 --gates rz-sx-cx --resynth 4 --relabel --cost cx",
    ]

    circuits = corpus(binary)
    circuits += [(f"qft_{n}", qft(n)) for n in (4, 6, 8, 10)]
    circuits += [(f"grover_{n}", grover(n)) for n in (3, 4, 5)]
    circuits += [
        ("adder_cdkm_4", adder(adder_ripple_c04, 4)),
        ("adder_vbe_3", adder(adder_ripple_v95, 3)),
        ("adder_draper_4", adder(adder_qft_d00, 4)),
    ]

    tools = [
        ("Qiskit", lambda c: run_qiskit(c, options.line)),
        ("tket", lambda c: run_tket(c, options.line)),
        *[
            (f"qirc {flags}", lambda c, flags=flags: run_qirc(c, options.line, binary, flags.split()))
            for flags in configs
        ],
    ]
    rows = []
    totals = {name: {"cx": 0, "gates": 0, "depth": 0, "time": 0.0} for name, _ in tools}
    for name, circuit in circuits:
        base = metrics(ops_of_qiskit(circuit))
        row = [name, str(circuit.num_qubits), f"{base['cx']} / {base['gates']} / {base['depth']}"]
        for tool, run in tools:
            ops, elapsed, ok = run(circuit)
            row.append(cell(ops, ok))
            for key, value in metrics(ops).items():
                totals[tool][key] += value
            totals[tool]["time"] += elapsed
        rows.append(row)
        print(" | ".join(row), file=sys.stderr)

    header = ["circuit", "qubits", "input", *[tool for tool, _ in tools]]
    print("| " + " | ".join(header) + " |")
    print("| " + " | ".join(["---"] * 2 + ["---:"] * (len(header) - 2)) + " |")
    for row in rows:
        print("| " + " | ".join(row) + " |")
    summary = ["total", "", ""]
    for tool, _ in tools:
        t = totals[tool]
        summary.append(f"{t['cx']} / {t['gates']} / {t['depth']}")
    print("| " + " | ".join(summary) + " |")
    print()
    print("compile time: " + ", ".join(f"{tool} {totals[tool]['time']:.2f} s" for tool, _ in tools))


if __name__ == "__main__":
    main()
