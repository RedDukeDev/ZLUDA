#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wunused-parameter"
#include <llvm-c/Core.h>
#include <llvm/IR/IRBuilder.h>
#include <llvm/IR/Type.h>
#include <llvm/IR/Instructions.h>
#include <lld/Common/Driver.h>
#include <lld/Common/CommonLinkerContext.h>
#include <llvm/Support/raw_ostream.h>
#include "llvm/Support/CommandLine.h"
#include "llvm/Support/raw_ostream.h"
#include <llvm/IR/Module.h>
#include <llvm/Bitcode/BitcodeWriter.h>
#include <llvm/Support/FileSystem.h>
#include <llvm/Transforms/Utils/SplitModule.h>
#include <llvm/IR/Operator.h>
#include <llvm/IR/InstIterator.h>
#include <llvm/ADT/SmallVector.h>
#pragma GCC diagnostic pop

#include <mutex>
#include <string>

// Declare that we want to use the ELF driver
LLD_HAS_DRIVER(elf)

using namespace llvm;

typedef enum
{
    LLVMZludaAtomicRMWBinOpXchg,     /**< Set the new value and return the one old */
    LLVMZludaAtomicRMWBinOpAdd,      /**< Add a value and return the old one */
    LLVMZludaAtomicRMWBinOpSub,      /**< Subtract a value and return the old one */
    LLVMZludaAtomicRMWBinOpAnd,      /**< And a value and return the old one */
    LLVMZludaAtomicRMWBinOpNand,     /**< Not-And a value and return the old one */
    LLVMZludaAtomicRMWBinOpOr,       /**< OR a value and return the old one */
    LLVMZludaAtomicRMWBinOpXor,      /**< Xor a value and return the old one */
    LLVMZludaAtomicRMWBinOpMax,      /**< Sets the value if it's greater than the
                                  original using a signed comparison and return
                                  the old one */
    LLVMZludaAtomicRMWBinOpMin,      /**< Sets the value if it's Smaller than the
                                  original using a signed comparison and return
                                  the old one */
    LLVMZludaAtomicRMWBinOpUMax,     /**< Sets the value if it's greater than the
                                 original using an unsigned comparison and return
                                 the old one */
    LLVMZludaAtomicRMWBinOpUMin,     /**< Sets the value if it's greater than the
                                  original using an unsigned comparison and return
                                  the old one */
    LLVMZludaAtomicRMWBinOpFAdd,     /**< Add a floating point value and return the
                                  old one */
    LLVMZludaAtomicRMWBinOpFSub,     /**< Subtract a floating point value and return the
                                old one */
    LLVMZludaAtomicRMWBinOpFMax,     /**< Sets the value if it's greater than the
                                 original using an floating point comparison and
                                 return the old one */
    LLVMZludaAtomicRMWBinOpFMin,     /**< Sets the value if it's smaller than the
                                 original using an floating point comparison and
                                 return the old one */
    LLVMZludaAtomicRMWBinOpUIncWrap, /**< Increments the value, wrapping back to zero
                                 when incremented above input value */
    LLVMZludaAtomicRMWBinOpUDecWrap, /**< Decrements the value, wrapping back to
                                 the input value when decremented below zero */
} LLVMZludaAtomicRMWBinOp;

static llvm::AtomicRMWInst::BinOp mapFromLLVMRMWBinOp(LLVMZludaAtomicRMWBinOp BinOp)
{
    switch (BinOp)
    {
    case LLVMZludaAtomicRMWBinOpXchg:
        return llvm::AtomicRMWInst::Xchg;
    case LLVMZludaAtomicRMWBinOpAdd:
        return llvm::AtomicRMWInst::Add;
    case LLVMZludaAtomicRMWBinOpSub:
        return llvm::AtomicRMWInst::Sub;
    case LLVMZludaAtomicRMWBinOpAnd:
        return llvm::AtomicRMWInst::And;
    case LLVMZludaAtomicRMWBinOpNand:
        return llvm::AtomicRMWInst::Nand;
    case LLVMZludaAtomicRMWBinOpOr:
        return llvm::AtomicRMWInst::Or;
    case LLVMZludaAtomicRMWBinOpXor:
        return llvm::AtomicRMWInst::Xor;
    case LLVMZludaAtomicRMWBinOpMax:
        return llvm::AtomicRMWInst::Max;
    case LLVMZludaAtomicRMWBinOpMin:
        return llvm::AtomicRMWInst::Min;
    case LLVMZludaAtomicRMWBinOpUMax:
        return llvm::AtomicRMWInst::UMax;
    case LLVMZludaAtomicRMWBinOpUMin:
        return llvm::AtomicRMWInst::UMin;
    case LLVMZludaAtomicRMWBinOpFAdd:
        return llvm::AtomicRMWInst::FAdd;
    case LLVMZludaAtomicRMWBinOpFSub:
        return llvm::AtomicRMWInst::FSub;
    case LLVMZludaAtomicRMWBinOpFMax:
        return llvm::AtomicRMWInst::FMax;
    case LLVMZludaAtomicRMWBinOpFMin:
        return llvm::AtomicRMWInst::FMin;
    case LLVMZludaAtomicRMWBinOpUIncWrap:
        return llvm::AtomicRMWInst::UIncWrap;
    case LLVMZludaAtomicRMWBinOpUDecWrap:
        return llvm::AtomicRMWInst::UDecWrap;
    }

    llvm_unreachable("Invalid LLVMZludaAtomicRMWBinOp value!");
}

