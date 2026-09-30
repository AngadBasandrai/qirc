const EXPLANATIONS: [(&str, &str); 19] = [
    (
        "QIR0001",
        "A character that cannot start any LLVM IR token.

QIR files are textual LLVM IR, so every token is a keyword, a type, a number, a
string, or a name starting with @, % or !. This often means the file is not LLVM
IR at all, for example OpenQASM without its `OPENQASM 3.0;` line, which is how
qirc tells the two apart.",
    ),
    (
        "QIR0002",
        "A sigil with no name after it.

Global names start with @, local values with % and metadata with !, and each
needs a name or a number right after it, as in `@main`, `%0` or `!llvm.module.flags`.
Quoted names such as `@\"my function\"` are also accepted.",
    ),
    (
        "QIR0003",
        "A string literal that never closes.

Strings such as c\"0_r\\00\" and attribute values run to the next double quote.
Check for a missing closing quote on the line the error points at.",
    ),
    (
        "QIR0100",
        "A syntax error.

In LLVM IR this is a token where the grammar expects something else, such as a
missing comma between call arguments or a missing type. In OpenQASM it also
covers unknown gates, wrong argument counts, indices outside a register and
values that do not fit their declared width. The label under the error names
what was expected.",
    ),
    (
        "QIR0200",
        "An operation qirc cannot lower to quantum IR.

qirc places every qubit at compile time, so a qubit must come from a constant,
an entry point parameter or a value it can compute before running, never from a
measurement. Recursion is expanded when it is a tail call or has a depth known
at compile time. Rewrite the operation so these are fixed, or keep the dynamic
part classical.",
    ),
    (
        "QIR0201",
        "The module has no entry point.

qirc runs the function marked with the `entry_point` attribute, as in
`attributes #0 = { \"entry_point\" }` on `define void @main() #0`. Without the
attribute it falls back to a function named `main`, so add one of the two.",
    ),
    (
        "QIR0202",
        "The program uses more qubits or results than qirc can address.

Qubit and result indices above the limit usually mean an index was computed
wrongly, for example a pointer cast from a large constant.",
    ),
    (
        "QIR0300",
        "The Base Profile forbids branching.

A program that declares `qir_profiles`=\"base_profile\" must be one straight
block of gates and measurements. If it needs to branch on a measurement, as
teleportation does, declare `adaptive_profile` instead.",
    ),
    (
        "QIR0301",
        "The Base Profile forbids reading a measurement result.

Reading a result back into the program with `__quantum__rt__read_result` is
what makes feedback possible, and only the Adaptive Profile allows it. Declare
`adaptive_profile`, or record the result with `__quantum__rt__result_record_output`
instead of reading it.",
    ),
    (
        "QIR0302",
        "A qubit outside the declared register.

The entry point's `required_num_qubits` attribute says how many qubits the
program uses, and every qubit index must be below it. Raise the attribute or
fix the index.",
    ),
    (
        "QIR0303",
        "A gate uses the same qubit twice.

A two qubit gate such as a CNOT needs two different qubits, and a controlled
gate cannot use its target as a control.",
    ),
    (
        "QIR0304",
        "A gate has the wrong number of parameters.

Rotations such as `rz`, `rx`, `ry` and `r1` take one angle and fixed gates such
as `h` take none. Check the call against the intrinsic's declaration.",
    ),
    (
        "QIR0305",
        "A gate has the wrong number of targets.

Single qubit gates act on one qubit, CNOT and CZ on a control and a target, and
Toffoli on two controls and a target.",
    ),
    (
        "QIR0306",
        "A result outside the declared results.

The entry point's `required_num_results` attribute says how many results the
program writes, and every result index must be below it.",
    ),
    (
        "QIR0307",
        "A branch to a block that does not exist.

Every `br` and `switch` target must be a label defined in the same function.",
    ),
    (
        "QIR0308",
        "A result read before it is measured.

On some path through the program the result is read or recorded before any
measurement writes it. Measure into the result first, on every path that
reaches the read.",
    ),
    (
        "QIR0400",
        "The program cannot be routed onto the coupling map.

The map must be connected and have at least as many qubits as the program.
Use a larger map, such as `--coupling line:8`, or check the edges in the
calibration's `cx` lines.",
    ),
    (
        "QIR0401",
        "The gate set cannot express some gates exactly.

qirc only rewrites a gate when the result is exact, so a rotation by an
arbitrary angle cannot be built from a fixed set such as `h,s,t,cx`, and it is
left as written. Add a rotation to the set, such as `rz`, to lower it.",
    ),
    (
        "QIR0402",
        "A rotation cannot be approximated in the gate set.

`--epsilon` rebuilds each rotation the gate set cannot express exactly as a
sequence of H, S and T gates within the given precision, so the set must
contain h and t. qirc reaches precisions down to about 1e-10; ask for a larger
epsilon, or add a rotation such as `rz` to the gate set.",
    ),
];

pub fn explain(code: &str) -> Option<&'static str> {
    let code = code.trim().to_ascii_uppercase();
    EXPLANATIONS
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, text)| *text)
}

pub fn codes() -> impl Iterator<Item = &'static str> {
    EXPLANATIONS.iter().map(|(code, _)| *code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_codes() {
        for source in [
            include_str!("lex.rs"),
            include_str!("parse.rs"),
            include_str!("lower.rs"),
            include_str!("sema.rs"),
            include_str!("qasm.rs"),
            include_str!("driver.rs"),
        ] {
            for (at, _) in source.match_indices("with_code(\"") {
                let code = &source[at + 11..at + 18];
                assert!(explain(code).is_some(), "{code} has no explanation");
            }
        }
        assert!(explain("qir0300").is_some());
        assert!(explain("QIR9999").is_none());
    }
}
