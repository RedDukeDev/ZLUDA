# Regenerates zluda_ptx_impl.bc and zluda_ptx_impl_constrained.bc.
#
# The pipeline at the top of zluda_ptx_impl.cpp is the specification; this script
# is that specification made executable, with the Windows spellings of the paths
# filled in and the post-conditions asserted afterwards instead of hoped for.
#
# Why a script at all: two of the four tools must come from the vendored tree
# (D:\Downloads\ZLUDA\ext\llvm-project) and the other two can come from the HIP
# SDK, and getting that split wrong fails either loudly (bitcode version) or
# silently (intrinsics). See the notes at each step.
#
# Status as of 2026-09-13 -- read before trusting it:
#   * RUN END TO END: both variants regenerated. clang (from the HIP SDK) compiles
#     the source, the HIP SDK's llvm-dis disassembles it, the sed chain runs, and
#     the patched LLVM 22 llvm-as reassembles. Result: noinline 0 module-wide, the
#     mma wrapper gone (it inlined), and the intrinsics now sitting directly in
#     the FUNC(...) helper bodies -- both halves of the fp8 one included.
#   * The constrained variant emits clang warnings (-ffp-model=strict is
#     overridden by -ffp-exception-behavior=ignore, and an unsupported rounding
#     mode). They come from the flags in the header comment, not from here, and
#     the two outputs are still distinct files (69 700 vs 68 988 bytes).
#   * NOT VERIFIED: that the outer helpers then inline into the *kernel* at
#     module-translation time. That is what the s_swappc and v_wmma gates test,
#     and neither fork edit has been compiled.
#
# Usage:
#   pwsh -File rebuild-bitcode.ps1                       # both variants
#   pwsh -File rebuild-bitcode.ps1 -SkipLlmTools         # if llvm-as is already built
#   pwsh -File rebuild-bitcode.ps1 -KeepOnFailure        # leave a failed artifact for inspection

param(
    [string]$ZLUDA = 'D:\Downloads\ZLUDA',
    [string]$Hip   = $env:HIP_PATH,
    [switch]$SkipLlmTools,
    [switch]$KeepOnFailure
)

$ErrorActionPreference = 'Stop'
$lib = Join-Path $ZLUDA 'ptx\lib'
$work = Join-Path $env:TEMP 'zluda-bitcode-rebuild'

function Fail($msg) { Write-Host "FAIL: $msg" -ForegroundColor Red; exit 1 }
function Note($msg) { Write-Host "  $msg" }

# ---------------------------------------------------------------- preconditions
if (-not $Hip) { Fail 'HIP_PATH is not set; pass -Hip <HIP SDK root>' }
if (-not (Test-Path (Join-Path $Hip 'bin\clang.exe')))    { Fail "no clang.exe under $Hip\bin" }
if (-not (Test-Path (Join-Path $Hip 'bin\llvm-dis.exe'))) { Fail "no llvm-dis.exe under $Hip\bin" }

$ocml = Join-Path $Hip 'amdgcn\bitcode\ocml.bc'
if (-not (Test-Path $ocml)) { Fail "no ocml.bc at $ocml (the pipeline links it into the module)" }

New-Item -ItemType Directory -Force -Path $work | Out-Null

# The *writing* tool has to be the vendored LLVM, because the .bc it produces has
# to be a version the fork's own LLVM reads back. Reading is looser: any llvm-dis
# at least as new as the writer can read the file, which is why the compile and
# the disassembly below use the HIP SDK and only the reassembly is patched.
$llvmBuild = Get-ChildItem (Join-Path $ZLUDA 'target\release\build') -Directory -Filter 'llvm_zluda*' -ErrorAction SilentlyContinue |
    Where-Object { Test-Path (Join-Path $_.FullName 'out\build\CMakeCache.txt') } |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
if (-not $llvmBuild) { Fail "no cached LLVM build under $ZLUDA\target\release\build (build ZLUDA once first)" }
$llvmBin = Join-Path $llvmBuild.FullName 'out\build\bin'
$src = (Select-String -Path (Join-Path $llvmBuild.FullName 'out\build\CMakeCache.txt') -Pattern '^LLVM_MAIN_SRC_DIR:' | Select-Object -First 1).Line
Note "vendored LLVM tree : $($llvmBuild.Name)"
Note "$src"

