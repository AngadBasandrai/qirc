; ModuleID = 'repeat_until_success'
source_filename = "repeat_until_success"

%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  br label %attempt

attempt:
  %tries = phi i64 [ 0, %entry ], [ %next, %attempt ]
  call void @__quantum__qis__reset__body(%Qubit* null)
  call void @__quantum__qis__ry__body(double 5.000000e-01, %Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %hit = call i1 @__quantum__qis__read_result__body(%Result* null)
  %next = add i64 %tries, 1
  %limit = icmp sge i64 %next, 50
  %stop = or i1 %hit, %limit
  br i1 %stop, label %done, label %attempt

done:
  call void @__quantum__qis__x__body(%Qubit* inttoptr (i64 1 to %Qubit*))
  call void @__quantum__qis__mz__body(%Qubit* inttoptr (i64 1 to %Qubit*), %Result* inttoptr (i64 1 to %Result*))
  ret void
}

declare void @__quantum__qis__reset__body(%Qubit*)
declare void @__quantum__qis__ry__body(double, %Qubit*)
declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result* writeonly)
declare i1 @__quantum__qis__read_result__body(%Result*)

attributes #0 = { "entry_point" "qir_profiles"="adaptive_profile" "required_num_qubits"="2" "required_num_results"="2" }