static AtomicOrdering mapFromLLVMOrdering(LLVMAtomicOrdering Ordering)
{
    switch (Ordering)
    {
    case LLVMAtomicOrderingNotAtomic:
        return AtomicOrdering::NotAtomic;
    case LLVMAtomicOrderingUnordered:
        return AtomicOrdering::Unordered;
    case LLVMAtomicOrderingMonotonic:
        return AtomicOrdering::Monotonic;
    case LLVMAtomicOrderingAcquire:
        return AtomicOrdering::Acquire;
    case LLVMAtomicOrderingRelease:
        return AtomicOrdering::Release;
    case LLVMAtomicOrderingAcquireRelease:
        return AtomicOrdering::AcquireRelease;
    case LLVMAtomicOrderingSequentiallyConsistent:
        return AtomicOrdering::SequentiallyConsistent;
    }

    llvm_unreachable("Invalid LLVMAtomicOrdering value!");
}

typedef unsigned LLVMFastMathFlags;

static FastMathFlags mapFromLLVMFastMathFlags(LLVMFastMathFlags FMF)
{
    FastMathFlags NewFMF;
    NewFMF.setAllowReassoc((FMF & LLVMFastMathAllowReassoc) != 0);
    NewFMF.setNoNaNs((FMF & LLVMFastMathNoNaNs) != 0);
    NewFMF.setNoInfs((FMF & LLVMFastMathNoInfs) != 0);
    NewFMF.setNoSignedZeros((FMF & LLVMFastMathNoSignedZeros) != 0);
    NewFMF.setAllowReciprocal((FMF & LLVMFastMathAllowReciprocal) != 0);
    NewFMF.setAllowContract((FMF & LLVMFastMathAllowContract) != 0);
    NewFMF.setApproxFunc((FMF & LLVMFastMathApproxFunc) != 0);

    return NewFMF;
}

LLVM_C_EXTERN_C_BEGIN

LLVMValueRef LLVMZludaBuildAlloca(LLVMBuilderRef B, LLVMTypeRef Ty, unsigned AddrSpace,
                                  const char *Name)
{
    return llvm::wrap(llvm::unwrap(B)->CreateAlloca(llvm::unwrap(Ty), AddrSpace, nullptr, Name));
}

LLVMValueRef LLVMZludaBuildAtomicRMW(LLVMBuilderRef B, LLVMZludaAtomicRMWBinOp op,
                                     LLVMValueRef PTR, LLVMValueRef Val,
                                     char *scope,
                                     LLVMAtomicOrdering ordering)
{
    auto builder = llvm::unwrap(B);
    LLVMContext &context = builder->getContext();
    llvm::AtomicRMWInst::BinOp intop = mapFromLLVMRMWBinOp(op);
    return llvm::wrap(builder->CreateAtomicRMW(
        intop, llvm::unwrap(PTR), llvm::unwrap(Val), llvm::MaybeAlign(),
        mapFromLLVMOrdering(ordering),
        context.getOrInsertSyncScopeID(scope)));
}

LLVMValueRef LLVMZludaBuildAtomicCmpXchg(LLVMBuilderRef B, LLVMValueRef Ptr,
                                         LLVMValueRef Cmp, LLVMValueRef New,
                                         char *scope,
                                         LLVMAtomicOrdering SuccessOrdering,
                                         LLVMAtomicOrdering FailureOrdering)
{
    auto builder = llvm::unwrap(B);
    LLVMContext &context = builder->getContext();
    return wrap(builder->CreateAtomicCmpXchg(
        unwrap(Ptr), unwrap(Cmp), unwrap(New), MaybeAlign(),
        mapFromLLVMOrdering(SuccessOrdering),
        mapFromLLVMOrdering(FailureOrdering),
        context.getOrInsertSyncScopeID(scope)));
}

void LLVMZludaSetFastMathFlags(LLVMValueRef FPMathInst, LLVMFastMathFlags FMF)
{
    Value *P = unwrap<Value>(FPMathInst);
    cast<Instruction>(P)->setFastMathFlags(mapFromLLVMFastMathFlags(FMF));
}

