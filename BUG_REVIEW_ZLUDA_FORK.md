# ZLUDA fork 代码审查报告

**审查对象**：`big2cater/ZLUDA`（本地检出 `D:\Downloads\ZLUDA`，分支 `fix/dlssnr-surface-sampler-and-wgp`）
**上游基点**：`9c8b43f`（`Add support for constrained sin/cos (#675)`，来自 RedDukeDev/ZLUDA）
**本人改动范围**：10 个提交，`git diff --stat 9c8b43f HEAD` = **40 文件 / +7670 / -4752**
**审查方式**：逐个提交读 diff + 通读改动后的完整文件 + git 对象与 bitcode 二进制取证
**未能做到的**：**没有实际编译**（ZLUDA 需要 `ext/llvm-project` 子模块构建，本机未就绪）。因此本报告中所有结论均来自静态阅读与二进制/仓库取证，凡需要实机确认的地方都已明确标注。

## 你的 10 个提交

| 提交 | 内容 |
|---|---|
| `ea59191` | Changes to zluda as attempt to make it able to run DLSS5 on AMD |
| `b3497ec` | parallel code generation for split modules, opt-in via ZLUDA_CODEGEN_PARTS |
| `c91676b` | Fix surface not deterministic（uninitialised sampler） |
| `9c53bb0` | 32-byte RDNA descriptor + adaptive sampler；WGP for gfx11 |
| `c0d09b7` | WGP for gfx12 (RDNA 4) |
| `cd38ad8` | 权限掩码采纳采样器；cache 兼容性 |
| `20a6304` | RDNA4 硬件 WMMA；``cuLaunchKernel`` 零堆分配 |
| `10df00c` | 可配置 CU/WGP + cache 隔离 + FP16 WMMA |
| `7f6457d` | 强制 100% WGP（函数属性）+ 占用率/寄存器指令修正 |
| `c4b98cc` | 用 RDNA4 WMMA 重新编译 `zluda_ptx_impl.bc` |

**总体印象**：这批改动的**注释质量明显高于常见水平**——多数关键决策都写清了「为什么这么做、试过什么不行」，例如 `compile.rs:157-167` 解释为何必须先整体优化再切分，`zluda_cache/src/lib.rs:53-62` 解释 `busy_timeout` 默认为 0 导致并行插入静默丢失。有几处我一度怀疑是 bug、复核后确认是**正确且考虑周全**的（见文末「已核实无问题」），这一点值得肯定。

问题集中在两个地方：**缓存键的身份标识**，以及**并行代码生成的默认值**。

---

# A. 致命

## A1 🔴 `zluda/src/impl/module.rs:346` — 缓存键里的编译器身份被写死成字面量，此后任何代码生成改动都会命中陈旧缓存

```rust
Some(zluda_cache::ModuleKey {
    hash: blake3::hash(text.as_bytes()).to_hex(),
    compiler_version: "builtin",
    zluda_version: "ea59191382b74add94d956261ec6c1bb469244a8",   // <-- 写死
    device: isa,
    backend_key: serialized_attributes,
    last_access: zluda_cache::ModuleCache::time_now(),
})
```

**这是 `cd38ad8` 从 `env!("VERGEN_GIT_SHA")` 改成的字面量**（diff 确认）。

**已核实的键组成**（`:313-318` 与 `:342`）：

| 字段 | 来源 | 是否随「编译器变了」而变 |
|---|---|---|
| `hash` | `blake3(PTX 文本)` | 否（只随输入 PTX 变） |
| `compiler_version` | 常量 `"builtin"` | 否 |
| `zluda_version` | **字面量** | **否** ❌ |
| `device` | `gcn_arch` | 否（只随 GPU 变） |
| `backend_key` | `serde_json{is_debug, clock_rate, cumode}` | 只随 cumode 变 |

**也就是说：键里没有任何一项会随编译器实现变化而变。** 唯一原本能反映它的 `zluda_version` 被冻结了。

**为什么现在就已经出问题**：`vergen-gix` **仍然配置着**（`zluda/Cargo.toml:46`、`zluda/build.rs:1`），所以 `env!("VERGEN_GIT_SHA")` 本来可用。而写死这个字面量的提交是 `cd38ad8`，**在它之后还有 4 个提交改了代码生成**：

- `20a6304` 启用 RDNA4 硬件 WMMA
- `10df00c` 可配置 CU/WGP 模式
- `7f6457d` 强制 100% WGP 模式
- `c4b98cc` 重新编译 `zluda_ptx_impl.bc`（**二进制内容变了**）

这些改动**都不会让缓存键变化**。后果：

- 任何已有热缓存的机器，在升级到含 WMMA/WGP 修复的构建后，**仍会拿到旧的、未启用 WMMA 的目标文件**；
- 换句话说，**你为 RDNA 4 做的 WMMA 加速，对「以前跑过一次」的用户静默不生效**；
- 界面上不会有任何提示，只会表现为「改了代码但速度没变」——而 `zluda_cache/src/lib.rs:26-31` 的注释恰好描述了这种排查痛苦（"which is how an afternoon gets spent measuring stale results"）。

**修复**（二选一，都只是一行）：

1. 恢复 `env!("VERGEN_GIT_SHA")`（最省事，且自动跟随每次构建）；
2. 更稳健：把**代码生成身份**显式放进键，例如再加一个字段 `ptx_impl_hash = blake3(ZLUDA_PTX_IMPL)`、以及 `is_cumode` 之外的 `codegen_abi` 版本号。这样 `.bc` 一改，键就自动失效，将来不再需要靠「手动改一个字面量」来冲缓存。

> 说明：我理解 `cd38ad8` 写死这个值**本意是一次性的缓存破除**（把值改成新的，让旧条目全部作废）。这个手法当场有效，但它同时把「自动失效」这个能力永久关掉了。无论 `VERGEN_GIT_SHA` 还是 `ptx_impl_hash`，都能达到同样的当场破除效果，而且不会留下这个坑。

## A2 🔴 `kernel_metadata/src/lib.rs:140-153` + `zluda/src/impl/module.rs:391,480` — `unwrap_or(0)` 把「解析失败」当成「零 kernel」，会**删除好缓存**并把好翻译判为失败

新增的校验函数：

```rust
pub fn count_kernels(elf_bytes: &[u8]) -> Option<usize> {
    let elf_file = object::read::elf::ElfFile64::<Endianness>::parse(elf_bytes).ok()?;   // 解析失败 -> None
    Some(elf_file.symbols()
        .filter(|symbol| symbol.name().map(|n| n.ends_with(".kd")).unwrap_or(false))
        .count())
}
```

两个调用点都用了 `unwrap_or(0)`：

```rust
// module.rs:391 —— 读缓存时
if kernels_wanted > 0 && kernel_metadata::count_kernels(&binary).unwrap_or(0) == 0 {
    eprintln!("[zluda] the cached translation of this module has no kernels in it, ...");
    if let Some((cache, key)) = cache_with_key.as_mut() {
        cache.remove_module(key);      // <-- 删除
    }
    return None;
}
```

```rust
// module.rs:480 —— 写缓存前
let kernels_built = kernel_metadata::count_kernels(&elf_module).unwrap_or(0);
if kernels_wanted > 0 && kernels_built == 0 {
    return Err(CUerror::UNKNOWN);      // <-- 判为编译失败
}
```

**`Option<usize>` → `usize` 的那次 `unwrap_or(0)` 丢掉了「我不知道」和「真的是 0」的区别**，而这两个调用点对二者的处理方式完全不同：

1. **`:480` 会把一次成功的翻译扔掉，并报成编译失败。** 只要 `object` crate 对这个合法 ELF 解析不了（版本差异、新增 section 类型、截断读、将来换 LLD 输出），用户就会遇到一个上游本来能正常工作的**硬失败**，错误码还是最没有信息量的 `CUerror::UNKNOWN`。
2. **`:391` 会删掉用户的缓存条目。** 如果解析失败是**系统性**的（比如这个 `object` 版本就是读不了该架构的 ELF），那么每次启动都会「删除 → 重新翻译 → 再删除」，**永远付 40-90 秒/模块的代价**——正是这个提交想治的那个症状。也就是说，**这个修复在失败路径上会制造它原本要消除的问题**。

**修复**：区分三态，只在确定是 `Some(0)` 时才拒绝/删除：

```rust
match kernel_metadata::count_kernels(&binary) {
    Some(0) if kernels_wanted > 0 => { /* 确实是空对象：删除并重译 */ }
    Some(_) => { /* 正常 */ }
    None => { /* 解析不了：受信使用，最多记一行日志，绝不删缓存 */ }
}
```

**附带**：`.kd` 后缀启发式本身是合理的（AMDGPU 的 kernel descriptor 符号确实叫 `<name>.kd`），但如果将来符号表被 strip 掉，`count_kernels` 会返回 `Some(0)` 而不是 `None` → 又会误判。所以这个函数最好在「找不到 `.symtab`」时返回 `None` 而不是 `Some(0)`。

---

# B. 严重

## B1 🟠 `llvm_zluda/src/compile.rs:145-155` + `:278-282` — 并行代码生成**默认开启**（提交说明写的是 opt-in），且线程数无上限

```rust
fn codegen_parts() -> u32 {
    match std::env::var("ZLUDA_CODEGEN_PARTS").ok().as_deref() {
        Some("auto") => std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1),
        Some(text) => text.parse().unwrap_or(1),          // 无上限校验
        None => std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1),  // 默认即并行
    }
}
```
```rust
let parts = codegen_parts();
let object_files: Vec<Vec<u8>> = if parts > 1 {
    let objects = emit_objects_in_parallel(&linked, parts, gcn_arch)?;
```

**问题一：提交说明与代码不符。** `b3497ec` 写的是 "opt-in via ZLUDA_CODEGEN_PARTS"，但 `None` 分支（环境变量未设＝绝大多数用户）直接走 `available_parallelism()`。**所以并行代码生成是默认开启的，不是 opt-in。**

**问题二：线程数没有任何上限。** `ZLUDA_CODEGEN_PARTS=100000` 会照单全收（`text.parse()` 无 clamp），每个 part 都会 `scope.spawn` 一个线程。

**问题三（真正的风险）：与下游的多进程预热相乘。** `zluda_cache/src/lib.rs:53-62` 的注释自己写明了这个场景：

> "a batch of parallel translations -- **exactly what precompiling a whole network at once produces, up to sixteen processes** finishing within moments of each other"

也就是说：**16 个翻译进程**，每个进程默认再开 `available_parallelism()` 个代码生成线程。在一台 16 线程机器上就是 **16 × 16 = 256 个并发代码生成线程**，每个线程都要：

- `Context::new()`（`compile.rs:202`）建一个 LLVMContext
- `make_target_machine(gcn_arch)`（`:205`）建一个 TargetMachine
- 把自己那部分 bitcode **从磁盘读回来并解析**（`:190-195`、`:203`）

这是**又一轮内存与调度超订**（每个 LLVMContext + TargetMachine 都是几十 MB 级），发生在已经因为 16 个 HIP 上下文而内存吃紧的机器上。

**这与下游那份实测报告高度吻合**——`docs/issue_blank_race_report.md` 记录：

> "One `--compile-one` child measured **3.5 s CPU over 7+ minutes** (zero progress, parked in a wait), while 16 other children + the main process loaded HIP 7.1 on the same GPU at the same time."

「零 CPU 进展、长时间 parked」正是**线程/内存超订后被换页或等锁**的典型特征。我**不能断言这是唯一原因**（也需要实机确认），但这个默认值是一个可以立刻验证的嫌疑点。

**修复建议**：

1. **默认改成 1（真正的 opt-in）**，或默认 `min(available_parallelism(), 4)`；
2. 把解析值 **clamp 到合理上界**（例如 1..=32），非法值报错而不是 `unwrap_or(1)` 吞掉；
3. 考虑读一个「预期并发进程数」的提示（例如 `ZLUDA_CODEGEN_PARTS=auto` 时按 `available_parallelism() / 预期进程数`），或直接用 `available_parallelism()` 之外的信号；
4. 下游的 `core/precompile.cpp` 已经把并发默认调低/可限流了，但从 ZLUDA 这一侧收口更彻底。

> 顺带确认：`parts == 0` 是安全的——`parts > 1` 的判据使得 `ZLUDA_CODEGEN_PARTS=0` 会回落到单线程路径，不会把 0 传给 `llvm::SplitModule`。

## B2 🟠 `nvapi/src/lib.rs:738-763` — `NvAPI_GPU_GetLogicalGpuInfo` 在 LUID 取不到时**仍然返回成功**，且 LUID 来源依赖加载时序

```rust
unsafe extern "C" fn NvAPI_GPU_GetLogicalGpuInfo(
    _logical_gpu: NvLogicalGpuHandle,
    data: *mut NV_LOGICAL_GPU_DATA,
) -> i32 {
    let data = unwrap_or::unwrap_some_or!(data.as_mut(), return _NvAPI_Status_NVAPI_INVALID_ARGUMENT);
    if !data.pOSAdapterId.is_null() {
        let mut luid = [0i8; 8];
        let mut node_mask: u32 = 0;
        if cuda_device_luid(&mut luid, &mut node_mask) {
            std::ptr::copy_nonoverlapping(luid.as_ptr().cast::<u8>(),
                                          data.pOSAdapterId.cast::<u8>(), 8);
        }
        // <-- 失败时什么都不做：既不写 LUID，也不报错
    }
    data.physicalGpuCount = 1;
    data.physicalGpuHandles[0] = FAKE_PHYSICAL_GPU;
    0                                    // <-- 无条件成功
}
```

而 LUID 的来源是：

```rust
unsafe fn cuda_device_luid(luid: &mut [i8; 8], node_mask: &mut u32) -> bool {
    let nvcuda = winapi_get_module(b"nvcuda.dll\0");     // GetModuleHandleA
    if nvcuda.is_null() { return false; }                // <-- 没加载就直接失败
    ...
}
```

