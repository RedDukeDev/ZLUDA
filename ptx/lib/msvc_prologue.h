// What HIP's device headers would give us, on a toolchain whose standard
// library is Microsoft's.
//
// Three of HIP's own headers are switched off when this one is force-included
// (see build_bc_msvc.sh): __clang_hip_cmath.h, because its device overloads of
// isless/isgreater/... collide with Microsoft's constexpr ones, which clang
// treats as host+device in HIP mode; __clang_cuda_math_forward_declares.h,
// which only forward-declares those same names; and
// __clang_cuda_complex_builtins.h, which is reached from the HIP runtime
// wrapper and needs the names the first header would have provided.
//
// zluda_ptx_impl.cpp turns out to need exactly three things out of all that,
// and each is reproduced below as HIP itself defines it -- not approximated.

#pragma once

#include <cmath>

// std::fabs and std::fma for float, verbatim from __clang_hip_cmath.h: both
// forward to the ::fabsf and ::fmaf of __clang_hip_math.h, which is still in
// play and which maps them to __builtin_fabsf and __builtin_fmaf. Microsoft's
// host versions of the same signatures stay where they are; a __device__
// function and a __host__ one may overload.
namespace std {
__device__ inline float fabs(float __x) { return ::fabsf(__x); }
__device__ inline float fma(float __x, float __y, float __z) { return ::fmaf(__x, __y, __z); }
} // namespace std

// The handler behind PTX's assertfail. HIP declares __assert_fail on device
// only when it is not building for Windows -- there it offers _wassert
// instead, whose body is a bare trap, because the message would arrive as
// wchar_t. So this is the Windows shape of the same thing, under the name the
// caller uses. The one behavioural difference from a Linux-built bitcode: a
// failing device assertion stops the wave without printing which assertion it
// was.
extern "C" __device__ __attribute__((noinline)) __attribute__((weak)) void
__assert_fail(const char *assertion, const char *file, unsigned int line,
              const char *function) {
    (void)assertion;
    (void)file;
    (void)line;
    (void)function;
    __builtin_trap();
}
