#!/usr/bin/env bash
# Regenerates the bitcode on Windows, with ROCm's own clang.
#
# build_bc.sh is the reference recipe and stays that. This is the fallback for
# a machine where the fork's clang is not available as a Windows binary: it
# uses the clang shipped with ROCm (currently LLVM 21) and ROCm's own ocml.bc,
# which are consistent with each other. The fork's LLVM is 22, and an LLVM 22
# reader accepts LLVM 21 bitcode, so the result loads.
#
# Two things differ from the reference and both are deliberate:
#
#  - Three of HIP's device headers are switched off and msvc_prologue.h
#    supplies what this file actually needs out of them; the header explains
#    why, and the only behavioural difference is that a failing device
#    assertion traps without printing its message.
#
#  - The compiler is not the fork's, so code generation differs everywhere,
#    not just where the source changed. Nothing here proves the result
#    equivalent. Run the numerical checks in tools/ before trusting it, and
#    empty ZLUDA's cache first (%LOCALAPPDATA%\zluda\ComputeCache) or the old
#    translations answer instead.
set -euo pipefail
cd "$(dirname "$0")"

ROCM=${ROCM:-/c/Program Files/AMD/ROCm/7.2}
CLANG="$ROCM/bin/clang++.exe"
LLVM_AS="$ROCM/bin/llvm-as.exe"
LLVM_DIS="$ROCM/bin/llvm-dis.exe"
OCML=${OCML:-$ROCM/amdgcn/bitcode/ocml.bc}

[ -f "$OCML" ] || { echo "missing $OCML" >&2; exit 1; }
for t in "$CLANG" "$LLVM_AS" "$LLVM_DIS"; do
  [ -x "$t" ] || { echo "missing $t - is ROCm installed at $ROCM?" >&2; exit 1; }
done

# A failed run used to leave no bitcode at all, having already replaced it.
for f in zluda_ptx_impl.bc zluda_ptx_impl_constrained.bc; do
  [ -f "$f" ] && cp -f "$f" "$f.backup"
done

common=(
  -DHIP_ENABLE_WARP_SYNC_BUILTINS
  # The headers msvc_prologue.h stands in for, marked as already seen.
  -D__CLANG_HIP_CMATH_H__
  -D__CLANG__CUDA_MATH_FORWARD_DECLARES_H__
  -D__CLANG_CUDA_COMPLEX_BUILTINS
  -include msvc_prologue.h
  -std=c++20
  -Xclang -fdenormal-fp-math=dynamic
  -Wall -Wextra -Wsign-compare -Wconversion
  -x hip zluda_ptx_impl.cpp
  -nogpulib -O3 -mno-wavefrontsize64
  -emit-llvm -c
  # gfx1030 only admits the builtins; the target is stripped again below, so
  # the bitcode stays generic and ZLUDA specialises it per device.
  --offload-device-only --offload-arch=gfx1030
  -Xclang -mlink-bitcode-file -Xclang "$OCML"
)

build_one() {
  local out=$1; shift
  echo ">>> $out"
  "$CLANG" "${common[@]}" "$@" -o "$out"
  "$LLVM_DIS" "$out" -o - \
    | sed '/@llvm.used/d' \
    | sed '/wchar_size/d' \
    | sed '/llvm.module.flags/d' \
    | sed '/__hip_cuid/d' \
    | sed 's/optnone//g' \
    | sed 's/define hidden/define linkonce_odr/g' \
    | sed 's/"target-cpu"="gfx1030"//g' \
    | sed -E 's/"target-features"="[^"]+"//g' \
    | "$LLVM_AS" - -o "$out"
  "$LLVM_DIS" "$out" -o /dev/null
  echo "    ok: $(stat -c%s "$out") bytes"
}

build_one zluda_ptx_impl.bc
build_one zluda_ptx_impl_constrained.bc -ffp-model=strict -ffp-exception-behavior=ignore
