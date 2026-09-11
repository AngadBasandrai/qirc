; ModuleID = 'DynamicAlloc'
source_filename = "DynamicAlloc"
target datalayout = "e-m:w-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128"
target triple = "x86_64-pc-windows-msvc"

%Qubit = type opaque
%Array = type opaque
%Result = type opaque
%String = type opaque
%Tuple = type opaque

@PauliZ = internal constant i2 1
@0 = internal constant [6 x i8] c"Hello\00"

define internal void @Program__Rotate__body(%Qubit* %q, double %theta) {
entry:
  %half = fmul double %theta, 5.000000e-01
  call void @__quantum__qis__rz__body(double %half, %Qubit* %q)
  ret void
}

define { i64, double }* @Program__Main__body() #0 {
entry:
  %q = call %Qubit* @__quantum__rt__qubit_allocate()
  %reg = call %Array* @__quantum__rt__qubit_allocate_array(i64 4)
  %0 = call i8* @__quantum__rt__array_get_element_ptr_1d(%Array* %reg, i64 0)
  %1 = bitcast i8* %0 to %Qubit**
  %q0 = load %Qubit*, %Qubit** %1, align 8
  call void @__quantum__qis__h__body(%Qubit* %q0)
  call void @Program__Rotate__body(%Qubit* %q, double 1.500000e+00)
  br label %header

header:                                           ; preds = %body, %entry
  %i = phi i64 [ 0, %entry ], [ %next, %body ]
  %cmp = icmp slt i64 %i, 4
  br i1 %cmp, label %body, label %exit

body:                                             ; preds = %header
  %2 = call i8* @__quantum__rt__array_get_element_ptr_1d(%Array* %reg, i64 %i)
  %3 = bitcast i8* %2 to %Qubit**
  %qi = load %Qubit*, %Qubit** %3, align 8
  call void @__quantum__qis__cnot__body(%Qubit* %q, %Qubit* %qi)
  %next = add nuw nsw i64 %i, 1
  br label %header

exit:                                             ; preds = %header
  %r = call %Result* @__quantum__qis__m__body(%Qubit* %q)
  %one = call %Result* @__quantum__rt__result_get_one()
  %eq = call i1 @__quantum__rt__result_equal(%Result* %r, %Result* %one)
  %sel = select i1 %eq, i64 1, i64 0
  %str = call %String* @__quantum__rt__string_create(i8* getelementptr inbounds ([6 x i8], [6 x i8]* @0, i32 0, i32 0))
  call void @__quantum__rt__message(%String* %str)
  call void @__quantum__rt__string_update_reference_count(%String* %str, i32 -1)
  call void @__quantum__rt__qubit_release(%Qubit* %q)
  call void @__quantum__rt__qubit_release_array(%Array* %reg)
  %tuple = call %Tuple* @__quantum__rt__tuple_create(i64 16)
  %4 = bitcast %Tuple* %tuple to { i64, double }*
  %5 = getelementptr inbounds { i64, double }, { i64, double }* %4, i32 0, i32 0
  store i64 %sel, i64* %5, align 8
  %6 = getelementptr inbounds { i64, double }, { i64, double }* %4, i32 0, i32 1
  store double 2.500000e-01, double* %6, align 8
  ret { i64, double }* %4
}

declare %Qubit* @__quantum__rt__qubit_allocate()
declare %Array* @__quantum__rt__qubit_allocate_array(i64)
declare void @__quantum__rt__qubit_release(%Qubit*)
declare void @__quantum__rt__qubit_release_array(%Array*)
declare i8* @__quantum__rt__array_get_element_ptr_1d(%Array*, i64)
declare %Tuple* @__quantum__rt__tuple_create(i64)
declare %String* @__quantum__rt__string_create(i8*)
declare void @__quantum__rt__string_update_reference_count(%String*, i32)
declare void @__quantum__rt__message(%String*)
declare %Result* @__quantum__rt__result_get_one()
declare i1 @__quantum__rt__result_equal(%Result*, %Result*)
declare %Result* @__quantum__qis__m__body(%Qubit*)
declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__rz__body(double, %Qubit*)
declare void @__quantum__qis__cnot__body(%Qubit*, %Qubit*)

attributes #0 = { "entry_point" "qir_profiles"="unrestricted" }

!llvm.module.flags = !{!0, !1}

!0 = !{i32 1, !"qir_major_version", i32 1}
!1 = !{i32 7, !"qir_minor_version", i32 0}