$patchedAs = Join-Path $llvmBin 'llvm-as.exe'
if (-not (Test-Path $patchedAs)) {
    if ($SkipLlmTools) { Fail "llvm-as.exe not built in $llvmBin and -SkipLlmTools was given" }
    Write-Host 'Building llvm-as / llvm-dis in the cached LLVM tree (links against libraries already built)...'
    $vswhere = @(
        'D:\Program Files\Microsoft Visual Studio\18\Community',
        'C:\Program Files\Microsoft Visual Studio\18\Community',
        'C:\Program Files\Microsoft Visual Studio\2022\Community'
    ) | Where-Object { Test-Path (Join-Path $_ 'VC\Auxiliary\Build\vcvars64.bat') } | Select-Object -First 1
    if (-not $vswhere) { Fail 'no MSVC vcvars64.bat found; run this from a Developer prompt instead' }
    # A batch file rather than "cmd /c call ... && ninja ...": nesting that much
    # quoting through PowerShell's native-argument passing does not survive, and
    # fails with a misleading "filename or extension is incorrect". This form is
    # the one that has actually been run.
    $bat = Join-Path $work 'build-llvm-tools.bat'
    $vcvarsBat = Join-Path $vswhere 'VC\Auxiliary\Build\vcvars64.bat'
    $llvmBuildDir = Join-Path $llvmBuild.FullName 'out\build'
    # One element per line, with no commas and no `+` inside the literal: in
    # PowerShell the comma binds tighter than `+`, so an element written as
    # 'call "' + $path + '" ...' inside a comma-separated array is silently
    # split into three elements at the quotes -- which is how the first version
    # of this script produced a batch file whose second line was `call "`.
    # String interpolation with escaped quotes has no such trap.
    $batLines = @(
        '@echo off'
        "call `"$vcvarsBat`" >nul 2>&1"
        'if errorlevel 1 exit /b 1'
        "cd /d `"$llvmBuildDir`""
        'ninja llvm-as llvm-dis'
        'exit /b %ERRORLEVEL%'
    )
    Set-Content -LiteralPath $bat -Value $batLines -Encoding ascii
    # Assert the file that was actually written. The split above failed silently
    # for the caller and produced a barrage of unrelated cmd errors; this turns
    # that into one clear message instead.
    $written = Get-Content -LiteralPath $bat
    $malformed = ($written.Count -ne $batLines.Count) -or `
                 (($written -join "`n") -notmatch 'vcvars64\.bat') -or `
                 (($written -join "`n") -notmatch 'ninja llvm-as llvm-dis')
    if ($malformed) {
        Write-Host '  generated batch file:' -ForegroundColor Yellow
        $written | ForEach-Object { Write-Host "    |$_|" }
        Fail "the generated batch file is malformed; it is at $bat"
    }
    Note "build via          : $bat"
    Write-Host '  (llvm-as/llvm-dis are two small tools, but they still need compiling and linking)'
    cmd /c $bat
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $patchedAs)) { Fail 'ninja llvm-as llvm-dis did not produce the tools' }
}
Note "reassembly tool    : $patchedAs"

# ------------------------------------------------------------------- the passes
# Both variants compile the SAME source file; the constrained one differs only in
# flags and in the name it is written under. (An earlier version of this script
# looked for zluda_ptx_impl_constrained.cpp, which does not exist -- both
# pipelines in the header comment compile zluda_ptx_impl.cpp.)
$variants = @(
    [pscustomobject]@{ Out = 'zluda_ptx_impl';             Extra = @() },
    [pscustomobject]@{ Out = 'zluda_ptx_impl_constrained'; Extra = @('-ffp-model=strict', '-ffp-exception-behavior=ignore') }
)
$cpp = Join-Path $lib 'zluda_ptx_impl.cpp'

