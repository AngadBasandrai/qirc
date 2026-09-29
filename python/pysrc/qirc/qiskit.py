from qiskit import QuantumCircuit, qasm2
from qiskit.converters import circuit_to_dag, dag_to_circuit
from qiskit.transpiler.basepasses import TransformationPass
from qiskit.transpiler.preset_passmanagers.builtin_plugins import OptimizationPassManager
from qiskit.transpiler.preset_passmanagers.plugin import PassManagerStagePlugin

from . import CompileError
from . import compile as compile_qirc

GATES = {
    "id", "x", "y", "z", "h", "s", "sdg", "t", "tdg", "sx", "sxdg", "rx", "ry", "rz", "p",
    "swap", "cx", "cy", "cz", "ch", "crx", "cry", "crz", "cp", "cswap",
}
KEPT = {"measure", "reset", "barrier"}


def gate_set(basis):
    if not basis:
        return None
    usable = sorted(GATES & set(basis))
    return ",".join(usable) if usable else None


def flatten(circuit):
    flat = QuantumCircuit(circuit.num_qubits, circuit.num_clbits)
    flat.compose(circuit, qubits=range(circuit.num_qubits), clbits=range(circuit.num_clbits), inplace=True)
    return flat


def translatable(circuit):
    for instruction in circuit.data:
        operation = instruction.operation
        if getattr(operation, "blocks", None) or getattr(operation, "condition", None) is not None:
            return False
    return True


def score(circuit):
    return sum(len(instruction.qubits) > 1 for instruction in circuit.data), circuit.size()


def optimize(circuit, basis=None, level=3):
    if not translatable(circuit):
        return circuit
    flat = flatten(circuit)
    options = {"opt": level}
    if level >= 3:
        options.update(resynth=4, cost="cx")
    gates = gate_set(basis)
    if gates:
        options["gates"] = gates
    try:
        text = compile_qirc(qasm2.dumps(flat), emit="qasm2", name="circuit.qasm", **options)
    except (CompileError, qasm2.QASM2ExportError):
        return circuit
    result = qasm2.loads(text, custom_instructions=qasm2.LEGACY_CUSTOM_INSTRUCTIONS)
    names = {instruction.operation.name for instruction in result.data}
    if basis and not names <= set(basis) | KEPT:
        return circuit
    if result.num_qubits > circuit.num_qubits or score(result) >= score(circuit):
        return circuit
    rebuilt = circuit.copy_empty_like()
    rebuilt.compose(result, qubits=range(result.num_qubits), clbits=range(result.num_clbits), inplace=True)
    return rebuilt


class QircOptimization(TransformationPass):
    def __init__(self, basis=None, level=3):
        super().__init__()
        self.basis = basis
        self.level = level

    def run(self, dag):
        circuit = dag_to_circuit(dag)
        optimized = optimize(circuit, self.basis, self.level)
        if optimized is circuit:
            return dag
        return circuit_to_dag(optimized)


class QircPlugin(PassManagerStagePlugin):
    def pass_manager(self, pass_manager_config, optimization_level=None):
        level = 3 if optimization_level is None else optimization_level
        stage = OptimizationPassManager().pass_manager(pass_manager_config, level)
        if level > 0:
            stage.append(QircOptimization(pass_manager_config.basis_gates, level))
        return stage