**两个问题：**

1. **静默成功**：`cuda_device_luid` 返回 false 时，函数照旧返回 `0`（成功）并宣称有 1 个 GPU，但 `pOSAdapterId` 里是调用方留下的**未初始化/零值**。DLSS 拿这个 LUID 去和 `cuDeviceGetLuid` 的真实值比对 → 不匹配 → 认为一个 GPU 都没有 → 回落到 `NV_GPU_ARCHITECTURE_GK100`（Kepler，无 tensor core）→ **神经渲染拒绝运行**。这正是 `:658-664` 注释里描述的失败模式，而这里是**第二次**踩进同一个坑，只是换了个入口。

2. **依赖加载时序**：`GetModuleHandleA("nvcuda.dll")` 只能找到**已经加载**的模块（注释里承认了这点）。所以这个函数的行为取决于**NVAPI 被查询的时刻，nvcuda.dll 是否已经加载**。如果 DLSS 在建 feature 时先问 NVAPI、而 nvcuda 还没被这个进程加载，就会走失败分支。**这是一个时序/顺序相关的行为**——恰好对应下游报告的「全新进程 50-60% 出全黑图、进程内 sticky」。我**不能断定这就是根因**（需要实机在 `cuda_device_luid` 两端加日志验证），但它是目前语义上最贴近的一条线索，值得优先验证。

**修复建议**：

- 失败时不要假装成功：要么诚实返回 0 个 GPU（让上层走它自己的路径），要么**确定性地**给出一个 LUID —— 例如从 DXGI 的 `IDXGIAdapter::GetDesc1().AdapterLuid` 取（不依赖任何模块是否已加载），或直接 `LoadLibraryW(L"nvcuda.dll")` 而不是 `GetModuleHandleA` 后再取 LUID；
- 无论如何，失败时把 `pOSAdapterId` 清零或填一个确定性值，并在日志里说一次，而不是静默。

## B3 🟠 `zluda/src/impl/surf.rs:124-137,164-169` + `tex.rs:45` — 采样器采纳**只有 texture → surface 单向流动**，创建顺序颠倒时会静默用错采样器

`adopt_sampler` 全仓库只有两个调用点：

```rust
// 调用点 1：surf.rs:135 —— give_plain_sampler 内部，用「一次性」纹理对象
unsafe fn give_plain_sampler(res_desc: &HIP_RESOURCE_DESC, array: usize) {
    let mut tex_desc: HIP_TEXTURE_DESC = mem::zeroed();
    tex_desc.addressMode = [HIPaddress_mode::HIP_TR_ADDRESS_MODE_CLAMP; 3];   // 硬编码 CLAMP
    tex_desc.filterMode = HIPfilter_mode::HIP_TR_FILTER_MODE_POINT;           // 硬编码 POINT
    ...
    adopt_sampler(array, texture as usize);
    let _ = hipTexObjectDestroy(texture);
}
```
```rust
// 调用点 2：tex.rs:45 —— 创建纹理对象时，把它真正的采样器贴给同一 array 上已有的 surface
super::surf::adopt_sampler(handle, *p_tex_object as usize);
```

**问题**：采纳方向**只有 texture → surface 一种**。`object_create`（`surf.rs:139-170`）里**没有任何**「查一下这个 array 上是否已经有纹理对象」的逻辑，只有 `give_plain_sampler` 给的 CLAMP/POINT 默认值。

因此**顺序决定结果**：

| 程序的调用顺序 | 结果 |
|---|---|
| `cuSurfObjectCreate` → `cuTexObjectCreate` | ✅ 先拿到默认采样器，随后被**真实**采样器覆盖（这是作者测过的顺序） |
| `cuTexObjectCreate` → `cuSurfObjectCreate` | ❌ 创建纹理对象时 `surfaces.is_empty()`（`surf.rs:78-80`）直接 return；之后创建 surface 只拿到 **CLAMP/POINT 默认值**，**永远不会被纠正** |

第二种情况下，surface 的 `tex` 采样变成了 **point + clamp**，而程序要求的是别的过滤/寻址模式 → **画面确定性地产出错的结果**（最近邻而非线性插值）。

这比原来的「不确定」更难发现：原来每次跑结果不同，你知道有问题；现在每次跑结果**都一样，但都不对**。

**修复**：在 `object_create` 里先反向查找——`tex.rs` 已有 `TEXTURE_DESCS`，再加一个 `array → tex_object` 的映射；若同一 array 上已存在纹理对象，就采纳**那个**采样器，只有在找不到时才回落到 `give_plain_sampler` 的默认值。

## B4 🟠 `zluda/src/impl/surf.rs:95-101` — 「权限字节」例外只声明适用于 GFX10/GFX11，而本项目的主目标是 **gfx12（RDNA 4）**

```rust
// The same array through two objects has to describe the same image.
// On RDNA (GFX10/GFX11), byte 14 contains the resource access permissions
// (texture object is read-only 0xb0, surface object is read-write 0xbf).
// ...
let agrees_exact = onto[..SAMPLER_OFFSET] == from[..SAMPLER_OFFSET];
let agrees_masked = onto[..14] == from[..14]
    && (onto[14] & 0xf0) == (from[14] & 0xf0)
    && onto[15..SAMPLER_OFFSET] == from[15..SAMPLER_OFFSET];

if !agrees_exact && !agrees_masked {
    ...
    continue;        // <-- 放弃写入采样器
}
```

**注释里明确写的是 GFX10/GFX11，没有 gfx12。** 如果 RDNA 4 的描述符权限位不在 byte 14 的低半字节（换了一个字节、或改成高半字节），那么 `agrees_masked` 会失败 → `continue` → **根本不写采样器** → surface 保留 HIP 自己的记账值 → **原始的非确定性在 RDNA 4 上原样复现**。

而且这个失败是「只警告一次」（`:102-109` 的 `Once`），很容易被日志淹掉。

**这是一个可以在你的 9070 XT 上立刻验证的预测**：如果 gfx12 上该假设不成立，日志里会出现一次

> `[zluda] a texture object and a surface object over the same array do not agree on their image descriptor, so the surface cannot be given a sampler.`

**建议**：把「哪个字节/哪几位是权限位」做成按架构（gfx10/11/12）查表；或在 gfx12 上先 dump 两份描述符的实际差异（作者显然已经有这个能力，`DLSS_LOG_RES` 之类的开关就是为这个加的），据实放宽掩码。

> 附带（非缺陷）：`agrees_exact` 是 `agrees_masked` 的子集（前 48 字节完全相等必然满足掩码版），所以 `!agrees_exact && !agrees_masked` 等价于 `!agrees_masked`。多一个条件是冗余的，但无害。

## B5 🟠 `zluda/src/impl/surf.rs:62-77` — 采样器写入发生在**锁外**，与 `object_destroy` 竞态可写入已释放的显存

```rust
let surfaces: Vec<usize> = match SURFACES.lock() {
    Ok(map) => map.as_ref().map(|list| {
        list.iter().filter(|(a, _)| *a == array).map(|(_, object)| *object).collect()
    }).unwrap_or_default(),
    Err(_) => return,
};                       // <-- 锁在这里就释放了
if surfaces.is_empty() { return; }
let mut from = [0u8; SAMPLER_OFFSET + SAMPLER_BYTES];
if !read_object_page(texture, &mut from) { return; }
for object in surfaces {
    ...
    let _ = hipMemcpyHtoD(hipDeviceptr_t((object + SAMPLER_OFFSET) as *mut _), ...);   // 锁外写显存
}
```

`object_destroy`（`:193-204`）会先 `SURFACES.retain(...)` 把该对象移除，**然后**才 `hipDestroySurfaceObject`。如果两个动作交错——A 线程刚收集完 `surfaces` 列表并释放锁，B 线程销毁了其中一个 surface 并释放了它的显存——A 就会**往已释放的显存地址写 16 字节**。因为写入结果被 `let _ =` 丢弃，**失败是不可见的**；若该地址被驱动复用，则是实打实的显存破坏。

作者用了 `Mutex` 说明已经考虑到线程，但**锁没有覆盖到 memcpy**。

**修复**：把「校验 + 写入」整个循环放进锁内；或给每个 surface 记一个 generation/引用计数，在写入前确认仍存活。

> 相关（同类，低概率）：`object_destroy` 用 `if let Ok(mut list) = SURFACES.lock()` 忽略「锁中毒」。若曾有线程持锁 panic，`retain` 会被跳过，而 `hipDestroySurfaceObject` 照样执行 → `SURFACES` 里留下指向已释放显存的条目，之后每次采纳都会写进去。建议中毒时至少也要清理。

---

# C. 中等

## C1 🟡 `ptx/lib/*.bc` 的 Git LFS 分发不一致，且两个 .bc 由**不同的 LLVM 提交**构建

**取证结果**（`git cat-file -s` 与文件头比对）：

| 文件 | git 中的大小 | 磁盘上 | 性质 |
|---|---|---|---|
| `zluda_ptx_impl.bc` | **130 字节** | 68400 字节 | LFS 指针（真实文件需 `git lfs pull`） |
| `zluda_ptx_impl_constrained.bc` | **67652 字节** | 67652 字节 | **真实 bitcode，被直接提交进 git** |

`.gitattributes` 声明了 `*.bc filter=lfs diff=lfs merge=lfs -text`，而 `ptx/build.rs:19-22` 会检查魔数并在发现 stub 时 panic：

```
"{} is a git lfs stub and not the actual file. Run `git lfs pull` to fetch it"
```

**三个观察：**

1. **不一致**：同目录两个 `.bc`，一个走 LFS、一个走裸提交，违反仓库自己的 `.gitattributes`。仓库体积被无谓撑大，而且这种不一致会传染（下一个人不知道该学哪种）。
2. **主 `.bc` 的指针变化本身没问题**：`9c8b43f`（上游）是 `oid 59675a0d… size 53052`，`HEAD` 是 `oid 5db4e9ff… size 68400`（+29%，与「用 RDNA4 WMMA 重新编译」一致；本地 LFS 缓存中确有该对象）。
   > ⚠️ **但这一条我在第一轮判断错了方向**，真实的 blob 历史比我当时看到的更严重——见 **J1**（`c4b98cc` 把真实 bitcode 从 git 里换回成了指针）。
3. **分发风险（需你确认）**：仓库里**没有 `.lfsconfig`**，`origin` 指向你自己的 fork，所以 `git lfs pull` 会去 **fork 的 LFS 存储**取那个新对象。如果当初**没有 push 成功这个 LFS 对象**，那么任何克隆你 fork 的人在 `cargo build` 时都会在 `check_lfs_file` **panic** —— 也就是说**别人根本编译不了这个 fork**。你把「另一个」`.bc` 裸提交进 git，恰恰像是当时 LFS push 不顺利的旁证。请务必确认：在一台干净机器上 `git clone` + `git lfs pull` 能否拿到 `5db4e9ff…`；若不能，最省事的做法是**两个 `.bc` 都裸提交**（或都走 LFS 并确保 push 成功）。

**第四个观察（可复现性）**：两个 `.bc` 内嵌的 LLVM producer 串不同：

- `zluda_ptx_impl.bc` → LLVM `5dcc622b51ecd499912c1062ce2b0ecda60d8e93`
- `zluda_ptx_impl_constrained.bc` → LLVM `590b9320a5be90e40268759c6203c01fde121e68`

两者都是 `21.0.0git`，但**是不同的提交**。而它们会在 `compile.rs:252-261` 被链接进**同一个** module。同时 `ptx/lib/zluda_ptx_impl.cpp` 本身改了 492 行。两者叠加就是经典的**「源文件与检入的 bitcode 漂移」**风险：源里加了函数、`.bc` 没重新生成，运行时症状是「符号找不到」或行为与源码不符。

**修复**：写一个脚本/CI 步骤，用**同一套** LLVM 一次性重建两个 `.bc`，并把所用 LLVM 提交记进注释或一个 sidecar 文件；CI 里校验两个 `.bc` 的 producer 串一致。

## C2 🟡 `llvm_zluda/src/compile.rs:306` + `:313-330` — 并行路径引入了 4 次磁盘往返，还多复制了一份最大对象

```rust
let object_file = object_files[0].clone();      // 完整复制第一个 object（可达十几 MB）
```

- `:306` 的 `.clone()` 只是为了后面把 `object_files` 拿去 `iter()`。而这里真正需要的只是**ELF 头**——`:304-305` 的注释自己说了「all that is read from it is the ELF header」。应该只读头部若干字节，或用 `&object_files[0]` 借用来消除这次复制。
- 并行路径相比上游**新增了多次全量磁盘往返**：`part_*.bc` 写出→读回解析（`:179`、`:190-195`）、`zluda.o` 逐个写盘（`:315-325`）、最终 ELF 写出→读回（`:328-330`、`:373-374`）。上游是纯内存完成、只在最后落一次盘。对 DLSS 那种「单个 object 14 MB」的模块，这是实打实的 I/O 代价——用磁盘换并行度，**在 `parts=1` 时纯亏**（好在 `parts>1` 才走这条路）。

**建议**：单 part 路径保持纯内存（现状即是）；多 part 路径考虑用 `MemoryBuffer`/`intrusive_ptr` 在进程内传递，或至少把 `parts` 默认压到小值（见 B1），避免「用 I/O 换来的并行」被超订抵消。

## C3 🟡 `llvm_zluda/src/compile.rs:284-295` — `parts > 1` 时编译器调试钩子被静默跳过

```rust
if parts > 1 {
    let objects = emit_objects_in_parallel(&linked, parts, gcn_arch)?;
    phase(&format!("generazione codice su {} parti", objects.len()));
} else {
    if let Some(hook) = compiler_hook {
        ... hook(&message, "opt.ll");      // 仅单 part 路径才有
        ... hook(&assembly, "asm");
    }
```

