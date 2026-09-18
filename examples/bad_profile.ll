; ModuleID = 'bad_profile'
source_filename = "bad_profile"

%Qubit = type opaque
%Result = type opaque

define void @main() #0 {
entry:
  call void @__quantum__qis__h__body(%Qubit* null)
  call void @__quantum__qis__mz__body(%Qubit* null, %Result* null)
  %bit = call i1 @__quantum__qis__read_result__body(%Result* null)
  br i1 %bit, label %flip, label %done

flip:
  call void @__quantum__qis__x__body(%Qubit* inttoptr (i64 1 to %Qubit*))
  br label %done

done:
  ret void
}

declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__x__body(%Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result* writeonly)
declare i1 @__quantum__qis__read_result__body(%Result*)

attributes #0 = { "entry_point" "qir_profiles"="base_profile" "required_num_qubits"="2" "required_num_results"="1" }
