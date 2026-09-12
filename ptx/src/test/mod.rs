use crate::pass::{self, TranslateError};
use ptx_parser as ast;

mod spirv_run;

#[cfg(not(feature = "ci_build"))]
#[macro_export]
macro_rules! read_test_file {
    ($file:expr) => {
        {
            use std::path::PathBuf;
            // CARGO_MANIFEST_DIR is the crate directory (ptx), but file! is relative to the workspace root (and therefore also includes ptx).
            let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            path.pop();
            path.push(file!());
            path.pop();
            path.push($file);
            std::fs::read_to_string(path).unwrap()
        }
    };
}

#[cfg(feature = "ci_build")]
#[macro_export]
macro_rules! read_test_file {
    ($file:expr) => {
        include_str!($file).to_string()
    };
}
pub(crate) use read_test_file;

fn parse_and_assert(ptx_text: &str) {
    ast::parse_module_checked(ptx_text).unwrap();
}

fn compile_and_assert(ptx_text: &str) -> Result<(), TranslateError> {
    let ast = ast::parse_module_checked(ptx_text).unwrap();
    let attributes = pass::Attributes {
        clock_rate: 2124000,
        cumode: true,
    };
    crate::to_llvm_module(ast, attributes, |_| {})?;
    Ok(())
}

#[test]
fn empty() {
    parse_and_assert(".version 6.5 .target sm_30, debug");
}

#[test]
fn operands_ptx() {
    let vector_add = include_str!("operands.ptx");
    parse_and_assert(vector_add);
}

#[test]
#[allow(non_snake_case)]
fn vectorAdd_kernel64_ptx() -> Result<(), TranslateError> {
    let vector_add = include_str!("vectorAdd_kernel64.ptx");
    compile_and_assert(vector_add)
}

#[test]
#[allow(non_snake_case)]
fn _Z9vectorAddPKfS0_Pfi_ptx() -> Result<(), TranslateError> {
    let vector_add = include_str!("_Z9vectorAddPKfS0_Pfi.ptx");
    compile_and_assert(vector_add)
}

#[test]
#[allow(non_snake_case)]
fn vectorAdd_11_ptx() -> Result<(), TranslateError> {
    let vector_add = include_str!("vectorAdd_11.ptx");
    compile_and_assert(vector_add)
}

#[test]
fn sust_all_combinations() -> Result<(), TranslateError> {
    let ptx_code = r#"
.version 7.0
.target sm_70
.address_size 64

.global .surfref surf_var;

.visible .entry test_sust_all(
    .param .u64 surf_obj,
    .param .u32 x,
    .param .u32 y,
    .param .b32 val_b32,
    .param .b16 val_b16,
    .param .b8  val_b8,
    .param .v2 .b32 val_v2_32,
    .param .v4 .b32 val_v4_32,
    .param .v2 .b16 val_v2_16,
    .param .v4 .b16 val_v4_16,
    .param .v2 .b8  val_v2_8,
    .param .v4 .b8  val_v4_8
) {
    .reg .b64 %sobj;
    .reg .b32 %x;
    .reg .b32 %y;
    .reg .b32 %v32;
    .reg .b16 %v16;
    .reg .b8  %v8;
    .reg .v2 .b32 %v2_32;
    .reg .v4 .b32 %v4_32;
    .reg .v2 .b16 %v2_16;
    .reg .v4 .b16 %v4_16;
    .reg .v2 .b8  %v2_8;
    .reg .v4 .b8  %v4_8;

    ld.param.u64 %sobj, [surf_obj];
    ld.param.u32 %x, [x];
    ld.param.u32 %y, [y];
    ld.param.b32 %v32, [val_b32];
    ld.param.b16 %v16, [val_b16];
    ld.param.b8  %v8, [val_b8];

    ld.param.v2.b32 %v2_32, [val_v2_32];
    ld.param.v4.b32 %v4_32, [val_v4_32];
    ld.param.v2.b16 %v2_16, [val_v2_16];
    ld.param.v4.b16 %v4_16, [val_v4_16];
    ld.param.v2.b8  %v2_8, [val_v2_8];
    ld.param.v4.b8  %v4_8, [val_v4_8];

    // sustobj - formatted (.p)
    sust.p.2d.b32 [%sobj, {%x, %y}], %v32;
    sust.p.2d.v2.b32 [%sobj, {%x, %y}], %v2_32;
    sust.p.2d.v4.b32 [%sobj, {%x, %y}], %v4_32;

    // sustobj - raw (.b)
    sust.b.2d.b8 [%sobj, {%x, %y}], %v8;
    sust.b.2d.b16 [%sobj, {%x, %y}], %v16;
    sust.b.2d.b32 [%sobj, {%x, %y}], %v32;
    sust.b.2d.v2.b8 [%sobj, {%x, %y}], %v2_8;
    sust.b.2d.v2.b16 [%sobj, {%x, %y}], %v2_16;
    sust.b.2d.v2.b32 [%sobj, {%x, %y}], %v2_32;
    sust.b.2d.v4.b8 [%sobj, {%x, %y}], %v4_8;
    sust.b.2d.v4.b16 [%sobj, {%x, %y}], %v4_16;
    sust.b.2d.v4.b32 [%sobj, {%x, %y}], %v4_32;

    ret;
}
"#;
    compile_and_assert(ptx_code)
}

#[test]
fn sustref_rejected_at_compile_time() {
    let ptx_code = r#"
.version 7.0
.target sm_70
.address_size 64

.global .surfref surf_var;

.visible .entry test_sustref(
    .param .u32 x,
    .param .u32 y,
    .param .b32 val_b32
) {
    .reg .b32 %x;
    .reg .b32 %y;
    .reg .b32 %v32;

    ld.param.u32 %x, [x];
    ld.param.u32 %y, [y];
    ld.param.b32 %v32, [val_b32];

    sust.b.2d.b32 [surf_var, {%x, %y}], %v32;
    ret;
}
"#;
    let res = std::panic::catch_unwind(|| {
        compile_and_assert(ptx_code)
    });
    match res {
        Ok(Err(_)) | Err(_) => {} // Rejected at compile time (panics in debug, Err in release)
        Ok(Ok(_)) => panic!("sustref should have been rejected at compile time!"),
    }
}