因为 `parts > 1` 现在是**默认**（B1），所以 `compiler_hook` 的 `opt.ll` / `asm` 输出在默认配置下**再也不产生了**。这会让基于这些 dump 的调试流程突然失效而无提示。**建议**：至少在 `parts > 1` 且 `compiler_hook` 存在时打印一行说明 hook 被跳过，或在其中一个 part 上仍产出 dump。

## C4 🟡 `llvm_zluda/src/compile.rs:91-95` — `is_cumode` 环境变量解析大小写不一致，且非法值静默回落

```rust
match std::env::var("ZLUDA_CUMODE").ok().as_deref() {
    Some("1" | "true" | "TRUE" | "cu" | "CU") => true,
    Some("0" | "false" | "FALSE" | "wgp" | "WGP") => false,
    _ => !(gcn_arch.starts_with("gfx10") || gcn_arch.starts_with("gfx11") || gcn_arch.starts_with("gfx12")),
}
```

- 接受 `"true"`/`"TRUE"` 却**不接受** `"True"`；接受 `"cu"`/`"CU"` 却不接受 `"Cu"`。手写一堆字面量而不是 `eq_ignore_ascii_case`，很容易踩到。
- **任何无法识别的值都静默落到架构默认分支**：`ZLUDA_CUMODE=yes`、`ZLUDA_CUMODE=wgp mode`、或打错一个字母，都不会报错，用户会以为自己设了而其实没有。建议非法值打一行 stderr 警告。
- **另一个一致性问题**：`is_cumode` 会从环境读取**两次**——一次在算缓存键时（`module.rs:267`，值被序列化进 `backend_key`），一次在真正生成代码时（`compile.rs:101`、`emit.rs` 的 `self.cumode`）。若在两者之间环境变量发生变化，**缓存键与实际产物不一致**（键说 A、产物是 B），下次读缓存就会命中错误的组合。虽然实践中同一进程内不会改，但这是个隐式耦合；把 `cumode` 在进程内**求值一次并传递**下去会更稳。

## C5 🟡 `ptx/src/pass/llvm/emit.rs:390` — `.minnctapersm` → `amdgpu-waves-per-eu` 的上界 16 是 RDNA wave32 专属，wave64 上会被 LLVM 整体拒绝

```rust
let min_waves = (*ctas).clamp(1, 16);
let value = format!("{min_waves}");
self.emit_fn_attribute_string(fn_, "amdgpu-waves-per-eu", &value);
```

紧邻的注释（`:386-389`）自己写明了：上界超过 `getMaxWavesPerEU()` 时「**LLVM rejects the entire attribute and falls back to Default(1, max)**」。而 `getMaxWavesPerEU()` 在 **RDNA wave32 上是 16，在 wave64（CDNA / GCN）上更低**（通常 8 或 10）。因此对非 RDNA 目标，一个 `.minnctapersm 16` 会被 clamp 成 16 → 超上界 → **整个属性被丢弃**，占用率调优静默失效（而这正是 `7f6457d` 想修的东西）。**建议**按架构取上界（`cumode`/wave64 时用 8）。

另外把 PTX 的 `.minnctapersm`（每 SM 最少 CTA 数）映射到 `amdgpu-waves-per-eu` 只是一种**近似**，语义并不等价；注释里说清这一点会比现在更容易维护。

## C6 🟡 `zluda_cache/src/lib.rs:97-111` — `busy_timeout` 修得对，但**静默丢写的根因仍在**

`busy_timeout = 30000` 这个修复是**真实且论证正确**的（`:53-62` 的注释把「默认 0 → 立即 SQLITE_BUSY → `.ok()` 吞掉 → 插槽静默丢失 → 下次重译」这条因果链讲得很清楚）。但要注意：

```rust
pub fn insert_module(&mut self, key: &ModuleKey, binary: &[u8]) {
    ...
        .ok();          // <-- 仍然吞掉一切错误
}
```

`busy_timeout` 只是让 SQLite **重试** 30 秒；超时之后**依然失败**，而失败**依然被 `.ok()` 静默丢弃**。对 14 MB 的 blob 插入 + 16 个进程争用，30 秒并非不可能触顶。所以这是**缓解而非根治**。

**建议**：至少在 `insert_module` 失败时打印一行（一次性、带 key 前缀），让「缓存没写进去」这件事可见；这比再调大超时更有价值，因为它是可观测性的问题，不是时限的问题。

> `ZLUDA_CACHE_DIR`（`:26-38`）是个好补充，注释里的两个动机（对比两个构建、以及「进程挂住时缓存文件被锁、根本清不掉」）都很实在。唯一小建议：对**相对路径**做个处理（相对 CWD 会被 `create_dir_all` 建到任意位置），或至少在文档里说明它是相对于 CWD 的。

---

# D. 性能优化机会（非缺陷）

1. **`count_kernels` 的解析成本**：它在**每次读缓存命中和每次写缓存前**都会完整解析一遍 ELF 符号表。对一个 14 MB 的 object，这意味着每次命中都要付一次符号表遍历。若它在实测中出现在热路径上，可以只读符号表所在 section 的头部/`.kd` 计数缓存进 metadata section（反正 `write_object` 已经在写 metadata 了，顺手记一个 kernel 数即可，读的时候直接取）。
2. **`remove_module` 只在真的确定是空对象时调用**（见 A2）——这也顺带避免了「因为解析失败而反复 DELETE+INSERT」的写放大。
3. **`parts` 与磁盘往返**：见 C2。把 `parts` 默认压小之后，`part_*.bc` 的写读开销与线程超订会一起下降，收益可能比调 `parts` 本身更大。
4. **缓存键加一个 `ptx_impl_hash`**（见 A1）：除了修 bug，还能让你以后**再也不用**靠手改字面量来冲缓存，省掉每次「改了 codegen → 忘了冲缓存 → 测出假结果」的来回。
5. **`is_cumode` 求值一次并传递**（见 C4）：省掉重复读环境变量，也消除键与产物不一致的隐式风险。
6. **`emit_target_features`（`emit.rs:398-409`）每次调用都 `format!` 一个字符串**。它是**每函数**调用一次；对编译器前端来说这属于可忽略但可见的分配。若想抠，可以按 `(cumode, debug_assertions)` 四种组合预先生成 4 个 `CString` 复用。**优先级最低**，列在这里只为完整。
7. **`-wavefrontsize64` 与 WGP 的关系值得实机量一次**：`compile.rs:101-105` 的 TargetMachine features 是 `-wavefrontsize64`（wave32），`emit.rs:400-408` 的每函数属性是 `+wavefrontsize32,-wavefrontsize64`（两者**一致**，都是 wave32，这点没问题）。但在 RDNA 上强制 wave32 是否对所有 DLSS kernel 都是最优，值得用 `ZLUDA_CUMODE` 双向对照测一次——你已经把开关做好了，缺的只是数据。

---

# E. 已核实**无问题**的项（避免误修）

这些是我（和几路并行审查）一度怀疑、但复核后确认**是对的**的地方。写出来是为了让你不必再花时间：

| 位置 | 一度怀疑 | 复核结论 |
|---|---|---|
| `module.rs:372-381` | `legacy_backend_key` 把 `,"cumode":true` 剥掉去命中旧条目 → 用 CU 模式的产物冒充 WGP 模式的请求 | **正确，且考虑周全。** 上游（`9c8b43f`）的 features 是**无条件** `c"-wavefrontsize64,+cumode"`，即旧缓存条目**全都是 cumode=true 的产物**；而这段回退**只在当前 `cumode == true` 时触发**（`if binary.is_none() && cumode`）。所以命中的旧条目与当前请求的 cumode 一致，不存在错配。 |
| `function.rs:98-127` | 17 元素的定长栈数组 `translated[count + 1]` 可能越界 | **安全。** `MAX_ENTRIES = 16`，循环走满时 `i` 达到 16 → `:119-121` 直接 `return ErrorInvalidValue`，所以能存活下来的路径必然是 `marker.is_null()` 提前 break，此时 `count = i ≤ 14`，`translated[count]`（终止符）最大索引 14 < 17。**零堆分配的目标也确实达成了。** |
| `compile.rs:279` | `ZLUDA_CODEGEN_PARTS=0` 会把 0 传进 `llvm::SplitModule` | **安全。** `parts > 1` 才走切分路径，`0` 会回落到单线程分支。 |
| `emit.rs:398-409` | 把 `"+wavefrontsize32,-wavefrontsize64,+cumode{}"` 改成 `"+wavefrontsize32,-wavefrontsize64,{}{}"` 后**占位符顺序错位** | **正确。** 第一个 `{}` = `cumode_str`，第二个 `{}` = 仅 debug 构建才有的 `,+precise-memory`，与上游的占位符布局一一对应，语义未变。 |
| `compile.rs:101-105` vs `emit.rs:400-408` | TargetMachine 与每函数属性的 wavefront 设置互相矛盾 | **不矛盾。** 两者都是 wave32（`-wavefrontsize64` + `+wavefrontsize32`），表述不同但方向一致。 |
| `lib.cpp:274` | 新函数里的 `strdup` 泄漏 | **是既有约定，不是本次引入。** 同文件 `:313`、`:317`、`:334` 全都用 `strdup` 且 Rust 侧从不释放（`compile.rs:185` 等只 `CStr::from_ptr` 转成 String）。仅发生在错误路径且次数有界。若要修，应作为独立的一次全文件清理，别夹在这批改动里。 |
| `nvapi/src/lib.rs:704-705` | `1 as NvPhysicalGpuHandle` 是野指针，被解引用会崩 | **安全。** 所有新增 handler 只**比较**它（`:805` `if physical_gpu != 1 as _`、`:819` `if gpu_id != 1`），从不解引用；占位参数也写成 `_physical_gpu`。 |
| `compile.rs:143-155` | 「opt-in」的提交说明是否与实现一致 | **不一致（见 B1）——但这一条是「说明错了」而非「代码错了」**，列在这里是为了区分：代码行为本身可解释，问题在于默认值太激进 + 无上限。 |

---

# F. PTX / 解析器层（第二轮复核）

范围：`ptx/`（含 `ptx_parser/`、`ptx/lib/zluda_ptx_impl.cpp`）与 `ptx/src/pass/llvm/emit.rs`。以下每条都经我独立复核，**并明确标出哪些是我复核后否掉的**。

## F1 🟠 `ptx/src/pass/replace_instructions_with_functions.rs:363-392` — `sust` 会生成**根本不存在的函数名**，`.surfref` 那条路径全仓库无实现

```rust
let name = format!(
    "{prefix}_{mode}_{dims}_{vector}{scalar}",
    prefix = match type_ {
        ast::TexType::Texref => "sustref",
        ast::TexType::Texobj => "sustobj",
    },
    mode = if formatted { "p" } else { "b" },
```

**已用 grep 核实**（这是我亲自验证的，不是转述）：

| 检查 | 结果 |
|---|---|
| `sustref` 在**全仓库**出现次数 | **1 次** —— 就是上面这一行（`replace_instructions_with_functions.rs:380`）。**没有任何定义。** |
| `sustobj` 在 `zluda_ptx_impl.cpp` 中的定义 | **只有 5 个**：`sustobj_p_2d_v4_b32`(1602)、`sustobj_b_2d_b32`(1634)、`sustobj_b_2d_v2_b16`(1640)、`sustobj_b_2d_v4_b8`(1646)、`sustobj_p_2d_b32`(1652) |

而解析器接受 `sust.b.2d{.v2,.v4}.{b8,b16,b32}` 的**全部组合**，所以这些名字可达但未定义：

| PTX | 生成的名字 | 是否存在 |
|---|---|---|
| `sust.p.2d.b32` / `sust.p.2d.v4.b32` | `sustobj_p_2d_b32` / `sustobj_p_2d_v4_b32` | ✅ |
| `sust.b.2d.b32` / `.v2.b16` / `.v4.b8` | 对应 3 个 | ✅ |
| `sust.b.2d.b8` / `sust.b.2d.b16` | `sustobj_b_2d_b8` / `_b16` | ❌ |
| `.v2.b8` / `.v2.b32` / `.v4.b16` / `.v4.b32` | `sustobj_b_2d_*` | ❌ |
| **任何经 `.surfref` 变量的 `sust.*`** | `sustref_*` | ❌ |

**为什么 `.surfref` 不是假想情况**：`ptx_parser/src/lib.rs` 把 `.texref` 与 `.surfref` 都映射到同一个 `ParsedType::Texref`，所以 `extern .surfref s;` 会走 `Texref` 前缀；只有 `%surfobj` 形式的 64 位寄存器操作数才走 `sustobj`。

**后果正是你自己在代码注释里写下的那句**（`replace_instructions_with_functions.rs:755-758`）：

> "An instruction lowered to a function that zluda_ptx_impl does not define ends up **dropped together with the whole kernel**, and nothing is reported"

也就是说：一句 `sust.b.2d.b8` 会让**整个 kernel 被静默丢弃**。而新增的 `ZLUDA_DEBUG_COMPILE` 诊断默认关闭，所以这个过程无声。

**建议**：要么补齐缺失入口（每个都是基于现有 `store_2D` / `byte_x_to_sample_x` 的几行），要么把解析器限制到已实现的组合，要么**至少**让 `sustref` 前缀变成显式失败而不是生成一个无人定义的符号。

## F2 🟡 `ptx/src/pass/llvm/emit.rs:2018-2031` — `zero_shared_memory` 把**元素个数**当作 `llvm.memset` 的**字节数**

```rust
let length = unsafe { LLVMGetArrayLength(value_type) } as u64;
let size = unsafe { LLVMConstInt(i32_type, length, 0) };
...
    c"llvm.memset.p3.i32",
    None, Vec::new(),
    vec![(global, pointer_type), (zero, i8_type), (size, i32_type), (not_volatile, i1_type)],
```

