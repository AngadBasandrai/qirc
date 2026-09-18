; ModuleID = 'GhzLoop'
source_filename = "GhzLoop"

%Qubit = type opaque
%Result = type opaque

@qubits = internal constant [4 x %Qubit*] [%Qubit* inttoptr (i64 0 to %Qubit*), %Qubit* inttoptr (i64 1 to %Qubit*), %Qubit* inttoptr (i64 2 to %Qubit*), %Qubit* inttoptr (i64 3 to %Qubit*)]

define internal void @Entangle(%Qubit* %control, %Qubit* %target) {
entry:
  %same = icmp eq %Qubit* %control, %target
  br i1 %same, label %skip, label %apply

apply:
  call void @__quantum__qis__cx__body(%Qubit* %control, %Qubit* %target)
  br label %skip

skip:
  ret void
}

define void @main() #0 {
entry:
  %head = getelementptr [4 x %Qubit*], [4 x %Qubit*]* @qubits, i64 0, i64 0
  %q0 = load %Qubit*, %Qubit** %head, align 8
  call void @__quantum__qis__h__body(%Qubit* %q0)
  br label %header

header:
  %i = phi i64 [ 0, %entry ], [ %next, %body ]
  %more = icmp slt i64 %i, 3
  br i1 %more, label %body, label %measure

body:
  %ctrl.ptr = getelementptr [4 x %Qubit*], [4 x %Qubit*]* @qubits, i64 0, i64 %i
  %ctrl = load %Qubit*, %Qubit** %ctrl.ptr, align 8
  %next = add nuw nsw i64 %i, 1
  %tgt.ptr = getelementptr [4 x %Qubit*], [4 x %Qubit*]* @qubits, i64 0, i64 %next
  %tgt = load %Qubit*, %Qubit** %tgt.ptr, align 8
  call void @Entangle(%Qubit* %ctrl, %Qubit* %tgt)
  br label %header

measure:
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 0 to %Qubit*), %Result* inttoptr (i64 0 to %Result*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 1 to %Qubit*), %Result* inttoptr (i64 1 to %Result*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 2 to %Qubit*), %Result* inttoptr (i64 2 to %Result*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 3 to %Qubit*), %Result* inttoptr (i64 3 to %Result*))
  ret void
}

declare void @__quantum__qis__h__body(%Qubit*)

declare void @__quantum__qis__cx__body(%Qubit*, %Qubit*)

declare void @__quantum__qis__mz__body(%Qubit*, %Result* writeonly)

attributes #0 = { "entry_point" "output_labeling_schema" "qir_profiles"="base_profile" "required_num_qubits"="4" "required_num_results"="4" }

!llvm.module.flags = !{!0, !1}

!0 = !{i32 1, !"qir_major_version", i32 1}
!1 = !{i32 7, !"qir_minor_version", i32 0}
