//! Proves that clearing the compilation cache hands memory back to the operating system, not only
//! to the allocator's free lists. A process that frees a gigabyte and still holds it is as
//! dead under a memory limit as one that leaks it.
//!
//! The cache is filled on one thread and cleared on another, the way a server does it: requests
//! are compiled on whichever thread picks them up, and the cache is cleared by whoever notices
//! that a large document went through. Memory freed by a thread other than the one that allocated
//! it is the case an allocator is most likely to hold on to.
//!
//! This file is its own test binary and holds a single test on purpose: the cache and the
//! allocator are both process-wide, so a second test running in parallel would skew the numbers.
#![cfg(feature = "mimalloc")]

use std::ffi::{c_char, CString};
use std::sync::mpsc;
use std::thread;

use typst_core::{
    compile, create_compiler, evict_cache, free_compile_result, free_compiler, set_sys_inputs,
    Compiler,
};

/// The memory the process holds in physical memory, which is what a container limit counts.
///
/// On Linux that is read from the kernel. mimalloc can only estimate it there, from what it has
/// committed, and committed memory it has returned to the operating system keeps counting.
#[cfg(target_os = "linux")]
fn resident_bytes() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kib: usize = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .expect("VmRSS is missing from /proc/self/status")
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .unwrap();
    kib * 1024
}

#[cfg(not(target_os = "linux"))]
fn resident_bytes() -> usize {
    let mut rss = 0usize;
    // SAFETY: every out-parameter of `mi_process_info` is optional, so the ones left null are
    // skipped, and the one passed is a valid pointer to a `usize`. On Windows and macOS the
    // resident size it reports is exact.
    unsafe {
        libmimalloc_sys::mi_process_info(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut rss,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }
    rss
}

/// A compiler that is handed to another thread. It is only ever used by one thread at a time.
struct SendCompiler(*mut Compiler);

// SAFETY: the compiler is used by the worker thread alone, until that thread has finished.
unsafe impl Send for SendCompiler {}

#[test]
fn clearing_the_cache_returns_the_memory_to_the_operating_system() {
    // Enough distinct content that the memoised layout dwarfs anything else a compilation keeps.
    // Every heading carries `sys.inputs.run`, so a compilation with a new value cannot reuse what
    // an earlier one cached, whether or not that was evicted.
    let source = "#set document(date: none)\n#let run = sys.inputs.at(\"run\")\n".to_owned()
        + &(0..200)
            .map(|page| format!("= Page {page} of run #run\n\n#lorem(300)\n\n#pagebreak()\n"))
            .collect::<String>();

    let root = CString::new(".").unwrap();
    let sys_inputs = CString::new(r#"{"run": "warm-up"}"#).unwrap();
    let compiler = unsafe {
        create_compiler(
            root.as_ptr(),
            std::ptr::null(),
            source.as_ptr(),
            source.len(),
            std::ptr::null::<*const c_char>(),
            0,
            std::ptr::null(),
            sys_inputs.as_ptr(),
            true,
            true,
        )
    };
    assert!(!compiler.is_null(), "failed to create compiler");

    // A first compilation loads the fonts and parses the source into the compiler's own world.
    // Those stay with the compiler, so the memory in use after clearing the cache is the baseline
    // that a compilation's cached results are measured against.
    let warm_up = unsafe { compile(compiler, std::ptr::null(), 96.0, 0.0, std::ptr::null()) };
    assert!(
        warm_up.error_ptr.is_null(),
        "valid document failed to compile"
    );
    unsafe { free_compile_result(warm_up) };
    assert!(evict_cache(), "clearing the cache failed");
    let baseline = resident_bytes();

    let measured_inputs = CString::new(r#"{"run": "measured"}"#).unwrap();
    assert!(
        unsafe { set_sys_inputs(compiler, measured_inputs.as_ptr()) },
        "failed to set the inputs"
    );

    // The worker compiles and then stays alive until the main thread is done measuring, so that
    // the memory it allocated is not simply released with the thread.
    let (compiled_tx, compiled_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel::<()>();
    let worker_compiler = SendCompiler(compiler);
    let worker = thread::spawn(move || {
        let worker_compiler = worker_compiler;
        let result = unsafe {
            compile(
                worker_compiler.0,
                std::ptr::null(),
                96.0,
                0.0,
                std::ptr::null(),
            )
        };
        assert!(
            result.error_ptr.is_null(),
            "valid document failed to compile"
        );
        unsafe { free_compile_result(result) };
        compiled_tx.send(()).unwrap();
        finish_rx.recv().unwrap();
    });

    compiled_rx.recv().unwrap();
    let after_compile = resident_bytes().saturating_sub(baseline);

    assert!(evict_cache(), "clearing the cache failed");
    let after_evict = resident_bytes().saturating_sub(baseline);

    eprintln!(
        "resident above baseline: {} KiB after compiling, {} KiB after clearing the cache",
        after_compile / 1024,
        after_evict / 1024
    );

    finish_tx.send(()).unwrap();
    worker.join().unwrap();
    unsafe { free_compiler(compiler) };

    assert!(
        after_compile > 16 * 1024 * 1024,
        "compiling 200 pages left only {after_compile} bytes resident, so this test proves nothing"
    );
    assert!(
        after_evict < after_compile / 4,
        "clearing the cache left {after_evict} of {after_compile} resident bytes with the process"
    );
}