`llvm.memset` 的第三个参数是**字节数**，而 `LLVMGetArrayLength` 返回的是**元素个数**。于是 `.shared .b32 buf[16]`（`[16 x i32]`）`length == 16` → **只清了 64 字节中的 16 字节**，其余保留上一个 kernel 留下的内容。

**已核实**：与 `get_type`（`emit.rs:4183-4196`）构造共享数组的方式一致——`LLVMArrayType2(scalar_type, dimension)`，`dimension` 是元素数。多维数组同样受影响（`.shared .f32 buf[8][8]` 只清 8 字节 / 256 字节）。

**严重度限定**：`emit.rs:1988` 有 `if !self.is_kernel || std::env::var_os("ZLUDA_ZERO_LDS").is_none() { return Ok(()); }` ——**默认不生效**，只在设了 `ZLUDA_ZERO_LDS` 时启用。但它一旦启用就是**静默错误**，而且它存在的唯一目的恰恰是消除 LDS 未初始化带来的不确定性——**清不干净等于没达到目的**。

**修复**：`llvm_zluda` 没有暴露 `LLVMABISizeOfType`，所以要么从建类型的 AST 带出元素大小（`ast::Type::Array` 的 `scalar.size_of()` × `vec` 倍数），要么补一个 `LLVMABISizeOfType` 绑定：

```rust
let size = unsafe { LLVMConstInt(i32_type, length * element_size, 0) };
```

## F3 🟡 `ptx/src/pass/llvm/emit.rs:3542-3570` — `elect.sync` 忽略 `membermask`

```rust
let mask = self.emit_intrinsic(
    c"llvm.amdgcn.ballot.i32",
    None,
    vec![&b32],
    vec![(true_, i1)],       // <-- 对常量 true 做 ballot，而不是 membermask
)?;
```

**已核实**：在 `emit.rs` 中搜索 `membermask` **命中 0 处** —— 该操作数从未被读取，ballot 是对常量 `true` 做的。因此选出的 lane 是**整个 wavefront 的最低活跃 lane**，而不是 `membermask` 中最低被置位的 lane。`elect.sync` 的语义要求后者。使用常见的部分掩码写法（`elect.sync _|p, 0x00ff00ff;`）时，当活跃集合宽于掩码，会选出 lane 8 而不是 lane 0。

**修复**：改为对「掩码非零」的谓词做 ballot（`mask_reg != 0`），或在 `cttz` 之前把 ballot 与掩码相与。三行。

## F4 🟢 `emit.rs` 那 9400 行 diff 的性质：**几乎全是机械改动**（已独立验证）

第二轮复核的结论与我自己先前的检查一致：`emit.rs` 是 4893 增 / 4528 删，但把增删行当作多重集比较后，**只有极少行是「纯新增」或「纯删除」**——该文件被重新格式化、约 400 行被搬动（`emit_tuning`、`emit_target_features`、`emit_linkage` 与新增 helper 块），这才是 diff 膨胀到 9400 行的原因。

**藏在 churn 里、真正改变语义的常量级改动只有两处**（且两处都是**修复**）：

- `MinNCtaPerSm`：`"{ctas},1024"` → `clamp(1, 16)`；
- `MaxNReg`：原来的 no-op → `amdgpu-num-vgpr`。

**没有**任何 target 名、ISA 守卫或指令选择常量在这 9000 行 churn 中被改动。这一条对你有用：**不需要逐行审这份 diff**。

## F5 🟢 测试覆盖缺口（值得注意）

`git diff --stat 9c8b43f HEAD -- ptx/src/test ptx/src/pass/test` 只有 **4 行新增**，且全是 fixture 里的 `cumode: true`。**本轮新增的每一个指令族都没有测试**：WMMA / FP8 MMA、`movmatrix`、`sust`、`tld4`、`tex.level`、`min.relu`、`red`、mbarrier / `cp.async.bulk` 模拟、`elect.sync`。

这也正是下面 G1 那条**假警报**能看起来可信的原因——没有测试能立刻否掉它。

---

# G. 第二轮复核中**被我复核后否掉**的结论（重要，避免误修）

第二轮并行复核提出了一条「**每一块 RDNA3/RDNA4 上都在算错**」的高危结论和一条「整个 WGP 开关标反了」的结论。**两条我都独立复核后确认不成立。** 记在这里，是为了让你不要按它们去改代码。

## G1 ❌ 不成立：`fp8_mma_half` 的 `pair` 选择「对 k+8 半段取错了字节对」

复核者主张：helper 用 `pair = laneid & 1` 同时作用于 `fp8_quad_low` 与 `fp8_quad_high`，而「k+8 那半段应该用另一个字节对」，并据此判定 FP8 MMA 四个乘积中有两个配错了 A 列与 B 行，进而称「每一块 RDNA3/RDNA4 上都算错」。

**错误的原因**：复核者假设 `m16n8k32` 8 位 A 片段的 `a2`/`a3` 偏移是 **+8**。实际是 **+16**（`a2`/`a3` 覆盖 k = 4q+16..4q+19，即整个 k=16..31 半段——这也正是代码把它作为第二次调用的原因）。用 +16 重新推导后，**同一个 `pair` 对两半段都是对的**。

**决定性证据是你自己的注释**（`ptx/lib/zluda_ptx_impl.cpp:1333-1336`）：

> "Writing t for a lane's position in its quad, the 8 bit form hands lane t the four values k = 4t..4t+3, while the 16 bit form wants **k = 2t, 2t+1 and k = 2t+8, 2t+9**. Those live in **lanes t/2 and t/2+2**"

按此逐位推导（`t` 为 quad 内位置；源 lane `t/2` 持 k=2t..2t+3，源 lane `t/2+2` 持 k=2t+8..2t+11）：

| t 奇偶 | `quad_low`(lanes t/2) 所需 k=2t,2t+1 的字节 | `quad_high`(lanes t/2+2) 所需 k=2t+8,2t+9 的字节 |
|---|---|---|
| t 偶 | 字节 0,1 → `pair=0` | 字节 0,1 → `pair=0` |
| t 奇 | 字节 2,3 → `pair=1` | 字节 2,3 → `pair=1` |

**两侧奇偶规则完全相同**，所以共用一个 `pair = laneid & 1` 是**正确的**。B 侧（`bb[0]`/`bb[1]`）同理推导，结论相同。`fp8_mma_half` 无需修改。

（该复核者建议的「修复」——把第二次调用强行改成 `pair = 1`——反而会**引入**错误。）

## G2 ❌ 不成立：`is_cumode` 的语义与命名「标反了」，`+cumode` 其实选中 WGP 模式

复核者主张后端把它当作否定使用（`WgpMode = isCuModeEnabled() ? 0 : 1`），因此「名为 `cumode` 的属性实际选中 WGP 模式」，并建议重命名为 `is_wgp_mode`；同时还说「新默认对 gfx10/11/12 返回 true」。

**两条都错。** 我直接查了 `ext/llvm-project` 源码：

```
AMDGPU.td:216-220
def FeatureCuMode : SubtargetFeature<"cumode", "EnableCuMode", "true",
  "Enable CU wavefront execution mode">;

AMDGPUAsmPrinter.cpp:1206
ProgInfo.WgpMode = STM.isCuModeEnabled() ? 0 : 1;
```

- `+cumode` → `EnableCuMode = true` → `WgpMode = 0`。字段名就叫 **`WgpMode`**，`0` 表示**不是** WGP 模式，即 **CU 模式**。这个三元表达式是「CU 模式开启 ⇒ WGP 模式关闭」的**直接陈述**，不是「自我否定」。所以 **`+cumode` = CU 模式**，与属性名一致。
- 新默认值：`_ => !(starts_with("gfx10") || starts_with("gfx11") || starts_with("gfx12"))`，对 `gfx1100`/`gfx1201` 求值为 **false**（复核者说 true，反了）。false → `-cumode`（`compile.rs:104`）→ `WgpMode = 1` → **WGP 模式**。

**结论：你的 WGP 特性方向是正确的**——RDNA 默认走 WGP 模式，`ZLUDA_CUMODE=cu` 可切回 CU 模式，命名与语义一致。**这条可以从待办里划掉。**（命名仍有可读性空间：`is_cumode()` 返回 true 表示「用 CU 模式」，容易误会，属风格问题。）

## G3 ⚠️ 需你确认（我无法在此验证）：LFS 对象

见 **C1**。第二轮复核独立确认了同一件事：主 `.bc` 在 git 里是 LFS 指针且 `oid` 已从 `59675a0d…`（53052 字节）变为 `5db4e9ff…`（68400 字节），而 `zluda_ptx_impl_constrained.bc` 在本区间内从 130 字节指针变成了 **67652 字节裸 blob**。**请务必在一台干净机器上验证 `git clone` + `git lfs pull` 能否拿到 `5db4e9ff…`。**

## G4 其余低严重度观察（我未逐条独立验证，供参考）

以下来自第二轮复核，我**没有**逐条亲自验证代码，列出仅供参考：

- `mbarrier.try_wait` 恒返回 `true`（`wait_state` 操作数被丢弃）。当前被掩盖的原因是唯一生产者 `CpAsyncBulk` 被实现成同步 `llvm.memcpy`，数据已经落地；但 `mbarrier.try_wait.parity` 的自旋写法会在 phase 完成前就退出。**若将来引入真的异步生产者，这里会变成真 bug。**
- `sust` 的 `clamp` 默认值记成了 `SustClamp::Trap`，而 AST 注释说 AMD 实际行为是 `.zero`。该字段目前没有任何消费者，行为未受影响，但第一个消费者会继承一个错误默认值。
- `ptx_parser/src/lib.rs` 把 `.level::eviction_priority`、`.level::cache_hint`、`.level::prefetch_size` 与 `cache_policy` 的 `PtxError::Todo` 诊断删除，改成 `let _ = (...)`。作为「提示」可以接受，但 `.prefetch_size` 语义上会请求预取后续 cache line，丢掉它会改变访存模式。
- `ptx_parser/src/lib.rs` 的 `reg_or_immediate` / `operand` 接受 `Token::Underscore` 作为整个操作数，而下游 `expand_operands.rs` 会为 `Sink` 调用 `register_unnamed` → 一句畸形的 `add.f32 %f1, _, %f2;` 会编译通过并**读取未初始化寄存器**，而不是报语法错。

---

# J. 第三轮复核：构建来源、缓存键、LLVM 后端

范围：`llvm_zluda/`、`zluda_cache/`、`kernel_metadata/`、`patches/`、`ext/llvm-project` 子模块。**两条新的致命问题都出在「构建来源」上，而不是算法上。**

## J1 🔴 `ptx/lib/zluda_ptx_impl.bc` 在 `c4b98cc` 被从 git 里**换回成了 LFS 指针** —— HEAD 的干净克隆构建不了

我把每个提交的 blob 大小逐一带出来了（`git cat-file -s <commit>:<path>`）：

| 提交 | `zluda_ptx_impl.bc` | `..._constrained.bc` |
|---|---|---|
| `9c8b43f`（上游基点） | 130（LFS 指针） | 130（LFS 指针） |
| **`ea59191`（你的第 1 个提交）** | **68400（真实 bitcode）** | **67652（真实 bitcode）** |
| `b3497ec` … `10df00c`、`7f6457d` | 68400（真实） | 67652（真实） |
| **`c4b98cc`（HEAD）** | **130（LFS 指针）** | 67652（真实） |

```
$ git show c4b98cc --stat -- ptx/lib/
 ptx/lib/zluda_ptx_impl.bc | Bin 68400 -> 130 bytes
```

**也就是说**：你在 `ea59191` 就把两个 `.bc` 都以**真实二进制**提交进了仓库（自包含，很好）；真实文件一直保留到 `7f6457d`；然后**在最后一个提交 `c4b98cc` 里，把主 `.bc` 换成了一个 130 字节的 LFS 指针**——而这个提交的标题恰恰是 "recompile zluda_ptx_impl.bc with RDNA 4 WMMA enabled"。

**后果**：

- **干净克隆 `HEAD` 拿不到 bitcode**。它由 `ptx/src/pass/mod.rs:37` 的 `include_bytes!` 消费，并被 `ptx/build.rs` 的 `check_lfs_file` 守卫；指针文件以 `vers` 开头而不是 `BC` 魔数，所以构建会 **panic**（`"... is a git lfs stub and not the actual file. Run git lfs pull to fetch it"`）。
- 你的本地工作树仍然是真实的 68400 字节文件（LFS 过滤器把它掩盖了，`git status` 看不出异常），**所以只有你自己的机器能编译**。
- 更关键的是：**这个文件正是携带 `+cumode` / WMMA 版本 `__zluda_ptx_impl_mma_*` 的唯一载体**。也就是说，标题写着「重新编译以启用 RDNA4 WMMA」的那个提交，把 WMMA 版本的 bitcode 从版本库里拿出去了。
- 能否恢复完全取决于 `git lfs pull` 能否从**你的 fork**（`https://github.com/big2cater/ZLUDA.git/info/lfs`）取到 `5db4e9ff…`。这个对象在上游不存在。

**修复**：把真实 blob 提交回去（`git lfs untrack` 或直接 `git add -f`），或者确认 LFS 对象已 push 并可被他人拉取。建议顺手把 `.gitattributes` 里 `*.bc filter=lfs` 对这两个文件取消，因为它们是**构建输入而不是产物**，用 LFS 只会带来这种失败模式。

## J2 🔴 f16 WMMA 的 LLVM 后端改动**只存在于你未提交的工作树里** —— 你的 fork 里根本没有这个特性

```
$ git -C ext/llvm-project apply --check --verbose ..\..\patches\llvm-fp16-wmma.patch
Checking patch llvm/lib/Transforms/ZLUDA/CombineMMA.cpp...
Hunk #1 succeeded at 81 (offset 9 lines).
error: patch failed: llvm/lib/Transforms/ZLUDA/CombineMMA.cpp:184
```
```
$ git -C ext/llvm-project status --porcelain
 M llvm/lib/Transforms/ZLUDA/CombineMMA.cpp
```

