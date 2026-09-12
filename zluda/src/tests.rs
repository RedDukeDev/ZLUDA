pub(crate) struct Zluda;

pub(crate) struct Cuda(libloading::Library);

/// Held by every test that retains the primary context.
///
/// The primary context is one object per device for the whole process, and its
/// `active` flag is nothing but "somebody holds a reference". Two tests that retain
/// or release it at the same time therefore see each other's changes and can no
/// longer tell their own effect apart from the neighbour's. They take turns here
/// instead.
pub(crate) static PRIMARY_CONTEXT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A poisoned lock is recovered rather than propagated: the guard protects nothing
/// but test bookkeeping, and refusing to hand it out after one test failed would turn
/// that one failure into every later test failing as well.
pub(crate) fn primary_context_guard() -> std::sync::MutexGuard<'static, ()> {
    PRIMARY_CONTEXT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Cuda {
    /// The same path the `*_nvidia` tests are gated on, so that "the driver is not
    /// there" and "the driver cannot be loaded" stay one condition.
    const CUDA_PATH: &'static str = zluda_common::test_support::CUDA_DRIVER_PATH;

    fn load() -> Self {
        unsafe { Self(libloading::Library::new(Self::CUDA_PATH).unwrap()) }
    }
}

macro_rules! implemented_test {
    ($($abi:literal fn $fn_name:ident( $( $arg_id:ident : $arg_type:ty ),* ) -> $ret_type:ty;)* ) => {
        pub(crate) trait CudaApi {
            fn new() -> Self;
            $(
                #[allow(non_snake_case, dead_code)]
                fn $fn_name(&self, $( $arg_id : $arg_type ),* ) {
                    paste::paste!{ self.[< $fn_name _unchecked >]( $( $arg_id ),* ) }.unwrap()
                }
                paste::paste!{ #[allow(non_snake_case, dead_code)] fn [< $fn_name _unchecked>](&self, $( $arg_id : $arg_type ),* ) -> $ret_type; }
            )*
        }



        impl CudaApi for Cuda {
            fn new() -> Self { Self::load() }
            $(
                paste::paste!{ fn [< $fn_name _unchecked >](&self, $( $arg_id : $arg_type ),* )  -> $ret_type {
                    let func = unsafe { self.0.get::<unsafe extern $abi fn ( $( $arg_type ),* ) -> $ret_type>(concat!(stringify!($fn_name), "\0").as_bytes()) }.unwrap();
                    unsafe { (func)( $( $arg_id ),* ) }
                }}
            )*
        }

        impl CudaApi for Zluda {
            fn new() -> Self { Self }
            $(
                paste::paste!{ fn [< $fn_name _unchecked >](&self, $( $arg_id : $arg_type ),* )  -> $ret_type {
                    unsafe { super::$fn_name( $( $arg_id ),* ) }
                }}
            )*
        }
    };
}
cuda_macros::cuda_function_declarations!(implemented_test);