void LLVMZludaBuildFence(LLVMBuilderRef B, LLVMAtomicOrdering Ordering,
                         char *scope, const char *Name)
{
    auto builder = llvm::unwrap(B);
    LLVMContext &context = builder->getContext();
    builder->CreateFence(mapFromLLVMOrdering(Ordering),
                         context.getOrInsertSyncScopeID(scope),
                         Name);
}

void LLVMZludaSetAtomic(
    LLVMValueRef AtomicInst,
    LLVMAtomicOrdering Ordering,
    char *SSID)
{
    auto inst = unwrap(AtomicInst);
    if (LoadInst *LI = dyn_cast<LoadInst>(inst))
    {
        LI->setAtomic(mapFromLLVMOrdering(Ordering), LI->getContext().getOrInsertSyncScopeID(SSID));
    }
    else if (StoreInst *SI = dyn_cast<StoreInst>(inst))
    {
        SI->setAtomic(mapFromLLVMOrdering(Ordering), SI->getContext().getOrInsertSyncScopeID(SSID));
    }
    else
    {
        llvm_unreachable("Invalid instruction type for LLVMZludaSetAtomic");
    }
}

// Cuts a module into parts for code generation, writing each as bitcode to
// "<path_prefix><n>.bc" and returning how many were written.
//
// Code generation is where a translation spends its time: measured on the
// largest module of the DLSS network, 1444 seconds out of 1753, all on one
// thread. The parts can be generated on as many threads as the machine has,
// and are linked back together afterwards -- LLVMZludaLinkWithLLD below
// already takes a list of objects.
//
// Bitcode on disk rather than modules in memory because an LLVMContext belongs
// to the thread that made it: each worker parses its own part into a context of
// its own, and files also make a part easy to look at when a part is what
// misbehaves.
//
// Locals are preserved, which keeps every user of an internal symbol inside one
// part. Shared-memory globals are the reason: the AMDGPU backend allocates
// those per module, so a kernel must not be separated from the ones it shares
// them with.
extern "C" uint32_t LLVMZludaSplitModule(LLVMModuleRef M, uint32_t parts,
                                         const char *path_prefix, char **ErrorMessage)
{
    uint32_t written = 0;
    std::string prefix(path_prefix);
    std::string failure;

    llvm::SplitModule(
        *llvm::unwrap(M), parts,
        [&](std::unique_ptr<llvm::Module> part)
        {
            if (!failure.empty())
            {
                return;
            }
            std::error_code ec;
            std::string path = prefix + std::to_string(written) + ".bc";
            llvm::raw_fd_ostream out(path, ec, llvm::sys::fs::OF_None);
            if (ec)
            {
                failure = ec.message();
                return;
            }
            llvm::WriteBitcodeToFile(*part, out);
            out.flush();
            if (out.has_error())
            {
                failure = "could not write " + path;
                return;
            }
            ++written;
        },
        /*PreserveLocals=*/true);

    if (!failure.empty())
    {
        if (ErrorMessage != nullptr)
        {
            *ErrorMessage = strdup(failure.c_str());
        }
        return 0;
    }
    return written;
}

std::mutex lld_mutex;

int LLVMZludaLinkWithLLD(uint32_t inputs, const char **input_paths, const char *output_path, char **ErrorMessage)
{
    std::vector<const char *> args;
    args.push_back("ld.lld");
    args.push_back("-shared");
    for (uint32_t i = 0; i < inputs; ++i)
    {
        args.push_back(input_paths[i]);
    }
    args.push_back("-o");
    args.push_back(output_path);
    args.push_back("--threads=1");
    args.push_back("--no-undefined");

    std::string log_out_str;
    std::string log_err_str;
    llvm::raw_string_ostream log_out(log_out_str);
    llvm::raw_string_ostream log_err(log_err_str);

    // lldMain does not seem to be thread-safe, so we wrap it in a mutex.
    std::lock_guard<std::mutex> guard(lld_mutex);
    lld::Result result = lld::lldMain(args, log_out, log_err,
                                      {{lld::Gnu, &lld::elf::link}});

    if (result.retCode != 0)
    {
        if (ErrorMessage)
        {
            if (!log_err_str.empty())
            {
                *ErrorMessage = strdup(log_err_str.c_str());
            }
            else
            {
                *ErrorMessage = strdup("LLD linking failed with unknown error");
            }
        }
        return result.retCode;
    }

    return 0;
}

bool LLVMZludaParseCommandLineOptions(int argc, const char *const *argv,
                                      char **ErrorMessage)
{
    std::string log_out_str;
    llvm::raw_string_ostream log_err_str(log_out_str);
    bool result = llvm::cl::ParseCommandLineOptions(argc, argv, StringRef(""), &log_err_str);
    if (!result && ErrorMessage)
    {
        *ErrorMessage = strdup(log_out_str.c_str());
    }
    return result;
}

