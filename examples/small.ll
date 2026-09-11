; small.ll

%Qubit = type opaque
%Result = type opaque

declare void @__quantum__qis__h(%Qubit*)
declare void @__quantum__qis__x(%Qubit*)
declare void @__quantum__qis__cnot(%Qubit*, %Qubit*)
declare void @__quantum__qis__mz(%Qubit*, %Result*)

define void @main(%Qubit* %q0, %Qubit* %q1, %Result* %r0) {
entry:
  call void @__quantum__qis__h(%Qubit* %q0)
  call void @__quantum__qis__x(%Qubit* %q1)
  call void @__quantum__qis__cnot(%Qubit* %q0, %Qubit* %q1)
  ret void
}
