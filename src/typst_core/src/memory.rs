//! Where the native library's memory comes from, and how it is given back.
//!
//! Without help, a Rust `cdylib` allocates through the `malloc` of the process that loads it. For a
//! .NET host on Linux that is glibc, which keeps memory that was freed in the middle of its arenas
//! instead of returning it to the operating system: after one large compilation the process stays
//! at the size of that compilation's peak for good, even though almost all of it is free again.
//! Which allocator ends up serving the library also depends on how it was linked (musl, glibc) and
//! on the platform, so the memory behaviour of the same document differed per operating system.
//!
//! With the `mimalloc` feature, which is on by default, every allocation Rust makes in this library
//! goes to mimalloc. That is private to the library: it does not replace the `malloc` of the host,
//! and nothing allocated here is ever freed by the host or the other way round. Every buffer that
//! crosses the ABI is released by one of the `free_*` functions of this crate.

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Returns memory that has been freed to the operating system.
///
/// mimalloc hands memory back by itself after a short delay, but only while the threads that
/// allocated it keep allocating. This does it right away, which is what a caller wants after it
/// cleared the compilation cache and is not going to compile for a while. It is not free: it walks
/// the heaps, so it belongs after an unusually large compilation, not after every one.
///
/// Does nothing without the `mimalloc` feature.
pub(crate) fn release_free_memory() {
    #[cfg(feature = "mimalloc")]
    {
        // SAFETY: `mi_collect` may be called at any time from any thread.
        unsafe { libmimalloc_sys::mi_collect(true) };
    }
}