LLVM_C_EXTERN_C_END

// PTX that takes a kernel parameter's address (mov.b64 %rd, param; ld.param
// [%rd+N]) reaches LLVM as inttoptr(ptrtoint(addrspacecast(kernarg)) + N):
// a flat pointer whose origin the integer round trip hides, so the loads are
// flat loads that the backend treats as divergent (texture handles read that
// way force a readfirstlane waterfall around every image instruction). This
// rewrites inttoptr(ptrtoint(P) + offsets) into a GEP on P, the same address
// with P's provenance, so infer-address-spaces can turn such loads back into
// constant-address-space loads. Returns the number of rewrites.
namespace
{
static Value *provenanceBase(Value *V, SmallVectorImpl<Value *> &Offsets, unsigned Depth)
{
    if (Depth > 8)
        return nullptr;
    if (auto *Cast = dyn_cast<PtrToIntInst>(V))
        return Cast->getType()->isIntegerTy(64) ? Cast->getPointerOperand() : nullptr;
    auto *Binary = dyn_cast<BinaryOperator>(V);
    if (Binary == nullptr)
        return nullptr;
    const bool IsAdd = Binary->getOpcode() == Instruction::Add;
    const bool IsDisjointOr = Binary->getOpcode() == Instruction::Or && cast<PossiblyDisjointInst>(Binary)->isDisjoint();
    if (!IsAdd && !IsDisjointOr)
        return nullptr;
    for (unsigned Side = 0; Side < 2; ++Side)
    {
        SmallVector<Value *, 4> Nested;
        if (Value *Base = provenanceBase(Binary->getOperand(Side), Nested, Depth + 1))
        {
            Offsets.append(Nested.begin(), Nested.end());
            Offsets.push_back(Binary->getOperand(1 - Side));
            return Base;
        }
    }
    return nullptr;
}
} // namespace

extern "C" unsigned LLVMZludaRecoverPointerProvenance(LLVMModuleRef ModuleRef)
{
    Module *M = unwrap(ModuleRef);
    unsigned Rewritten = 0;
    for (Function &F : *M)
    {
        SmallVector<IntToPtrInst *, 32> Casts;
        for (Instruction &I : instructions(F))
            if (auto *Cast = dyn_cast<IntToPtrInst>(&I))
                Casts.push_back(Cast);
        for (IntToPtrInst *Cast : Casts)
        {
            SmallVector<Value *, 4> Offsets;
            Value *Base = provenanceBase(Cast->getOperand(0), Offsets, 0);
            if (Base == nullptr || !Base->getType()->isPointerTy())
                continue;
            IRBuilder<> Builder(Cast);
            Value *Offset = nullptr;
            for (Value *Part : Offsets)
            {
                Value *Wide = Builder.CreateSExtOrTrunc(Part, Builder.getInt64Ty());
                Offset = Offset == nullptr ? Wide : Builder.CreateAdd(Offset, Wide);
            }
            Value *Pointer = Offset == nullptr ? Base : Builder.CreateGEP(Builder.getInt8Ty(), Base, Offset);
            if (Pointer->getType() != Cast->getType())
                Pointer = Builder.CreateAddrSpaceCast(Pointer, Cast->getType());
            Cast->replaceAllUsesWith(Pointer);
            Cast->eraseFromParent();
            ++Rewritten;
        }
    }
    return Rewritten;
}

// Device-library image wrappers (__llvm_amdgcn_image_*, called by ZLUDA's texture/surface
// helpers) were left as calls: their FP attributes did not match the strictfp kernels, so the
// inliner refused them. Each call then began with a full s_waitcnt and a readfirstlane
// waterfall over the (actually uniform) resource descriptor. The wrappers contain no FP math,
// so give them the caller-compatible "dynamic" denormal mode and strictfp, and force inlining.
extern "C" unsigned LLVMZludaInlineImageHelpers(LLVMModuleRef ModuleRef)
{
    Module* M = unwrap(ModuleRef);
    unsigned Changed = 0;
    for (Function& F : *M)
    {
        const StringRef N = F.getName();
        if (F.isDeclaration() || !N.starts_with("__llvm_amdgcn_image_"))
            continue;
        F.removeFnAttr(Attribute::NoInline);
        F.removeFnAttr(Attribute::OptimizeNone);
        F.addFnAttr(Attribute::AlwaysInline);
        F.addFnAttr("denormal-fp-math", "dynamic,dynamic");
        F.addFnAttr("denormal-fp-math-f32", "dynamic,dynamic");
        F.addFnAttr(Attribute::StrictFP); // no FP math inside
        for (User* U : F.users())
            if (auto* CB = dyn_cast<CallBase>(U))
                CB->removeFnAttr(Attribute::NoInline);
        ++Changed;
    }
    return Changed;
}