foreach ($v in $variants) {
    $name = $v.Out
    Write-Host ''
    Write-Host "=== $name ===" -ForegroundColor Cyan

    $target  = Join-Path $lib "$name.bc"
    $backup  = Join-Path $work "$name.bc.orig"
    $stageBc = Join-Path $work "$name.stage.bc"
    $stageLl = Join-Path $work "$name.stage.ll"
    $outBc   = Join-Path $work "$name.out.bc"

    if (-not (Test-Path $target)) { Fail "$target does not exist (nothing to regenerate)" }
    # Keep the FIRST backup, not the latest. Re-running the script would otherwise
    # overwrite the pristine artifact with its own previous output, leaving git as
    # the only copy of the original -- which is what happened on the first
    # successful run here.
    if (Test-Path $backup) {
        Note "backup             : $backup (kept from the first run)"
    } else {
        Copy-Item -LiteralPath $target -Destination $backup -Force
        Note "backup             : $backup"
    }

    # 1. compile to bitcode. The HIP SDK's clang is used deliberately: it does not
    #    need to know llvm.zluda.mma as an intrinsic, because a function's
    #    intrinsic-ness is decided by whoever *reads* the module, by name lookup,
    #    and the reader here is the fork's patched LLVM. Verified: reassembling
    #    the committed IR with the HIP SDK's llvm-as keeps the names intact.
    #    If clang cannot find its HIP headers, add "--rocm-path=$Hip".
    $clangArgs = @(
        '-DHIP_ENABLE_WARP_SYNC_BUILTINS', '-std=c++20', '-Xclang', '-fdenormal-fp-math=dynamic',
        '-Wall', '-Wextra', '-Wsign-compare', '-Wconversion', '-x', 'hip', $cpp,
        '-nogpulib', '-O3', '-mno-wavefrontsize64', '-o', $stageBc, '-emit-llvm', '-c',
        '--offload-device-only', '--offload-arch=gfx1030',
        '-Xclang', '-mlink-bitcode-file', '-Xclang', $ocml
    ) + $v.Extra
    & (Join-Path $Hip 'bin\clang.exe') @clangArgs
    if ($LASTEXITCODE -ne 0) { Fail "clang failed for $name" }

    # 2. bitcode -> text
    $ir = & (Join-Path $Hip 'bin\llvm-dis.exe') $stageBc -o -
    if ($LASTEXITCODE -ne 0) { Fail "llvm-dis failed for $name" }
    Note "IR lines           : $($ir.Count)"

    # 3. the sed chain, replicated. '/'+pattern+'/d' drops lines; the rest are
    #    global substitutions. optnone is the only attribute struck out: the
    #    wrappers no longer carry it in the source, so there is no leftover
    #    noinline to remove, and removing it would undo the noinline the mma
    #    helpers rely on to stay calls. See the header comment.
    $out = $ir | Where-Object { $_ -notmatch '@llvm\.used|wchar_size|llvm\.module\.flags|__hip_cuid' }
    $out = $out -replace 'optnone', '' `
                -replace 'define hidden', 'define linkonce_odr' `
                -replace '"target-cpu"="gfx1030"', '' `
                -replace '"target-features"="[^"]+"', ''
    [IO.File]::WriteAllLines($stageLl, $out)

    # 4. text -> bitcode, with the patched assembler.
    & $patchedAs $stageLl -o $outBc
    if ($LASTEXITCODE -ne 0) { Fail "patched llvm-as failed for $name" }

    # ------------------------------------------------------ post-conditions
    # Asserted on the artifact rather than the intermediates, so what is checked
    # is what would be committed.
    $check = & (Join-Path $Hip 'bin\llvm-dis.exe') $outBc -o -
    if ($LASTEXITCODE -ne 0) { Fail "the produced $name.bc cannot be read back" }
    $noInline        = ($check | Select-String -Pattern 'noinline').Count
    $wrapperMentions = ($check | Select-String -Pattern '_ZL49__llvm_zluda_mma').Count
    $intrinsics      = ($check | Select-String -Pattern 'llvm\.zluda\.mma\.m16n8k16').Count
    $pairHelper      = ($check | Select-String -Pattern 'mma_sync_aligned_m16n8k32_row_col_f16_e4m3_e4m3_f16_pair').Count

    Note "noinline             : $noInline   (must be > 0 -- the mma helpers that stay calls)"
    Note "inlined wrapper refs : $wrapperMentions   (must be 0 -- see the note below)"
    Note "llvm.zluda.mma names : $intrinsics   (must be > 0)"
    Note "pair helper present  : $pairHelper   (must be > 0)"

    if ($noInline -eq 0 -or $pairHelper -eq 0 -or $intrinsics -eq 0) {
        if (-not $KeepOnFailure) {
            Copy-Item -LiteralPath $backup -Destination $target -Force
            Write-Host '  backups restored' -ForegroundColor Yellow
        }
        Fail "$name failed its post-conditions; intermediates are in $work"
    }
    # The wrapper count is the Step 0 gate, and it is NOT the alwaysinline count:
    # the attribute is consumed by the inlining it causes, so a module where the
    # call was inlined has *no* alwaysinline left to count. Counting it therefore
    # measures the opposite of what is wanted, and the first version of this
    # script did exactly that and reported a success as a failure. What proves
    # Step 0 instead is that the mma wrapper no longer exists -- it is gone
    # because it was inlined into the FUNC(...) helpers, taking its noinline (and
    # its alwaysinline) with it.
    if ($wrapperMentions -ne 0) {
        Write-Host "  WARNING: the mma wrapper still appears $wrapperMentions time(s)." -ForegroundColor Yellow
        Write-Host '  The optnone removal did not take effect, so the intrinsic is still behind a call' -ForegroundColor Yellow
        Write-Host '  and CombineMMAPass will not pair. Next lever: force alwaysinline onto the mma' -ForegroundColor Yellow
        Write-Host '  helper definitions, or compile with the patched clang (see the header comment).' -ForegroundColor Yellow
    }

    Copy-Item -LiteralPath $outBc -Destination $target -Force
    Note "written            : $target"
}

Write-Host ''
Write-Host 'Done. Next, in this order:' -ForegroundColor Green
Write-Host '  1. bump the cache marker in zluda/src/impl/module.rs (it currently reads /fp8-inline-r1),'
Write-Host '     or a warm cache keeps serving modules the pre-change backend compiled.'
Write-Host '  2. rebuild ZLUDA.'
Write-Host '  3. gates: mma-helper s_swappc count -> 0, then v_wmma halving on module 14 under'
Write-Host '     -mllvm -print-after=zluda-combine-mma. Only then measure frame time.'
Write-Host "  4. to undo everything: git -C `"$ZLUDA`" checkout -- ptx/lib/zluda_ptx_impl.bc ptx/lib/zluda_ptx_impl_constrained.bc"
