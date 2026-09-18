; ModuleID = 'ghz'
source_filename = "ghz"

%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  call void @__quantum__qis__h__body(%Qubit* null)
  br label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %next = add i64 %i, 1
  %control = inttoptr i64 %i to %Qubit*
  %target = inttoptr i64 %next to %Qubit*
  call void @__quantum__qis__cx__body(%Qubit* %control, %Qubit* %target)
  %more = icmp slt i64 %next, 21
  br i1 %more, label %loop, label %done

done:
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 21 to %Qubit*), %Result* inttoptr (i64 1 to %Result*))
  ret void
}

declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__cx__body(%Qubit*, %Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result* writeonly)

attributes #0 = { "entry_point" "qir_profiles"="base_profile" "required_num_qubits"="22" "required_num_results"="2" }
