define amdgpu_kernel void @cvt_rn_satfinite_e4m3x2_f32(ptr addrspace(4) byref(i64) %"43", ptr addrspace(4) byref(i64) %"44") #0 {
  %"45" = alloca i64, align 8, addrspace(5)
  %"46" = alloca i64, align 8, addrspace(5)
  %"47" = alloca float, align 4, addrspace(5)
  %"48" = alloca float, align 4, addrspace(5)
  %"49" = alloca i16, align 2, addrspace(5)
  br label %1

1:                                                ; preds = %0
  br label %"42"

"42":                                             ; preds = %1
  call void @llvm.amdgcn.s.dcache.inv()
  %2 = load i64, ptr addrspace(4) %"43", align 8
  store i64 %2, ptr addrspace(5) %"45", align 8
  %3 = load i64, ptr addrspace(4) %"44", align 8
  store i64 %3, ptr addrspace(5) %"46", align 8
  %4 = load i64, ptr addrspace(5) %"45", align 8
  %"61" = inttoptr i64 %4 to ptr
  %5 = load float, ptr %"61", align 4
  store float %5, ptr addrspace(5) %"47", align 4
  %6 = load i64, ptr addrspace(5) %"45", align 8
  %"62" = inttoptr i64 %6 to ptr
  %"41" = getelementptr inbounds i8, ptr %"62", i64 4
  %7 = load float, ptr %"41", align 4
  store float %7, ptr addrspace(5) %"48", align 4
  %8 = load float, ptr addrspace(5) %"47", align 4
  %9 = load float, ptr addrspace(5) %"48", align 4
  %10 = bitcast float %9 to i32
  %11 = and i32 %10, -2147483648
  %12 = lshr i32 %11, 24
  %13 = and i32 %12, 128
  %14 = and i32 %10, 2147483647
  %15 = lshr i32 %14, 23
  %16 = icmp eq i32 %15, 255
  %17 = icmp eq i32 %15, 0
  %18 = and i32 %14, 8388607
  %19 = select i1 %17, i32 0, i32 8388608
  %20 = or i32 %18, %19
  %21 = sub i32 %15, 127
  %22 = select i1 %17, i32 -126, i32 %21
  %23 = and i32 %20, 1048575
  %24 = lshr i32 %20, 20
  %25 = icmp ugt i32 %23, 524288
  %26 = icmp eq i32 %23, 524288
  %27 = and i32 %24, 1
  %28 = icmp ne i32 %27, 0
  %29 = and i1 %26, %28
  %30 = or i1 %25, %29
  %31 = zext i1 %30 to i32
  %32 = add i32 %24, %31
  %33 = icmp eq i32 %32, 16
  %34 = select i1 %33, i32 8, i32 %32
  %35 = zext i1 %33 to i32
  %36 = add i32 %22, %35
  %37 = add i32 %36, 7
  %38 = icmp uge i32 %37, 16
  %39 = sub i32 %34, 8
  %40 = shl i32 %37, 3
  %41 = or i32 %40, %39
  %42 = select i1 %38, i32 126, i32 %41
  %43 = sub i32 14, %22
  %44 = icmp sgt i32 %43, 30
  %45 = select i1 %44, i32 30, i32 %43
  %46 = shl i32 1, %45
  %47 = sub i32 %46, 1
  %48 = and i32 %20, %47
  %49 = lshr i32 %47, 1
  %50 = lshr i32 %20, %45
  %51 = icmp ugt i32 %48, %49
  %52 = icmp eq i32 %48, %49
  %53 = and i32 %50, 1
  %54 = icmp ne i32 %53, 0
  %55 = and i1 %52, %54
  %56 = xor i1 %44, true
  %57 = and i1 %55, %56
  %58 = or i1 %51, %57
  %59 = zext i1 %58 to i32
  %60 = add i32 %50, %59
  %61 = icmp ugt i32 %60, 7
  %62 = select i1 %61, i32 7, i32 %60
  %63 = icmp sge i32 %22, -6
  %64 = select i1 %63, i32 %42, i32 %62
  %65 = or i32 %64, %13
  %66 = or i32 %13, 127
  %67 = select i1 %16, i32 %66, i32 %65
  %68 = bitcast float %8 to i32
  %69 = and i32 %68, -2147483648
  %70 = lshr i32 %69, 24
  %71 = and i32 %70, 128
  %72 = and i32 %68, 2147483647
  %73 = lshr i32 %72, 23
  %74 = icmp eq i32 %73, 255
  %75 = icmp eq i32 %73, 0
  %76 = and i32 %72, 8388607
  %77 = select i1 %75, i32 0, i32 8388608
  %78 = or i32 %76, %77
  %79 = sub i32 %73, 127
  %80 = select i1 %75, i32 -126, i32 %79
  %81 = and i32 %78, 1048575
  %82 = lshr i32 %78, 20
  %83 = icmp ugt i32 %81, 524288
  %84 = icmp eq i32 %81, 524288
  %85 = and i32 %82, 1
  %86 = icmp ne i32 %85, 0
  %87 = and i1 %84, %86
  %88 = or i1 %83, %87
  %89 = zext i1 %88 to i32
  %90 = add i32 %82, %89
  %91 = icmp eq i32 %90, 16
  %92 = select i1 %91, i32 8, i32 %90
  %93 = zext i1 %91 to i32
  %94 = add i32 %80, %93
  %95 = add i32 %94, 7
  %96 = icmp uge i32 %95, 16
  %97 = sub i32 %92, 8
  %98 = shl i32 %95, 3
  %99 = or i32 %98, %97
  %100 = select i1 %96, i32 126, i32 %99
  %101 = sub i32 14, %80
  %102 = icmp sgt i32 %101, 30
  %103 = select i1 %102, i32 30, i32 %101
  %104 = shl i32 1, %103
  %105 = sub i32 %104, 1
  %106 = and i32 %78, %105
  %107 = lshr i32 %105, 1
  %108 = lshr i32 %78, %103
  %109 = icmp ugt i32 %106, %107
  %110 = icmp eq i32 %106, %107
  %111 = and i32 %108, 1
  %112 = icmp ne i32 %111, 0
  %113 = and i1 %110, %112
  %114 = xor i1 %102, true
  %115 = and i1 %113, %114
  %116 = or i1 %109, %115
  %117 = zext i1 %116 to i32
  %118 = add i32 %108, %117
  %119 = icmp ugt i32 %118, 7
  %120 = select i1 %119, i32 7, i32 %118
  %121 = icmp sge i32 %80, -6
  %122 = select i1 %121, i32 %100, i32 %120
  %123 = or i32 %122, %71
  %124 = or i32 %71, 127
  %125 = select i1 %74, i32 %124, i32 %123
  %126 = shl i32 %125, 8
  %127 = or i32 %67, %126
  %"63" = trunc i32 %127 to i16
  store i16 %"63", ptr addrspace(5) %"49", align 2
  %128 = load i64, ptr addrspace(5) %"46", align 8
  %129 = load i16, ptr addrspace(5) %"49", align 2
  %"64" = inttoptr i64 %128 to ptr
  store i16 %129, ptr %"64", align 2
  ret void
}

; Function Attrs: nocallback nofree nosync nounwind willreturn
declare void @llvm.amdgcn.s.dcache.inv() #1

attributes #0 = { "amdgpu-ieee"="false" "amdgpu-unsafe-fp-atomics"="true" "denormal-fp-math"="ieee" "denormal-fp-math-f32"="ieee" "no-trapping-math"="true" "target-features"="+wavefrontsize32,-wavefrontsize64,+cumode,+precise-memory" "uniform-work-group-size"="true" }
attributes #1 = { nocallback nofree nosync nounwind willreturn }
