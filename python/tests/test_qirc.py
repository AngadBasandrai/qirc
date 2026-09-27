from pathlib import Path

import pytest

import qirc

ROOT = Path(__file__).resolve().parents[2]
CORPUS = ROOT / "tests" / "corpus"
BELL = (CORPUS / "base_profile_bell.ll").read_text()
TELEPORT = (CORPUS / "adaptive_teleport.ll").read_text()


def test_compile_targets():
    qasm = qirc.compile(BELL, emit="qasm3", opt=3, gates="rz-sx-cx")
    assert qasm.startswith("OPENQASM 3.0;")
    assert "h q" not in qasm
    assert "cx q[0], q[1];" in qasm


def test_run_counts():
    counts = qirc.run(BELL, shots=400, seed=7)
    assert set(counts) == {"00", "11"}
    assert sum(counts.values()) == 400
    assert counts == qirc.run(BELL, shots=400, seed=7)


def test_diff():
    assert qirc.diff(TELEPORT, opt=3, gates="rz-sx-cx", resynth=4) == "equivalent"
    changed = BELL.replace("__quantum__qis__h__body", "__quantum__qis__x__body")
    assert qirc.diff(BELL, changed) == "different"


def test_relabel():
    qft = (CORPUS / "pyqir_simple.ll").read_text()
    plain = qirc.compile(qft, emit="qasm3", opt=2)
    moved = qirc.compile(qft, emit="qasm3", opt=2, relabel=True)
    assert "swap" in plain and "swap" not in moved
    assert qirc.diff(qft, opt=2, relabel=True) == "equivalent"


def test_cost():
    report = qirc.cost(TELEPORT, opt=2, gates="rz-sx-cx")
    assert len(report["paths"]) == 4
    assert report["worst"]["cx"] == 2
    assert not report["truncated"]


def test_calibration():
    device = str(ROOT / "examples" / "line5.cal")
    report = qirc.cost((CORPUS / "qsharp_ising.ll").read_text(), opt=2, gates="rz-sx-cx", calibration=device)
    assert 0.5 < report["worst"]["success"] < 1
    assert qirc.cost(BELL)["worst"]["success"] is None


def test_errors():
    with pytest.raises(qirc.CompileError, match="QIR0100"):
        qirc.compile("define garbage {")
    with pytest.raises(ValueError, match="unknown"):
        qirc.compile(BELL, gates="nonsense")
    with pytest.raises(TypeError, match="colour"):
        qirc.compile(BELL, colour="red")
