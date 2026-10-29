//! `git` spawn latency from a large-RSS process (`#[ignore]` bench). The
//! headed app holds several GB, and a `fork()` of such a process has to copy
//! its page tables and take every allocator lock. Run:
//! `cargo test -p mast-harness --profile installed --test perf_bench -- --ignored --nocapture`
//! (`BALLAST_MB=0` for a small process).

use std::time::Instant;

#[test]
#[ignore = "benchmark: run with --ignored --nocapture"]
fn bench_git_spawn_latency() {
    let mb: usize = std::env::var("BALLAST_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(2048);
    // Touch every page so the memory is resident, like the app's heap.
    let mut ballast = vec![0u8; mb << 20];
    for i in (0..ballast.len()).step_by(4096) {
        ballast[i] = 1;
    }
    std::hint::black_box(&ballast);
    let n = 200;
    let mut times = Vec::with_capacity(n);
    let mut spawns = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        let mut command = mast_harness::git_command();
        command
            .arg("--version")
            .stdout(std::process::Stdio::null());
        let mut child = command.spawn().unwrap();
        spawns.push(t.elapsed().as_secs_f64() * 1000.0);
        assert!(child.wait().unwrap().success());
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    spawns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    eprintln!(
        "BENCH harness | spawn() call only with {mb} MiB resident x{n} | mean_ms={:.3} | p50_ms={:.3} | p95_ms={:.3}",
        spawns.iter().sum::<f64>() / n as f64,
        spawns[n / 2],
        spawns[n * 95 / 100]
    );
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    eprintln!(
        "BENCH harness | git --version with {mb} MiB resident x{n} | mean_ms={:.2} | p50_ms={:.2} | p95_ms={:.2} | max_ms={:.2}",
        times.iter().sum::<f64>() / n as f64,
        times[n / 2],
        times[n * 95 / 100],
        times[n - 1]
    );
}
