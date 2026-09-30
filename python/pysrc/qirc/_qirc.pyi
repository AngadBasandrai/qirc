from typing import Literal, TypedDict

__version__: str

class CompileError(Exception): ...

Emit = Literal[
    "run",
    "ir",
    "qasm",
    "qasm3",
    "qasm2",
    "stim",
    "pulse",
    "openpulse",
    "schedule",
    "ionq",
    "qir",
    "llvm",
    "json",
    "circuit",
    "quantikz",
    "latex",
    "svg",
    "cost",
    "resources",
    "rotations",
    "check",
]
CostModel = Literal["gates", "cx", "ibm"]
Verdict = Literal["equivalent", "different", "inconclusive"]

class Tally(TypedDict):
    t: int
    cx: int
    gates: int
    depth: int
    live: int
    success: float | None

class Path(Tally):
    blocks: list[str]
    again: str | None

class Report(TypedDict):
    paths: list[Path]
    worst: Tally
    truncated: bool

def compile(
    source: str,
    *,
    emit: Emit = "qir",
    name: str = ...,
    opt: int | None = ...,
    gates: str | None = ...,
    exclude: str | None = ...,
    resynth: int | None = ...,
    cost: CostModel | None = ...,
    coupling: str | None = ...,
    relabel: bool = ...,
    calibration: str | None = ...,
    reuse: bool = ...,
    noisy: bool = ...,
    epsilon: float | None = ...,
    budget: float | None = ...,
    dd: bool = ...,
    bind: dict[str, float] | None = ...,
) -> str: ...
def run(
    source: str,
    *,
    shots: int = 1000,
    seed: int | None = None,
    name: str = ...,
    opt: int | None = ...,
    gates: str | None = ...,
    exclude: str | None = ...,
    resynth: int | None = ...,
    cost: CostModel | None = ...,
    coupling: str | None = ...,
    relabel: bool = ...,
    calibration: str | None = ...,
    reuse: bool = ...,
    noisy: bool = ...,
    epsilon: float | None = ...,
    budget: float | None = ...,
    dd: bool = ...,
    bind: dict[str, float] | None = ...,
) -> dict[str, int]: ...
def diff(
    source: str,
    other: str | None = None,
    *,
    name: str = ...,
    opt: int | None = ...,
    gates: str | None = ...,
    exclude: str | None = ...,
    resynth: int | None = ...,
    cost: CostModel | None = ...,
    coupling: str | None = ...,
    relabel: bool = ...,
    calibration: str | None = ...,
    reuse: bool = ...,
    noisy: bool = ...,
    epsilon: float | None = ...,
    budget: float | None = ...,
    dd: bool = ...,
    bind: dict[str, float] | None = ...,
) -> Verdict: ...
def expectation(
    source: str,
    observable: str,
    *,
    zne: bool = False,
    shots: int = 4000,
    seed: int = 1,
    name: str = ...,
    opt: int | None = ...,
    gates: str | None = ...,
    exclude: str | None = ...,
    resynth: int | None = ...,
    cost: CostModel | None = ...,
    coupling: str | None = ...,
    relabel: bool = ...,
    calibration: str | None = ...,
    reuse: bool = ...,
    noisy: bool = ...,
    epsilon: float | None = ...,
    budget: float | None = ...,
    dd: bool = ...,
    bind: dict[str, float] | None = ...,
) -> float: ...
def cost(
    source: str,
    *,
    name: str = ...,
    opt: int | None = ...,
    gates: str | None = ...,
    exclude: str | None = ...,
    resynth: int | None = ...,
    cost: CostModel | None = ...,
    coupling: str | None = ...,
    relabel: bool = ...,
    calibration: str | None = ...,
    reuse: bool = ...,
    noisy: bool = ...,
    epsilon: float | None = ...,
    budget: float | None = ...,
    dd: bool = ...,
    bind: dict[str, float] | None = ...,
) -> Report: ...
