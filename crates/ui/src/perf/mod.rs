//! Headless benchmarks (`#[ignore]` tests) and their shared helpers. Run:
//! `cargo test -p mast-ui --lib --profile installed -- --ignored --nocapture
//! --test-threads=1 perf::` (or a single bench by name). Each bench prints
//! `BENCH <name> | <label> | ms=… | allocs=… | MiB=…` lines; see
//! `docs/perf-baseline.md` for the baseline and the list of commands.
//!
//! The counting allocator is installed for the whole ui test binary (one
//! relaxed atomic add per allocation, negligible next to the work measured).

pub(crate) mod data;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

pub struct CountingAlloc;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size() as u64, Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(layout.size() as u64, Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(new_size as u64, Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// (allocation count, bytes requested) since process start.
pub fn alloc_totals() -> (u64, u64) {
    (ALLOCS.load(Relaxed), BYTES.load(Relaxed))
}

/// Run `f` once, print one BENCH line, return its result.
pub fn measure<R>(bench: &str, label: &str, f: impl FnOnce() -> R) -> R {
    let (a0, b0) = alloc_totals();
    let t = Instant::now();
    let out = f();
    let elapsed = t.elapsed();
    let (a1, b1) = alloc_totals();
    eprintln!(
        "BENCH {bench} | {label} | ms={:.2} | allocs={} | MiB={:.2}",
        elapsed.as_secs_f64() * 1000.0,
        a1 - a0,
        (b1 - b0) as f64 / (1024.0 * 1024.0)
    );
    out
}

/// Run `f` `n` times, print mean / p50 / p95 / max ms and mean allocations
/// per iteration.
pub fn measure_n(bench: &str, label: &str, n: usize, mut f: impl FnMut(usize)) {
    // BENCH_SCALE=20 repeats every loop 20x (long enough to attach `sample`).
    let n = n * std::env::var("BENCH_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1usize);
    let (a0, b0) = alloc_totals();
    let mut times = Vec::with_capacity(n);
    for i in 0..n {
        let t = Instant::now();
        f(i);
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let (a1, b1) = alloc_totals();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = times.iter().sum::<f64>() / n as f64;
    eprintln!(
        "BENCH {bench} | {label} x{n} | mean_ms={mean:.2} | p50_ms={:.2} | p95_ms={:.2} | max_ms={:.2} | allocs/iter={} | KiB/iter={:.1}",
        times[n / 2],
        times[(n * 95 / 100).min(n - 1)],
        times[n - 1],
        (a1 - a0) / n as u64,
        (b1 - b0) as f64 / 1024.0 / n as f64
    );
}
