#!/usr/bin/env bash
# Regenerates zluda_ptx_impl.bc and zluda_ptx_impl_constrained.bc.
#
# Same as the recipe in the header of zluda_ptx_impl.cpp, with one difference:
# ocml.bc comes from the fork's own device-libs rather than /opt/rocm. The ROCm
# installed here (7.15) ships an ocml.bc in LLVM 23 bitcode, which the fork's
# clang (LLVM 22) cannot read; dropping the link entirely would leave
# __ocml_tanh_f16 and __ocml_tanh_f32 unresolved, so rebuild it from the fork:
#
#   cd ext/llvm-project/amd/device-libs && mkdir -p build && cd build && \
#   cmake -GNinja -DCMAKE_BUILD_TYPE=Release \
#     -DCMAKE_C_COMPILER=../../../build/bin/clang \
#     -DCMAKE_CXX_COMPILER=../../../build/bin/clang++ \
#     -DLLVM_DIR=../../../build/lib/cmake/llvm .. && ninja
#
# (the LLVM build also needs `ninja llvm-link opt`.)
set -euo pipefail
cd "$(dirname "$0")"

LLVM_BIN=${LLVM_BIN:-../../ext/llvm-project/build/bin}
CLANG=$LLVM_BIN/clang
LLVM_AS=$LLVM_BIN/llvm-as
LLVM_DIS=$LLVM_BIN/llvm-dis
OCML=${OCML:-../../ext/llvm-project/amd/device-libs/build/amdgcn/bitcode/ocml.bc}

[ -f "$OCML" ] || { echo "missing $OCML - see the instructions at the top of this file" >&2; exit 1; }
for t in "$CLANG" "$LLVM_AS" "$LLVM_DIS"; do
  [ -x "$t" ] || { echo "missing $t - build it first: cd ext/llvm-project && mkdir -p build && cd build && cmake -DCMAKE_BUILD_TYPE=Release -DLLVM_ENABLE_PROJECTS=clang -DLLVM_TARGETS_TO_BUILD='AMDGPU;X86' -GNinja ../llvm && ninja clang llvm-dis llvm-as" >&2; exit 1; }
done

common=(
  -DHIP_ENABLE_WARP_SYNC_BUILTINS
  -std=c++20
  -Xclang -fdenormal-fp-math=dynamic
  -Wall -Wextra -Wsign-compare -Wconversion
  -x hip zluda_ptx_impl.cpp
  -nogpulib -O3 -mno-wavefrontsize64
  -emit-llvm -c
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
