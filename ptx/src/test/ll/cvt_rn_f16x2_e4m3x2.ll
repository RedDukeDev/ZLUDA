define amdgpu_kernel void @cvt_rn_f16x2_e4m3x2(ptr addrspace(4) byref(i64) %"40", ptr addrspace(4) byref(i64) %"41") #0 {
  %"42" = alloca i64, align 8, addrspace(5)
  %"43" = alloca i64, align 8, addrspace(5)
  %"44" = alloca i16, align 2, addrspace(5)
  %"45" = alloca i32, align 4, addrspace(5)
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
  %5 = load i16, ptr %"54", align 2
  store i16 %5, ptr addrspace(5) %"44", align 2
  %6 = load i16, ptr addrspace(5) %"44", align 2
  %7 = zext i16 %6 to i32
  %8 = and i32 %7, 255
  %9 = and i32 %7, 65280
  %10 = shl i32 %9, 8
  %11 = or i32 %8, %10
  %12 = and i32 %11, 8388736
  %13 = shl i32 %12, 8
  %14 = and i32 %11, 8323199
  %15 = shl i32 %14, 7
  %16 = and i32 %11, 8323199
  %17 = add i32 %16, 65537
  %18 = and i32 %17, 8388736
  %19 = lshr i32 %18, 7
  %20 = mul i32 %19, 65535
  %21 = bitcast i32 %15 to <2 x half>
  %22 = fmul <2 x half> %21, splat (half 0xH5C00)
  %23 = bitcast <2 x half> %22 to i32
  %24 = xor i32 %20, -1
  %25 = and i32 %23, %24
  %26 = and i32 %20, 2113961472
  %27 = or i32 %13, %25
  %28 = or i32 %27, %26
  %"55" = bitcast i32 %28 to <2 x half>
  %"50" = bitcast <2 x half> %"55" to i32
  store i32 %"50", ptr addrspace(5) %"45", align 4
  %29 = load i64, ptr addrspace(5) %"43", align 8
  %30 = load i32, ptr addrspace(5) %"45", align 4
  %"57" = inttoptr i64 %29 to ptr
  store i32 %30, ptr %"57", align 4
  ret void
}

; Function Attrs: nocallback nofree nosync nounwind willreturn
declare void @llvm.amdgcn.s.dcache.inv() #1

attributes #0 = { "amdgpu-ieee"="false" "amdgpu-unsafe-fp-atomics"="true" "denormal-fp-math"="ieee" "denormal-fp-math-f32"="preserve-sign" "no-trapping-math"="true" "target-features"="+wavefrontsize32,-wavefrontsize64,+cumode,+precise-memory" "uniform-work-group-size"="true" }
attributes #1 = { nocallback nofree nosync nounwind willreturn }