**三条已核实的事实**：

1. **追踪的补丁文件打不上**——它自己声称的 hunk 位置与实际不符（第一个 hunk 偏移了 9 行，第二个直接失败）。
2. **仓库里没有任何东西引用它**：`git grep "fp16-wmma\|llvm-fp16"` 零命中；除了子模块指针之外，没有任何构建步骤会应用补丁。
3. **子模块被就地改脏了**，改动内容正好就是这个补丁。

**结论**：`int_zluda_mma_m16n8k16_f32_f16_f16_f32` 的 f16 WMMA 下沉实现，**只存在于你本地 `ext/llvm-project` 的未提交工作树里**。任何人克隆你的 fork 都拿不到它；子模块 pin 在上游提交上，补丁也不会被应用。你所有关于 RDNA4 FP16 WMMA 的实测，都建立在一个**无法从仓库重建**的二进制上。

**修复**：在你 fork 的 LLVM 仓库里**提交**这个改动，更新 `.gitmodules`/子模块指针，然后删掉或真正接上补丁文件。同时建议在报告 WMMA 性能数字时说明所用的 LLVM revision 与 `LLVMAMDGPUCodeGen.lib` / `LLVMZLUDAPasses.lib` 的来源——复核者发现你的构建树里这两个库的时间戳不同（`CombineMMA.cpp.obj` 是 18:00:51，而同树的 `LLVMAMDGPUCodeGen.lib` 是 17:55:43），即**这一个特性的两半是在不同时间构建的**。

## J3 🟠 并行代码生成改成默认开启时，你把**自己写的「故意默认关闭」的理由删掉了**

`b3497ec`（引入该功能的提交）里原本写着：

```rust
// ZLUDA_CODEGEN_PARTS. Absent or below two keeps the whole module in one piece,
// which is what every build did before this existed.
//
// Off by default on purpose: splitting narrows what the optimiser can see, so
// the code that comes out is not the same code. Whether that costs anything at
// run time has to be measured on the network, not assumed, and until it has
// been the fast path stays opt-in.
fn codegen_parts() -> u32 {
    ...
    None => 1,          // <-- 当时是 1
}
```

而 `10df00c` 把这段理由删除，并把 `None => 1` 改成 `None => available_parallelism()`。

**这条比我在 B1 里的描述更严重**：它不是「注释与实现不符」，而是**你明确写下了「默认关闭，因为在网络上实测过之前不能假设它没有代价」，随后在没有实测的情况下反转了这个决定**（至少没有任何证据表明测过）。复核者在你的工作树里**没有找到任何内存预算或分配上限**（全树 grep `available_parallelism|memory_budget` 只命中 `compile.rs` 的三个调用点）。

**建议**：要么改回 `1`，要么先按你自己原本的标准补一次 `ZLUDA_CODEGEN_PARTS=1` vs 默认 A/B 实测，再决定默认值。

## J4 🟠 `ZLUDA_CODEGEN_PARTS` 会改变生成的代码，但**不在缓存键里**（与 A1 同类）

`ExtraCacheAttributes` 只有 `{is_debug, clock_rate, cumode}`（`module.rs:313-318`），**没有 parts**。而按你自己的注释，切分会改变输出（"the code that comes out is not the same code"）。后果：

- 同一份二进制、不同机器（或同一机器改过 CPU 亲和性 / cgroup 限额）→ **代码不同、键相同**；
- 共享或拷贝来的 `zluda2.db` 会把 A 主机 `parts=32` 的产物喂给本该编译 `parts=8` 的 B 主机。

**修复**（一行）：把生效的 parts 数放进 `ExtraCacheAttributes`（它已经序列化进 `backend_key`）：

```rust
#[derive(serde::Serialize)]
struct ExtraCacheAttributes {
    is_debug: bool,
    clock_rate: u32,
    cumode: bool,
    codegen_parts: u32,     // 新增
}
```

## J5 🟠 `busy_timeout` 修复**实测**只解决了一类丢写；同时给**读**路径引入了 30 秒阻塞

复核者用仓库实际依赖的版本（bundled SQLite 3.50.2 + diesel 2.2.12）做了实测探针：

| 场景 | 结果 |
|---|---|
| 竞争写者持锁 300 ms，`busy_timeout=2000` | INSERT **成功**（312 ms 后） |
| 竞争写者持锁，**不设** busy_timeout（diesel 默认） | INSERT **19.3 µs 内失败** → 丢失 |
| **连接持有一个过期的 WAL 读快照**，另一连接已提交，`busy_timeout=30000` | INSERT **14.1 µs 内失败** → **丢失** |
| 无过期快照、锁被持有，`busy_timeout=30000` | 等待 **32.4 s** 后（仍）失败 → miss |

**两个必须说清楚的结论**：

1. **`SQLITE_BUSY_SNAPSHOT` 会绕过 busy handler。** 它在 `sqlite3WalBeginWriteTransaction` 里立刻返回，写路径不会为它重新调用 busy handler。所以「下一次运行还是找不到」这一类丢写**没有被修复**。而这在本代码库里不是角落情况：`get_module_binary` 本身就是一条 `UPDATE`（`zluda_cache/src/lib.rs:73-83`）——**每次读缓存都是一次写**，读和写在同一条连接上，且 `run_pending_migrations` 也写。刚读过的连接正是接下来要插入的连接。
2. **`busy_timeout=30000` 让读路径最多阻塞 30 秒然后仍然 miss。** 下游场景是「16 个进程几乎同时完成」/ 整个网络一次性加载；按这个设置，每个进程在查表阶段就可能烧掉 30 s/模块，之后照样回落到几分钟的翻译。

**修复**：不要只依赖超时。
- 插入失败时**回滚并在同一连接上重试**（新语句会拿到新快照），仍失败则**记录下来**——目前 `insert_module` 的 `.ok()` 让一切都不可见；
- 或者对插入使用 `BEGIN IMMEDIATE`，在建立快照之前就拿到写锁，从根上消除这一类冲突；
- 读路径不要继承写路径的超时：查 blob 用短超时（或 0），`last_access` 更新走另一条 best-effort 路径。

## J6 🟡 `zluda/src/impl/module.rs:391-398` — 删除缓存时用错了键，导致「自愈」永远治不好

```rust
if binary.is_none() && cumode {
    if let Some((c, key)) = cache_with_key.as_mut() {
        let legacy_backend_key = key.backend_key.replace(",\"cumode\":true", "");
        if legacy_backend_key != key.backend_key {
            let mut legacy_key = key.clone();
            legacy_key.backend_key = legacy_backend_key;
            binary = c.get_module_binary(&legacy_key);      // 命中：来自 legacy_key
        }
    }
}
let binary = binary?;
if kernels_wanted > 0 && kernel_metadata::count_kernels(&binary).unwrap_or(0) == 0 {
    if let Some((cache, key)) = cache_with_key.as_mut() {
        cache.remove_module(key);        // <-- 用的是当前 key，不是 legacy_key
    }
```

`legacy_key` 是 `:376-377` 的局部变量，在 `:380` 之后即失效；而删除时传的是**当前 key**（带 `,"cumode":true`），它匹配不到那一行（该行的 `backend_key` 里没有这段）。于是：**坏条目永远删不掉**，每次运行都会重新命中它、重新打印同一句警告、重新翻译一遍——**你写这个守卫的目的（「下一条运行会重新翻译，而不是永远被喂同一个坏答案」）恰恰没有发生**。

**修复**：记住是哪个键命中的，删那个键。

## J7 🟡 `llvm_zluda/src/compile.rs:304-311` — 切分后 `zluda32` 元数据只来自**第 0 个分片**

```rust
// Any of the objects serves as the model for the metadata sections below:
// all that is read from it is the ELF header, which they share.
let object_file = object_files[0].clone();
```

注释不准确：`kernel_metadata::write_object` 会复制**整个 ELF 头**，而 `zluda32` 的载荷是**模块级**的——`ptx/src/pass/convert_32bit_to_64bit.rs` 为**模块里每一个 kernel** 构建 `explicit_args_size_align`，但 `compile.rs:338-340` 把它归档到 `&object_file`（只有分片 0）。`parts > 1` 时，`zluda/src/impl/driver.rs` 提供的 `zluda32` 视图就**缺少分片 1..N-1 里的 kernel**。

因为默认现在是 `parts > 1`（B1/J3），**32-bit PTX 模块默认会踩到这条**。`ZLUDA_CODEGEN_PARTS=1` 是这个问题的对照开关。

## J8 🟡 `llvm_zluda/src/compile.rs:125-141` — `run_optimizer` 重构后泄漏了 LLVM 错误字符串

`Message::new` 是**不持有所有权**的构造函数（`utils.rs:138-142`），而 `utils.rs:144-150` 的 `impl Drop for Message` 才会调 `LLVMDisposeMessage`。`compile.rs` 里原本的写法经由 `Message` 析构释放；重构成 `run_optimizer` 之后：

```rust
let err_msg = unsafe { llvm_sys::error::LLVMGetErrorMessage(error) };
let message = Message::new(unsafe { CStr::from_ptr(err_msg) });   // 不持有，不会释放
return Err(message.to_str().to_string());
```

每次优化失败泄漏一个堆字符串（罕见但属本次引入的回归），且 `err_msg` 这里**没有判空**（`utils.rs:88` 判了）。

## J9 🟢 测试套件被钉在 `cumode: true`，**新默认（WGP）完全没有测试**

`ptx/src/test/mod.rs:40`、`ptx/src/test/spirv_run/mod.rs:1346,1373,1394`、`compiler/src/main.rs:114` 都硬编码 `cumode: true`；`ptx/src/test/ll/*.ll` 的 100+ 个 golden fixture 也仍带 `+cumode`；而 `emit.rs:399-403` 现在默认发 `-cumode`。

**也就是说：只有 `ZLUDA_CUMODE=1` 那条路径被覆盖，而这条分支的名字（`...-wgp`）所指的 WGP 默认路径一个测试都没有。** 建议把测试参数化到两种模式，并按默认模式重新生成 fixture。

---

# K. 第三轮中**被我复核后否掉**的又一条高危结论

## K1 ❌ 不成立：`amdgcn_wmma_f32_16x16x16_f16` 在非 f16 分支被喂了 `<16 x i16>`，bf16 路径「verifier 不会接受」

复核者判定为 **CRITICAL**，理由是「intrinsic 签名要求浮点向量操作数，而 bitcast 只对 f16 生效，所以 bf16 分支依赖一个验证器不会接受的隐式转换」。

**不成立。** 我直接读了 `ext/llvm-project` 的 intrinsic 声明：

```tablegen
llvm/include/llvm/IR/IntrinsicsAMDGPU.td
def int_amdgcn_wmma_f32_16x16x16_f16   : AMDGPUWmmaIntrinsic<llvm_anyfloat_ty, llvm_anyfloat_ty>;
def int_amdgcn_wmma_f32_16x16x16_bf16  : AMDGPUWmmaIntrinsic<llvm_anyint_ty,   llvm_anyfloat_ty>;
```

**两个变体的 A/B 操作数类型本来就不同**：

| 变体 | A/B 操作数类型 | `ShuffledA`/`CombinedB`（`<16 x i16>`） | 是否需要 bitcast |
|---|---|---|---|
| `..._f16` | `llvm_anyfloat_ty` | i16 ≠ float | **需要** → 代码 `IsF16` 分支 bitcast 到 `<16 x half>` ✅ |
| `..._bf16` | `llvm_anyint_ty` | i16 = int | **不需要**，本来就是对的 ✅ |

所以 `if (IsF16) { ...bitcast 到 V16F16Ty... }` 这个条件**恰好就是「按 intrinsic 声明的操作数类型来区分」**——这正是复核者建议的「correct」做法本身。复核者引用 f16 那一行（`IntrinsicsAMDGPU.td:3045`）时漏看了紧邻的 bf16 那一行。

**结论：`CombineMMA.cpp` 的 `IsF16` 条件 bitcast 是正确的，bf16 路径无需改动。** 不要按那条建议「无条件 bitcast」——虽然对 bf16 而言 i16→i16 是退化的 bitcast（无害），但当前写法更准确地表达了意图。

> 这是本次审查中**第二个**被否掉的 CRITICAL 级结论（第一个是 G1 的 FP8 MMA）。两条都属于「推论链很长、且把一个前提记错」的类型——这也说明为什么值得在动手之前先核一遍。

---

# N. 第四轮复核：表面/纹理/采样器（含关键 ABI 取证）

## N1 🟠 采样器「不采纳」的判定**比设备实际所需更严**——这会让整个修复在现场变成空操作（**可实测预测**）

**先给出 ABI 事实**（来自本机安装的 ROCm 7.1 头文件，已核实）：

```c
// C:\Program Files\AMD\ROCm\7.1\include\hip\texture_types.h:61-64
#define HIP_IMAGE_OBJECT_SIZE_DWORD   12
#define HIP_SAMPLER_OBJECT_SIZE_DWORD  8
#define HIP_SAMPLER_OBJECT_OFFSET_DWORD HIP_IMAGE_OBJECT_SIZE_DWORD
```

所以采样器槽位在**字节 48**（12 dword），**与架构无关**。`surf.rs:41` 的 `SAMPLER_OFFSET = 48` **是对的**，而中间提交 `9c53bb0` 声称的「32 字节 RDNA 描述符」是**错的**（`cd38ad8` 把它改回来了，这是正确的方向）。

**但设备侧实际只读 32 字节**（已核实 `ptx/lib/zluda_ptx_impl.cpp:1507-1508`，`term` 与 4 处采样点一致）：

