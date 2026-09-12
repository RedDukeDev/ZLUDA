define amdgpu_kernel void @cvt_rn_satfinite_e4m3x2_f16x2(ptr addrspace(4) byref(i64) %"40", ptr addrspace(4) byref(i64) %"41") #0 {
  %"42" = alloca i64, align 8, addrspace(5)
  %"43" = alloca i64, align 8, addrspace(5)
  %"44" = alloca i32, align 4, addrspace(5)
  %"45" = alloca i16, align 2, addrspace(5)
  br label %1

1:                                                ; preds = %0
  br label %"39"

"39":                                             ; preds = %1
  call void @llvm.amdgcn.s.dcache.inv()
  %2 = load i64, ptr addrspace(4) %"40", align 8
  store i64 %2, ptr addrspace(5) %"42", align 8
  %3 = load i64, ptr addrspace(4) %"41", align 8
  store i64 %3, ptr addrspace(5) %"43", align 8
  %4 = load i64, ptr addrspace(5) %"42", align 8
  %"54" = inttoptr i64 %4 to ptr
  %5 = load i32, ptr %"54", align 4
  store i32 %5, ptr addrspace(5) %"44", align 4
  %6 = load i32, ptr addrspace(5) %"44", align 4
  %"56" = bitcast i32 %6 to <2 x half>
  %7 = bitcast <2 x half> %"56" to i32
  %8 = and i32 %7, 2147450879
  %9 = add i32 %8, 67109888
  %10 = and i32 %9, -2147450880
  %11 = bitcast i32 %8 to <2 x half>
  %12 = fmul <2 x half> %11, splat (half 0xH1C00)
  %13 = bitcast <2 x half> %12 to i32
  %14 = lshr i32 %13, 7
  %15 = and i32 %14, 65537
  %16 = add i32 %13, 4128831
  %17 = add i32 %16, %15
  %18 = fadd <2 x half> %11, splat (half 0xH4000)
  %19 = fsub <2 x half> %18, splat (half 0xH4000)
  %20 = fmul <2 x half> %19, splat (half 0xH6000)
  %21 = extractelement <2 x half> %20, i32 0
  %22 = extractelement <2 x half> %20, i32 1
  %23 = fptoui half %21 to i32
  %24 = fptoui half %22 to i32
  %25 = and i32 %8, 65535
  %26 = lshr i32 %8, 16
  %27 = icmp ult i32 %25, 9216
  %28 = and i32 %17, 65535
  %29 = lshr i32 %28, 7
  %30 = select i1 %27, i32 %23, i32 %29
  %31 = icmp ult i32 %26, 9216
  %32 = lshr i32 %17, 23
  %33 = select i1 %31, i32 %24, i32 %32
  %34 = icmp ugt i32 %30, 126
  %35 = select i1 %34, i32 126, i32 %30
  %36 = icmp ugt i32 %33, 126
  %37 = select i1 %36, i32 126, i32 %33
  %38 = and i32 %10, 32768
  %39 = icmp ne i32 %38, 0
  %40 = select i1 %39, i32 127, i32 %35
  %41 = and i32 %10, -2147483648
  %42 = icmp ne i32 %41, 0
  %43 = select i1 %42, i32 127, i32 %37
  %44 = lshr i32 %7, 8
  %45 = and i32 %44, 128
  %46 = lshr i32 %7, 24
  %47 = and i32 %46, 128
  %48 = or i32 %40, %45
  %49 = or i32 %43, %47
  %50 = shl i32 %49, 8
  %51 = or i32 %48, %50
  %"55" = trunc i32 %51 to i16
  store i16 %"55", ptr addrspace(5) %"45", align 2
  %52 = load i64, ptr addrspace(5) %"43", align 8
  %53 = load i16, ptr addrspace(5) %"45", align 2
  %"57" = inttoptr i64 %52 to ptr
  store i16 %53, ptr %"57", align 2
  ret void
}

; Function Attrs: nocallback nofree nosync nounwind willreturn
declare void @llvm.amdgcn.s.dcache.inv() #1

attributes #0 = { "amdgpu-ieee"="false" "amdgpu-unsafe-fp-atomics"="true" "denormal-fp-math"="ieee" "denormal-fp-math-f32"="preserve-sign" "no-trapping-math"="true" "target-features"="+wavefrontsize32,-wavefrontsize64,+cumode,+precise-memory" "uniform-work-group-size"="true" }
attributes #1 = { nocallback nofree nosync nounwind willreturn }
