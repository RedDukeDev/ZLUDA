//! Support for the test suites that run against both an NVIDIA driver and ZLUDA.
//!
//! `cuda_macros::test_cuda` turns every annotated test into two: `*_zluda`, which runs
//! against this crate's implementation, and `*_nvidia`, which runs the same test
//! against the real driver so the two can be compared. The second half needs a machine
//! that has the driver, and this is where the suites decide whether they do.
//!
//! Nothing here is gated on `cfg(test)`: this crate is built as a dependency of the
//! test binaries, where `cfg(test)` is false.

/// Where the NVIDIA driver lives on this platform.
///
/// On Windows a ZLUDA install puts a redirecting `nvcuda.dll` at this path, and that
/// one does forward to the real driver, so loading it still means what it says.
#[cfg(windows)]
pub const CUDA_DRIVER_PATH: &str = "C:\\Windows\\System32\\nvcuda.dll";
#[cfg(not(windows))]
pub const CUDA_DRIVER_PATH: &str = "/usr/lib/x86_64-linux-gnu/libcuda.so.1";

/// Whether the `*_nvidia` half of the tests can run, saying so once when it cannot.
///
/// Saying so means reaching the real stderr: the test harness captures `eprintln!` and
/// only replays it for tests that fail, and this notice is specifically about the
/// tests that are *not* going to fail because they are not going to run. A green suite
/// that quietly skipped a third of itself is the thing worth avoiding.
///
/// The once-per-process flag lives here rather than in the macro because an attribute
/// expands into a function per test: a `static` inside one would be a different
/// `static` for every test, and the notice would arrive once per test instead.
#[doc(hidden)]
pub fn nvidia_tests_available() -> bool {
    if std::path::Path::new(CUDA_DRIVER_PATH).exists() {
        return true;
    }
    if std::env::var_os("ZLUDA_REQUIRE_CUDA").is_some() {
        panic!("ZLUDA_REQUIRE_CUDA is set, but the NVIDIA driver is not at {CUDA_DRIVER_PATH}");
    }
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        use std::io::Write;
        let mut stderr = std::io::stderr();
        let _ = stderr.write_fmt(format_args!(
            "[zluda] {} not found: skipping every *_nvidia test. Set ZLUDA_REQUIRE_CUDA=1 to \
             fail instead.\n",
            CUDA_DRIVER_PATH
        ));
    }
    false
}