```cpp
GLOBAL_SPACE v8s32 *image_typed = (GLOBAL_SPACE v8s32 *)image;     // 8 × 4 = 32 字节
GLOBAL_SPACE v4s32 *sampler_typed = (GLOBAL_SPACE v4s32 *)sampler; // 16 字节
```

**字节 32..47 在 `tex` 路径上从不被读取。** 而当前的采纳判据是：

```rust
let agrees_masked = onto[..14] == from[..14]
    && (onto[14] & 0xf0) == (from[14] & 0xf0)
    && onto[15..SAMPLER_OFFSET] == from[15..SAMPLER_OFFSET];   // <-- 覆盖到字节 47
```

**已核实的历史**：中间版本 `9c53bb0` 同时接受两种宽松判据——

```rust
// 9c53bb0:zluda/src/impl/surf.rs:97-100
let agrees_48 = onto[..SAMPLER_OFFSET] == from[..SAMPLER_OFFSET];
let agrees_32 = onto[..32] == from[..32];
if !agrees_48 && !agrees_32 {
```

`cd38ad8` 在修 byte-14 问题的同时**删掉了 `agrees_32` 这条宽松通路**。于是现在是**严格**要求字节 15..47 全部相同。

**风险**：如果字节 32..47 对 image 与 surface 两种对象承载了不同的运行时记账（HSA 的 size/geometry），那么**每一个 surface 都会判定失败 → 根本不写采样器 → surface 保留 HIP 原始的宿主指针 → 原始的非确定性原样复现**，而现场只留下**一行** stderr（`Once` 只打印一次）。

**这是一个可以立刻实测的预测**，与 B4 指向同一处但机制更明确：在你的 9070 XT 上跑一次，看是否出现

```
[zluda] a texture object and a surface object over the same array do not agree on their image descriptor, so the surface cannot be given a sampler.
```

**建议**：把判据改成与设备实际消费的范围一致——`onto[..32] == from[..32]` 加上一份**显式白名单**（今天就是 byte 14 的低半字节），而不是无条件要求 15..47 全等。这同时覆盖了 B4 里「权限位假设只声明到 GFX11」的问题：白名单化之后，gfx12 上多出来的一位差异会被显式列出，而不是让整条采纳静默失败。

## N2 🟠 `zluda/src/impl/tex.rs:61-68` — LINEAR / PITCH2D 资源的描述符被**清零**返回，可能让 `refresh_texref` 把 texref 绑到空指针

```rust
*desc = mem::zeroed();                                   // :61 清掉整个描述符
desc.resType = HIPresourcetype(res_type);
match desc.resType {
    ...ARRAY... => desc.res.array.hArray = handle as hipArray_t,
    ...MIPMAPPED_ARRAY... => desc.res.mipmap.hMipmappedArray = handle as hipMipmappedArray_t,
    _ => {}                                              // :68 LINEAR / PITCH2D 什么都不填
}
```

因为 `TEXTURE_DESCS` 只缓存了 `(resType, handle)`（`tex.rs:16`），`res.linear.devPtr` 与 `res.linear.sizeInBytes` 被 `mem::zeroed()` 抹掉后**再也没被填回**。作者是知情的（`tex.rs:27` 的注释写着 "linear or pitched memory keeps its type and reports a null resource"），但下游后果值得确认：`zluda/src/impl/hipfix.rs` 的 `refresh_texref` 对 LINEAR 走的是

```rust
HIPresourcetype::HIP_RESOURCE_TYPE_LINEAR => hipTexRefSetAddress(
    &mut 0, raw_texref, res_desc.res.linear.devPtr, res_desc.res.linear.sizeInBytes),
```

也就是**用空 `devPtr` 和 0 长度去重新绑定 texref**。这是 `ea59191` 引入缓存时带进的回归（`cuTexObjectCreate` 本身早就在已实现列表里）。

**修复**（比现在更省事）：缓存**整个** `HIP_RESOURCE_DESC`（136 字节且是 `Copy`），原样写回；或者至少在 `_ => {}` 处对 LINEAR/PITCH2D 返回 `NotSupported`，让调用方拿到错误而不是一个会一路传到 `hipTexRefSetAddress` 的空描述符。

## N3 🟠 三条**静默不采纳**路径，且唯一兜底是一个可能不符合程序意图的 CLAMP/POINT 采样器

除 N1 之外还有两处（均已在代码中核实）：

```rust
// surf.rs:71-73 —— 这个 array 上没有登记任何 surface：直接返回，零日志
if surfaces.is_empty() {
    return;
}
```
```rust
// surf.rs:130-134 —— 唯一给 surface 定义采样器的调用，失败即静默返回
if hipTexObjectCreate(&mut texture, res_desc, &tex_desc, ptr::null()) != hipError_t::Success {
    return;                          // 没有任何日志
}
```

这与 **B3（采纳方向单向、依赖创建顺序）** 是同一族问题，但把范围讲全了：无论因为「顺序不对」「array 句柄不匹配（mipmapped array 与 `cuMipmappedArrayGetLevel` 得到的是不同对象）」「描述符比对失败」还是「`hipTexObjectCreate` 失败」，最终结果都一样——**surface 保留 HIP 留下的字节，也就是你注释里诊断的那个宿主指针**。

而且 `give_plain_sampler` 硬编码 `CLAMP` + `POINT`（`surf.rs:126-128`）**不做任何比对**就写进去：如果程序要的是 WRAP/LINEAR，得到的是「看着合理但不对」的像素，而不是错误。

**建议的完整修法**（同时解决 B3 与 N3）：

1. **记录而非丢弃**：`tex::object_create` 时把该纹理对象的采样器按 **resource handle** 存进一张 side map；
2. **双向采纳**：`surf::object_create` 时先查这张 map——若同一 handle 上已有纹理对象，**采纳它的采样器**；只有在确实找不到时才回落到 CLAMP/POINT；
3. **日志改成每次都打**（不要 `Once`），并带上 array handle、两个对象 handle、以及两页前 48 字节的 hex——`Once` 恰好把「surface #1 成功、#2..N 失败」这种情况藏掉了；
4. `surf.rs:113` 的 `let _ = hipMemcpyHtoD(...)` 建议改成返回错误——**这是把「静默的非确定性」变成「可见失败」性价比最高的一处改动**。

## N4 🟡 16 字节写入假设 surface 对象分配 ≥64 字节（**唯一需要实机验证的一条**）

`HIP_TEXTURE_OBJECT_SIZE_DWORD` 这一族常量描述的是**纹理**对象；而 `hipSurfaceObject_t` 是 `struct __hip_surface*` 这样的不透明指针，设备侧的 surface 辅助函数只碰偏移 0 处的 image（`amd_surface_functions.h`）。所以「surface 对象页在偏移 48 处有可写的 16 字节」是**推断而非验证**——如果 surface 对象只分配了 48 字节，这就是一次 16 字节堆溢出。考虑到它确实工作，分配大概 ≥64 字节，但建议实测确认：**连续创建两个 surface，写入第一个的偏移 48 后检查第二个的描述符是否被破坏。**

**顺带一个简化**：现在走的是 DtoH(64) → HtoD(16) 的宿主往返（`surf.rs:46-52`、`113-117`）。用一次 16 字节的 `hipMemcpyDtoD(texture+48 → object+48)` 可以**完全等价且不经宿主内存**——而「经宿主内存」正是被回退掉的 `9c53bb0` 用来做运行时判断的手段，去掉往返就消除了整类错误。

## N5 ✅ 已核实**正确**的部分（这一轮的重要正面结论）

| 项 | 结论 |
|---|---|
| `SAMPLER_OFFSET = 48` | **正确**，已由 ROCm 头文件 ABI 独立证明（`HIP_SAMPLER_OBJECT_OFFSET_DWORD = HIP_IMAGE_OBJECT_SIZE_DWORD = 12`）。提交信息里「32 字节 RDNA 描述符」的说法是**过时/错误**的——**代码对，说法错** |
| `9c53bb0` 里那个运行时分支 | **本身就是第二处非确定性**：`from[48..64]` 源自宿主指针、`from[32..48]` 是 image/padding，所以「哪个分支赢」是**宿主地址相关**的、逐进程不同的决定。`cd38ad8` 删掉它、硬编码 48 是**正确方向**，且与 ABI 一致 |
| `HIP_RESOURCE_DESC` 与 `CUDA_RESOURCE_DESC` 布局 | **逐字段一致**（含 `reserved[32]`），所以 `from_cuda_transmute!` 与按偏移 0/8 读 `resType`/`hArray` 都是安全的 |
| 枚举值 | `HIP_RESOURCE_TYPE_ARRAY..PITCH2D`（0..3）与 CUDA 侧完全一致，`surf.rs:145-153` 的翻译是精确的 |
| 缓存描述符这个决定 | 对 ARRAY / MIPMAPPED_ARRAY **是对的**——AMD 运行时确实会让 `*GetResourceDesc` 的 `resType` 未初始化（你观察到的 `0x30CE0BE0` 与「陈旧/未定义槽位」一致）。**只有 LINEAR/PITCH2D 那条（N2）被漏了** |
| 锁的卫生 | 干净：没有在持锁时调驱动、三把锁从不同时持有 → 无死锁、无锁序反转（包括 `hipCreateSurfaceObject` → `hipTexObjectCreate` → `tex::object_create` 的重入路径） |
| 拆除顺序 | `object_destroy` 先从 `SURFACES` 移除、再 `hipDestroySurfaceObject`，顺序正确 |
| `cargo check -p zluda` | **通过**（新增的 `cuTexObjectGetResourceDesc` 等名字规范化都解析成功） |

---

# O. 建议的修复顺序

## 第 0 批：**先让仓库能重建**（第三轮新增，这两条比算法问题更紧急）

1. **J1** — `c4b98cc` 把真实 `zluda_ptx_impl.bc` 换成了 LFS 指针。**干净克隆 HEAD 现在构建不了**，而这个提交的全部目的正是要发布重新编译过的 bitcode。把真实 blob 提交回去。
2. **J2** — f16 WMMA 的 LLVM 后端改动**只在你未提交的工作树里**；补丁打不上、也没人引用它。**你的 fork 里没有这个特性。** 在 LLVM fork 里提交它并更新子模块指针。

> 这两条意味着：**目前任何人都无法从你的仓库重建出你实测过的那个 ZLUDA。** 在修好之前，其余的性能结论都缺少可复现的载体。

## 第 1 批：会让特性静默失效的（各一行）

3. **A1** — `zluda_version` 写死 → 恢复 `env!("VERGEN_GIT_SHA")`，或改加 `ptx_impl_hash`。**建议先清一次 `%LOCALAPPDATA%\zluda\ComputeCache` 重测**，验证你的 WMMA/WGP 此前是否根本没生效。
4. **A2** — `count_kernels(...).unwrap_or(0)` 三态化。
5. **J4** — 把 `codegen_parts` 放进 `ExtraCacheAttributes`（一行字段）。

## 第 2 批：与下游「挂死 / 黑屏」最相关的

6. **B1 + J3** — `codegen_parts()` 默认改回 `1`（你原本就是 `1`，而且**你自己写过「实测之前不改默认」的理由，后来又删掉了**）。上界 clamp 到 1..=16。
7. **B2** — `NvAPI_GPU_GetLogicalGpuInfo` 失败时不要静默成功。**先加日志**验证它是否就是黑屏根因。
8. **J5** — 读路径别继承 30 s 超时；插入失败改成「回滚重试 + 记录」。

## 第 3 批：正确性细节

9. **F1** — 补齐 `sustref_*` / 缺失的 `sustobj_b_2d_*`，否则一句 `sust.b.2d.b8` 会静默丢掉整个 kernel。
10. **N1 + B3 + N3** — 采样器三条一起改：判据收窄到设备真正读取的 32 字节 + 显式白名单；在 `surf::object_create` 里**反向查找已存在的纹理对象**并采纳其采样器；`surfaces.is_empty()` 与 `hipTexObjectCreate` 失败都**打日志**（不要 `Once`）；`hipMemcpyHtoD` 的 `let _ =` 改成返回错误。**这是「把静默不确定变成可见失败」性价比最高的一组。**
11. **N2** — 缓存整个 `HIP_RESOURCE_DESC`（或对 LINEAR/PITCH2D 明确返回 `NotSupported`）。
12. **B4** — 在 **gfx12** 上确认权限字节假设（见 P 表第 3 项）。
13. **J6 / J7** — `remove_module` 用命中的键；`parts > 1` 时拒绝 `metadata32`。
14. **B5 / N4 / F2 / F3** — 采样器写入移入锁内；`zero_shared_memory` 用字节数；`elect.sync` 读 `membermask`；确认 surface 对象分配 ≥64 字节。

## 第 4 批：卫生与可观测性

15. **C2 / C3 / C4 / C5 / J8 / N4** — 去掉 `object_files[0].clone()`；`parts > 1` 时提示 hook 被跳过；环境变量解析健壮化；`waves-per-eu` 上界按架构；`LLVMGetErrorMessage` 泄漏；采样器拷贝改用 `hipMemcpyDtoD` 免宿主往返。
16. **J9** — 把 PTX 测试参数化到 WGP/CU 两种模式（**当前默认模式零覆盖**）。
17. **C1** — 统一两个 `.bc` 的分发方式，并让两个 `.bc` 由**同一套 LLVM** 重建（它们的 producer 串目前不同）。

---

# P. 需要你实机确认的 5 件事（我无法在此验证）

