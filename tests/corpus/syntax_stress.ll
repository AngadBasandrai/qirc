; ModuleID = 'syntax_stress'
source_filename = "syntax_stress.ll"

%Qubit = type opaque
%Result = type opaque
%"quoted type" = type { i64, [4 x double], <2 x i64>, { i8, i8 } }
%Packed = type <{ i8, i64 }>

@g.str = private unnamed_addr constant [4 x i8] c"ab\0A\00", align 1
@g.int = global i64 42, align 8
@g.arr = internal constant [3 x i64] [i64 1, i64 2, i64 3]
@g.zero = internal constant [8 x i8] zeroinitializer
@g.ptr = internal constant %Qubit* inttoptr (i64 7 to %Qubit*)

declare void @__quantum__qis__h__body(%Qubit*)
declare void @__quantum__qis__rz__body(double, %Qubit*)
declare void @__quantum__qis__mz__body(%Qubit*, %Result*)
declare i1 @__quantum__qis__read_result__body(%Result*)
declare i64 @"quoted fn name"(i64 %0, ...)

define dso_local i64 @stress(i64 %n, double %theta, i1 %flag) local_unnamed_addr #0 {
entry:
  %0 = alloca i64, align 8
  store i64 %n, i64* %0, align 8
  %1 = load i64, i64* %0, align 8
  %2 = add nsw nuw i64 %1, 3
  %3 = sub i64 %2, 1
  %4 = mul i64 %3, 2
  %5 = sdiv i64 %4, 3
  %6 = srem i64 %5, 7
  %7 = and i64 %6, 255
  %8 = or i64 %7, 16
  %9 = xor i64 %8, -1
  %10 = shl i64 %9, 2
  %11 = ashr i64 %10, 1
  %12 = lshr i64 %11, 1
  %13 = icmp sgt i64 %12, 0
  %14 = fadd double %theta, 1.000000e+00
  %15 = fmul double %14, 0x400921FB54442D18
  %16 = fdiv double %15, 2.000000e+00
  %17 = fsub double %16, 5.000000e-01
  %18 = fcmp ogt double %17, 0.000000e+00
  %19 = zext i1 %13 to i64
  %20 = sext i1 %18 to i64
  %21 = trunc i64 %20 to i32
  %22 = sitofp i32 %21 to double
  %23 = fptosi double %22 to i64
  %24 = ptrtoint %Qubit* inttoptr (i64 3 to %Qubit*) to i64
  %25 = inttoptr i64 %24 to %Qubit*
  %26 = bitcast %Qubit* %25 to i8*
  %27 = getelementptr inbounds [4 x i8], [4 x i8]* @g.str, i64 0, i64 0
  %28 = select i1 %flag, i64 %19, i64 %23
  call void @__quantum__qis__rz__body(double %17, %Qubit* %25)
  call void @__quantum__qis__h__body(%Qubit* %25)
  switch i64 %28, label %default [
    i64 0, label %case0
    i64 1, label %case1
  ]

case0:                                            ; preds = %entry
  call void @__quantum__qis__mz__body(%Qubit* %25, %Result* inttoptr (i64 0 to %Result*))
  %29 = call i1 @__quantum__qis__read_result__body(%Result* inttoptr (i64 0 to %Result*))
  br i1 %29, label %merge, label %default

case1:                                            ; preds = %entry
  br label %merge

default:                                          ; preds = %case0, %entry
  br label %merge

merge:                                            ; preds = %default, %case1, %case0
  %30 = phi i64 [ 1, %default ], [ 2, %case1 ], [ 3, %case0 ]
  %31 = call i64 (i64, ...) @"quoted fn name"(i64 %30, i64 9, double 1.000000e+00)
  ret i64 %31
}

define void @noop() {
  ret void
}

define void @unreachable_tail() {
entry:
  unreachable
}

attributes #0 = { noinline nounwind optnone uwtable "frame-pointer"="all" "target-cpu"="x86-64" }

!llvm.module.flags = !{!0, !1}
!llvm.ident = !{!2}

!0 = !{i32 1, !"qir_major_version", i32 1}
!1 = !{i32 7, !"qir_minor_version", i32 0}
!2 = !{!"qirc stress fixture"}