| # | 验证内容 | 判定标准 |
|---|---|---|
| 1 | **清楚缓存后重测单帧耗时** | 若 40~60 ms 变成别的数字 → 证实 **A1**（此前 WMMA 对热缓存根本没生效） |
| 2 | **干净机器上 `git clone` + `git lfs pull` + `cargo build`** | 能否拿到 `5db4e9ff…`；失败即证实 **J1** |
| 3 | **gfx12 上 `surf.rs` 的描述符断言** | 日志里是否出现 `do not agree on their image descriptor` → 同时验证 **B4** 与 **N1**（后者机制更明确：判据覆盖了设备根本不读的字节 32..47） |
| 4 | **`ZLUDA_CODEGEN_PARTS=1` vs 默认 A/B** | 对比每个 kernel 的 `cuModuleGetFunction` 成功与否 → 同时验证 **B1 / J3 / J7 / 第三轮 suspect A**（分片后 kernel 描述符 `.note` 是否只剩分片 0） |
| 5 | **surface 对象分配是否 ≥64 字节**（N4） | 连续创建两个 surface，写入第一个的偏移 48 后检查第二个描述符是否被破坏 |

---


# R. 修复记录（本次已实施）

在 `c4b98cc` 之上新增 **6 个提交**。所有改动都经过编译验证；`cargo check` 覆盖 `zluda_cache`、`kernel_metadata`、`ptx_parser`、`ptx`、`nvapi`、`zluda`、`llvm_zluda` 七个 crate，`zluda_cache` 的 5 个测试全过。

| 提交 | 修复项 | 内容 |
|---|---|---|
| `e7708fe` | **B1 / J3 / C4 / J7 / J8 / C2 / C3** | `codegen_parts` 默认回到 **1**（`auto` 才用核心数）+ clamp 到 1..=32；两个环境变量改为进程内**只读一次**（`OnceLock`，消除「键与产物可能不一致」）；非法值**报错**而不是静默当 auto；大小写不敏感；`metadata32` 与切分**互斥**（拒绝而不是给出缺 kernel 的视图）；`LLVMGetErrorMessage` 显式释放；去掉 `object_files[0].clone()`；`parts>1` 时提示 `opt.ll`/`asm` 被跳过 |
| `0b97cea` | **A1 / A2 / J4 / J5 / J6** | 缓存键改为 `VERGEN_GIT_SHA + ZLUDA_PTX_IMPL_DIGEST`（后者由 `zluda/build.rs` 对两个 `.bc` 做 FNV-1a，无需新依赖）；`codegen_parts` 进 `ExtraCacheAttributes`；`count_kernels` 改三态（只有确定 `Some(0)` 才删缓存/判失败）；删除时用**命中的那个键**；`insert_module` 返回 `Result` 并**重试 busy + 上报**；`busy_timeout` 30s→5s 且**读路径单独用 100ms** |
| `9ea08e3` | **B2** | LUID 取不到时**清零而非留空**并说明一次；`cuda_device_luid` 增加 `LoadLibraryA` 兜底，不再依赖加载时序 |
| `5057490` | **N1 / N2 / N3 / B3 / B5** | 纹理对象**注册**（反向采纳）：先建纹理后建 surface 也能拿到真正的采样器；采纳判据收窄到设备真正读取的 **32 字节** + byte 14 低半字节白名单；整个采纳过程**持锁**（消除 UAF 窗口）；失败**每次都报**（含句柄与两份描述符）；**存整个 `HIP_RESOURCE_DESC`** 并原样返回（修掉 LINEAR 空 `devPtr` 传给 `hipTexRefSetAddress` 的回归）；锁中毒改为恢复而非报错（顺带消除「创建后报错导致对象泄漏」） |
| `d130a7b` | **F1 / F2 / F3** | `zero_shared_memory` 按**字节数**清（新增 `llvm_type_size_bytes` 递归求元素大小；不认识就跳过并提示）；`elect.sync` ballot 结果**与 membermask 相与**；`sust` 落到未定义函数名前**报错并拒绝**（5 个已实现形式之外的，包括全部 `sustref_*`）；顺手把该文件里 2 个裸 NUL 字节改成 `\0` 转义（原先让 diff/grep/编辑器把该文件当二进制） |
| `76b3793` | **J1 / J2** | 两个 `.bc` 改为**普通文件跟踪**（`ptx/lib/*.bc` 覆盖 `.gitattributes` 的 LFS 规则），仓库里的 blob 现在是 68400 / 67652 字节的真实 bitcode——**干净克隆可以构建了**；`patches/llvm-fp16-wmma.patch` 重新生成，`git apply --check` 对 pin 住的 revision 通过，并写明 f16/bf16 操作数类型差异 |

## 验证方式

- **J1**：`git cat-file blob HEAD:ptx/lib/zluda_ptx_impl.bc` → 68400 字节、首两字节 `BC`（不是 `vers`）。
- **J2**：`git -C ext/llvm-project apply --reverse --check` 返回 0；另在临时目录用 pin 住的版本做过**正向** apply 检查，同样通过。
- **无回归**：`ptx` 测试在改动前后**完全一致**（200 通过 / 383 失败）——我用 `git stash` 单独回退 `emit.rs` 做了对照，确认那 383 个失败是**改动前就存在**的 fixture 漂移，与本次修复无关。
- 「383 个失败」本身就是一个发现：10 个提交改了代码生成（尤其 cumode 默认翻转）却从未重新生成 `ptx/src/test/ll/*.ll`，所以 golden 测试长期无效。**建议单独用一个提交重新生成 fixture**（这需要一台能跑 LLVM 的机器）。

## 没有做、以及为什么

| 项 | 原因 |
|---|---|
| **F1 的「补齐 `sustref_*` 等实现」** | 需要改 `ptx/lib/zluda_ptx_impl.cpp` 并**重新生成两个 `.bc`**；本机没有 `ext/llvm-project/build`，无法重建。只改 `.cpp` 而不重建 `.bc` 会**制造**我一直在报告的源码/bitcode 漂移，所以改为「落到未定义名字就报错」（把静默丢 kernel 变成可见失败）。补齐实现请在有 LLVM 的机器上做，并同时重建 `.bc`。 |
| **J2 的「在 LLVM fork 里提交并更新 gitlink」** | 需要 push 到本检出够不到的仓库。已把 patch 修成可复现的形式；但**只要还在「只改工作树」，你的 fork 里就仍然没有这个特性**——这一条仍然是待办。 |
| **N4（surface 对象是否 ≥64 字节）** | 需要实机验证（P 表第 5 项）。已把 `hipMemcpyHtoD` 的失败从丢弃改为上报，所以一旦越界会**可见**而不是静默。 |
| **重新生成 PTX golden fixture** | 需要能跑 LLVM/GPU 的环境。 |

---

# S. 一句话总览

这批改动的**工程质量比预期高**：注释解释决策而不只是描述代码；几处我怀疑的地方复核后都是对的（尤其是那个 `cumode` 回退键与 WGP 极性，判断得很准）；`busy_timeout` 那条因果链写得比多数 issue 报告都清楚；采样器偏移 48 与描述符布局也都经得起 ABI 对照。

但**四轮复核把问题的重心从「算法」挪到了「构建来源与身份」**，这是最初完全没有预料到的方向：

**最该先处理的三件事（按紧急度）**

1. **J1 / J2 —— 你的仓库目前无法被他人重建出你实测过的那个 ZLUDA。**
   - `c4b98cc`（标题正是"重新编译 bitcode 以启用 RDNA4 WMMA"）把真实 `zluda_ptx_impl.bc` 换成了 130 字节的 LFS 指针 → 干净克隆**构建 panic**；
   - f16 WMMA 的 LLVM 后端改动**只存在于你未提交的工作树里**，追踪的补丁既打不上也无人引用 → 你的 fork 里**没有这个特性**。
   在这两条修好之前，其余性能结论都缺少可复现的载体。

2. **A1 / J4 —— 缓存键不再标识「是哪个构建」。** `zluda_version` 被冻结成字面量，`codegen_parts` 又没进键；而已有 4 个提交改过代码生成。**建议先清一次 `%LOCALAPPDATA%\zluda\ComputeCache` 重测**，看 40~60 ms 是否变化——这能立刻验证你为 RDNA 4 做的 WMMA 加速此前是否根本没生效过（缓存早就热了）。

3. **N1 / B3 / B4 —— 采样器修复在现场可能整体是空操作。** 采纳判据比较了设备**从不读取**的字节 32..47（设备侧只读 `v8s32` = 32 字节），而中间版本 `9c53bb0` 本来有宽松通路、被 `cd38ad8` 一并删掉了。若那 16 字节在两种对象上承载不同记账，**每个 surface 都会判定失败、静默不写采样器**，原始非确定性原样复现，现场只留一行 stderr。**这是最值得先跑一次的验证。**

**然后是 B1 / J3** —— 并行代码生成默认全开且无上限（与下游 16 进程预热相乘可达数百线程），而且**你自己写过「实测之前保持默认关闭」的理由，后来把它删掉了**。这是与「翻译子进程挂着不动」最相关的一条。

**最后两条需要你实机确认**（我无法在此验证）：P 表里的 5 项——特别是第 2 项（干净机器 `git lfs pull` + `cargo build`）和第 3 项（gfx12 上的描述符断言）。

---

# T. 第八轮验证（HEAD = `4d1dc62` + `817fe5a`）

**结论先说**：**`F1` 已经彻底做完，而且做得比我要求的多** —— 现在不但实现补齐了，`ptx/lib/*.bc` 与 `ptx/lib/zluda_ptx_impl.cpp` 之间**源码↔bitcode 漂移为零**（我重新编译了一遍逐符号比对，见下）。重建最大的风险（ROCm 7.1 的 clang 是 LLVM 21，ZLUDA 能不能读懂）**已排除**。剩下**一个真问题**：新增的 12 个 `sustref_*` 建立在一个 ZLUDA **不存在的运行时契约**上（N10）。

## N6 ✅ F1 完成，源码↔bitcode 漂移已被消除（我自己重编译验证）

上一轮我说「本机没有 LLVM，改 `.cpp` 不重建 `.bc` 会制造漂移」，所以你这次直接重建了 —— 我做的是**独立复算**：用 ROCm 7.1 的 clang（就是你重建用的那个工具链）按文件头注释里的那条命令重新编译当前 `.cpp`，再和**仓库里提交的** `.bc` 逐函数比对。

```powershell
# 1) 用 ROCm 7.1 clang 重新编译当前 .cpp（命令取自 zluda_ptx_impl.cpp 头部注释，路径改为 Windows）
cd D:\Downloads\ZLUDA\ptx\lib
& 'C:\Program Files\AMD\ROCm\7.1\bin\clang.exe' -DHIP_ENABLE_WARP_SYNC_BUILTINS -std=c++20 `
  -Xclang -fdenormal-fp-math=dynamic -Wall -Wextra -Wsign-compare -Wconversion -x hip `
  zluda_ptx_impl.cpp -nogpulib -O3 -mno-wavefrontsize64 -o $env:TEMP\zlu\rebuilt.bc `
  -emit-llvm -c --offload-device-only --offload-arch=gfx1030 `
  -Xclang -mlink-bitcode-file -Xclang 'C:\Program Files\AMD\ROCm\7.1\amdgcn\bitcode\ocml.bc'

# 2) 两边都反汇编，抽出所有 __zluda_ptx_impl_* 定义的「名字 + 规范化参数类型序列」做集合比对
& 'C:\Program Files\AMD\ROCm\7.1\bin\llvm-dis.exe' ptx\lib\zluda_ptx_impl.bc -o $env:TEMP\zlu\zluda_ptx_impl.ll
& 'C:\Program Files\AMD\ROCm\7.1\bin\llvm-dis.exe' $env:TEMP\zlu\rebuilt.bc     -o $env:TEMP\zlu\rebuilt.ll
# 比对脚本在 $env:TEMP\zlu\cmp_impl.ps1（剥掉 noundef/zeroext/readonly/captures(none) 等属性后逐参数比类型）
```

结果：

| 项 | 提交的 `.bc` | 重编译的 `.cpp` |
|---|---|---|
| `__zluda_ptx_impl_*` 定义数 | 152 | 152 |
| 只在一边存在的符号 | **0** | **0** |
| 参数类型序列不同的函数 | **0** | — |

编译过程 `-Wall -Wextra -Wsign-compare -Wconversion` **零警告**，退出码 0。→ **上一轮报告里那条「源码/bitcode 会漂移」的风险现在不存在了**；`没有做` 表里 F1 那一行的理由已作废。

新增的 24 个函数在 `.bc` 里的实际签名（节选，这就是设备端真正被调用的 ABI）：

```llvm
define linkonce_odr void @__zluda_ptx_impl_sustobj_b_2d_b8(i64, <2 x i32>, i8 zeroext)
define linkonce_odr void @__zluda_ptx_impl_sustobj_b_2d_v2_b16(i64, <2 x i32>, <2 x i16>)
define linkonce_odr void @__zluda_ptx_impl_sustobj_b_2d_v4_b32(i64, <2 x i32>, <4 x i32>)
define linkonce_odr void @__zluda_ptx_impl_sustobj_p_2d_v2_b32(i64, <2 x i32>, <2 x i32>)
define linkonce_odr void @__zluda_ptx_impl_sustref_b_2d_b32(ptr addrspace(1) readonly, <2 x i32>, i32)
define linkonce_odr void @__zluda_ptx_impl_sustref_p_2d_v4_b32(ptr addrspace(1) readonly, <2 x i32>, <4 x i32>)
```

## N7 ✅ 新 `.bc` 能被 ZLUDA 自己的 LLVM 21 解析（重建最大的风险，已排除）

这是我最担心的一条：`.bc` 是**设备端 bitcode**，由**构建它的人**选定的 clang 产出，却由**消费它的人**（ZLUDA 自己链接的 LLVM）读入。ROCm 7.1 的 clang 是：

```
clang version 21.0.0git (git@github.com:Compute-Mirrors/llvm-project 5dcc622b51ecd499912c1062ce2b0ecda60d8e93)
```

而 ZLUDA 侧 `llvm_zluda/Cargo.toml` 是 `llvm-sys = "211"`（LLVM 21），`ext/llvm-project` pin 在 `bee10d88d`（`zluda-rdna4-wmma`）。两边都是 **21.x**，方向正确。（留意：ROCm 7.1 是 Compute-Mirrors 的 fork，不是上游 LLVM；这次版本号恰好对上了，但**「谁产的 `.bc`」这件事仍然没有写进仓库**，见下。）

链路是活的（不是推测）：`ptx/src/test/spirv_run/mod.rs:1592` 的 `run_hip` 调 `llvm_zluda::compile(..., ptx_impl=module.linked_bitcode(), ...)`，而 `linked_bitcode()` 返回的就是 `include_bytes!` 进来的这两个 `.bc`，`compile.rs:359` 再 `load_module()` → `LLVMParseBitcodeInContext2`。证据：

- **584 个测试全绿 / 0 失败**（改动前是 390 通过 / 193 失败）。其中 `div_ftz`、`div_noftz`、`call_rnd`、`rcp_f64` 的 golden IR 带 `strictfp`，走的是 **constrained** 那份 `.bc`；其余大片走默认那份 → **两个 `.bc` 都被真实解析、链接、编译成 ELF 并在 GPU 上跑过**。
- 我另外单独跑了 4 个 GPU 测试（顺手拿到 N10 的关键证据）：
  ```
  cargo test -p zluda --lib -- impl::tex::tests --skip _nvidia
  test r#impl::tex::tests::texref_formats_channels_dimensions_zluda ... ok
  test r#impl::tex::tests::texref_s32coord_formats_channels_dimensions_zluda ... ok
  test r#impl::tex::tests::texobj_formats_channels_dimensions_zluda ... ok
  test r#impl::tex::tests::texobj_s32coord_formats_channels_dimensions_zluda ... ok
  test result: ok. 4 passed; 0 failed
  ```
  （这些经 `texref_*`/`texobj_*` 走完整 `ptx_impl` 链路并按格式/通道/维度逐值校验。）

所以：**「重建的 `.bc` 有 LLVM 版本不兼容」这个风险，到此为止可以划掉。** 唯一遗留的卫生问题：构建命令仍是「文件头注释里的手工命令」，谁在什么工具链下产的 `.bc` 无法从仓库追溯 —— 这与 C1（两个 `.bc` 曾由不同 LLVM 提交构建）是同一类问题，建议把 clang 版本/命令记进 `ptx/lib/README` 或一个 `build_ptx_impl.ps1`。

## N8 ✅ 白名单的 24 个名字 = 解析器能产生的**全部**名字（完整且精确）

上一轮我只说「白名单会漏」，这次算清楚了。`ptx_parser/src/lib.rs:4639-4668` 的 `sust` 文法里，维度是**写死的 `.2d`**，另外只有两条产生式：

```
sust.p.2d{.vec}.b32{.sustclamp}      [a, b], c     // .vec ∈ {.v2,.v4}          → 3 种
sust.b.2d{.vec}.stype{.sustclamp}    [a, b], c     // .stype ∈ {.b8,.b16,.b32}  → 9 种
```

即 **12 种可解析形态**（`.1d`/`.3d`、`.v4.b64`、`.p.f32` 全部在**解析阶段**就被拒，不会走到下沉），每种再按首个操作数是 64 位寄存器（`Texobj`）还是 `.surfref` 符号（`Texref`）分成两个名字 → **恰好 24 个**。这就是你白名单的大小，`.cpp` 里也恰好定义 24 个。**没有漏，也没有多余**；`IMPLEMENTED.contains()` 那条兜底从此对 `sust` 是死代码（保留无妨）。

## N9 ✅ 参数类型约定与既有「已在 GPU 上验证过」的函数一致

我一度担心 `.b8` 在 ZLUDA 里被当成 16 位寄存器（PTX 里 `.b8` 确实住在 16 位寄存器），那样 `<2 x i8>` 的实参就会对不上 `.bc` 的形参、链接失败、kernel 变「missing」。**不成立**，有两处硬证据：

- `ptx/src/test/ll/vector_extract.ll`：`.v4 .b8` 就是 `<4 x i8>` + `i8` 元素（`load <4 x i8>` / `insertelement ... i8`）。
- `ptx/src/test/ll/div_ftz.ll:50`：`call float @__zluda_ptx_impl_div_f32_part2(..., i8 %"51")`，而 `.bc` 里该函数形参是 `i8 noundef zeroext` —— **调用点写的是裸 `i8`、定义侧带 `zeroext`，且这些测试在 GPU 上通过**。新增的 24 个函数走的正是同一条路（裸 `i8`/`i16`/`<2 x i8>` 调用点 vs 定义侧 `zeroext`）。

即：`sust.b.2d.b8` 这类形式**不会**因为参数类型不匹配而在加载时失败。

## N10 🟠 新增的 12 个 `sustref_*` 读的是**错的那个字段**，而且整条 `.surfref` 运行时路径在 ZLUDA 里根本不存在

这一条是新增代码里唯一的实质问题，分三个**互相独立**的事实，缺一不可：

**事实 A（IR）**：同一个 `.surfref`/`.texref` 变量，两条路径读的偏移不一样。

```llvm
; texref_2d_v4_f32_f32 —— 对象在偏移 72
%3 = getelementptr inbounds nuw i8, ptr addrspace(1) %0, i64 72
%4 = load ptr, ptr addrspace(1) %3, align 8          ; = textureReference.textureObject
%7 = getelementptr inbounds nuw i8, ptr addrspace(1) %6, i64 48   ; 采样器

; sustref_b_2d_b32 —— 新代码读偏移 0
%4 = load ptr, ptr addrspace(1) %0, align 8          ; = surfaceReference.surfaceObject
```

偏移 72 不是随手写的：ZLUDA 把 `.texref` 变量生成成 `struct.texture { struct.textureReference }`（`emit.rs:4257-4311`，字段表按 ROCm 布局硬编码），按该字段表算 `normalized(0) readMode(4) filterMode(8) addressMode[3](12..24) channelDesc(24..44) sRGB(44) maxAnisotropy(48) mipmapFilterMode(52) bias(56) minClamp(60) maxClamp(64) textureObject(72, 8 字节对齐) numChannels(80) format(84)` —— **72**，与 ROCm 头 `hip/texture_types.h` 的 C 布局一致。新代码读的偏移 0 是 `{int normalized; int readMode;}` 这 8 个字节。

**事实 B（实测）**：偏移 72 那条路是**活的**，而且 HIP 真的往那里写了东西 —— 就是上面 N7 里那 4 个 GPU 测试（`texref_*` 按 1D/2D/3D × 格式 × 通道数逐值比对，全部通过）。

**事实 C（源码）**：ZLUDA **没有任何路径**会往 `.surfref` 变量里写值。

- `cuModuleGetSurfRef` 是被实现的，但实现是 `hipModuleGetTexRef`（`zluda/src/impl/module.rs:636-642`），返回的 `CUsurfref` 在 `zluda/src/lib.rs:311` 就等价于 `*mut textureReference`；
- `cuSurfRefSetArray` / `cuSurfRefGetArray` **不在** `zluda/src/lib.rs` 的 `implemented <= [...]` / `implemented_in_function <= [...]` 名单里，所以宏会把它们生成成 `unimplemented!` 桩（`lib.rs:19-31`：`Err(r#impl::unimplemented())`）。它们**出现在** `nvcuda.dll` 的导出表里（我 `llvm-readobj --coff-exports` 数过 675 个导出，其中 `cuSurfRefSetArray`、`cuSurfRefGetArray` 都在），但**调用会直接返回错误**，服务端 `zluda64_server/src/main.rs` 里也没有对应的 `Opcode::` 分支。

**后果**：`.surfref` 变量在 LLVM 里是 `undef` 初值（`emit.rs:283-288`），链接后基本就是零，而且**没有任何东西会写它**。设备端于是把这个 8 字节（0）当成 surface 对象地址，交给 `store_2D` → `__ockl_image_store_2D((const unsigned int addrspace(4)*)0, ...)`。**改动前**这 12 个名字不在白名单，翻译阶段直接报错（响亮、可诊断）；**改动后**翻译通过，问题挪到运行期、表现为「往空描述符写」（大概率是丢弃或 GPU fault）。也就是说：这一半的白名单扩大，把一次**响亮的失败**换成了一次**安静的失败**，而代价是 12 个不可能生效的函数体。

**两种自洽的改法（选一个，别停在中间）**：

1. **让它保持响亮**（我倾向这个，改动一行）：把 12 个 `sustref_*` 从 `IMPLEMENTED` 里拿掉，只留 12 个 `sustobj_*`。白名单的含义应该是「这个名字真的能用」，而现在它不能。等运行时路径补上再加回来，那时测试也才有意义。
2. **让它真的能用**：在服务端实现 `cuSurfRefSetArray`，**不要**复用 `hipTexRefSetArray` —— 那会把对象写进偏移 72，而且 texture 对象的描述符字节 14 是**只读权限**（`0xb0`，见你 `surf.rs` 里 `describes_same_image` 的注释），硬件会拒绝 store。正确做法是走 `cuSurfObjectCreate` 那条路（创建真正的 surface object），把它的句柄写到 `.surfref` 变量的**偏移 0** —— 这恰好就是新设备代码假设的 `surfaceReference{ surfaceObject }` 布局（ROCm 头 `hip/surface_types.h:46`）。此时 `sustref_*` 读偏移 0 是**对的**，而 `.surfref` 变量不再被当 `textureReference` 用；只是「经 `.surfref` 变量做 `tex` 读取」这条路会读到偏移 72 的空值 —— 今天本来也不通，但值得写进注释，免得下一个人以为是新引入的。

## N11 🟡 `817fe5a` 让 182 个 `_cuda` 测试**静默通过**（占套件 31%）

CRLF 归一化那条改得对（那 11 个失败本来就是行尾差异，内容逐字节相同）。但 CUDA 侧的处理：

```rust
let cuda = match &*CUDA { Ok(cuda) => cuda, Err(_) => return Ok(()) };
```

在没有 `nvcuda.dll` 的机器上，**182 个 `_cuda` 测试直接返回 `Ok(())`**，名字还叫 `..._cuda`。后果是「绿色套件」不再等于「CUDA 对照路径跑过了」——而 CUDA 对照恰恰是这类翻译层唯一的外部参照物。建议二选一（都很小）：跳过时打**一次** stderr 提示（像 `surf.rs` 里那些「could not ...」那样，说明发生了什么、为什么），以及加一个 `ZLUDA_REQUIRE_CUDA=1` 让 CI 上把跳过变成失败。

## N12 🟡 新增测试叫 end-to-end，实际只到翻译层；整个 `sust` 家族在 GPU 上零覆盖

`sust_all_combinations` 的价值是真的（它覆盖了 24 个名字的**名字选择**与操作数解析，正好是上一轮 F1 的类型），但 `compile_and_assert` 只调 `pass::to_llvm_module`，**不链接 `.bc`、不 codegen、不上 GPU**。而 `sust` 这条指令——**包括改动前就存在的 5 个 `sustobj_*`**——在这之前之后都没有任何 GPU 测试。

现在有现成的模板可抄：`zluda/src/impl/tex.rs` 里的 `texref_read_test` / `texref_s32coord_test`（`CudaApi` + `#[test_cuda]`，创建 array → 绑定 → 起 kernel → 读回逐值比对），而且我刚证明它在你机器上**确实能跑**（N7 那 4 个 `_zluda` 测试）。照它写一个 `surfobj_write_test`（`cuSurfObjectCreate` → 起一个 `sust.b.2d.b32` 的 kernel → `cuMemcpyDtoH` 读回比字节），能一次性把描述符、坐标单位（`.b` 按字节、`.p` 按元素）、`byte_x_to_sample_x`、以及 N10 的偏移问题全部钉死在实机上。这是把「24 个名字都实现了」变成「24 个名字都能用」的唯一办法。

## 这一轮我实际执行了什么（可复现 / 可复核）

| 动作 | 结果 |
|---|---|
| `git log` / `git status` / `git rev-parse` 对比 origin | HEAD `4d1dc62` 与 `origin/fix/dlssnr-surface-sampler-and-wgp` **已同步**，工作树只有未跟踪的 `BUG_REVIEW_ZLUDA_FORK.md` |
| 用 ROCm 7.1 clang 重编译 `.cpp` + 逐符号比对提交的 `.bc` | 152 / 152 符号，**0 差异**（N6） |
| `llvm-dis` 反汇编两个 `.bc` | 都能解析，拿到真实签名与函数体（N6/N10） |
| `cargo test -p ptx --lib` | **584 passed / 0 failed**（此前 390/193） |
| `cargo test -p zluda --lib -- impl::tex::tests --skip _nvidia` | **4 passed**（真实 GPU，N7/N10 的关键证据） |
| `cargo check -p zluda -p zluda32 -p zluda64_server -p zluda_cache -p llvm_zluda -p nvapi` | `Finished`，退出码 0，无新警告 |
| 读 `ptx_parser` 的 `sust` 文法 | 12 形态 × 2 = 24，与白名单完全一致（N8） |
| 读 golden `.ll`（`vector_extract` / `div_ftz`） | `.b8`→`i8`、`.v4.b8`→`<4 x i8>`，与 `.bc` 形参一致（N9） |

**没有做**：N10 的实机验证（`sustref` 到底会不会读到 0）——这需要一个能绑定 `.surfref` 的路径，而那条路径正是缺失的东西；所以它是**源码级定论 + 逻辑推论**，不是实测。想实测只有一条路：先实现 `cuSurfRefSetArray`。另外我**没有改动任何仓库文件**（唯一的临时脚本放在 `%TEMP%\zlu\`），`ptx/src/pass/replace_instructions_with_functions.rs` 的注释现在与事实一致（「All valid 2D combinations parsed by ptx_parser are implemented in ptx/lib/zluda_ptx_impl.cpp」——已按 N8 核实为真）。
