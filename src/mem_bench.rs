//! Memory latency benchmark using the random pointer-chasing technique.
//!
//! A buffer of N bytes is filled with a random Hamiltonian cycle: each element
//! stores the index of the next element to read, spaced one cache-line apart.
//! Traversing this chain forces the CPU to fetch each successive cache line
//! before it can compute the next address — completely defeating the hardware
//! prefetcher.  The measured nanoseconds-per-access at each working-set size
//! maps directly onto L1 / L2 / L3 / DRAM latency.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ── Constants ─────────────────────────────────────────────────────────────────

/// x86-64 cache line size (bytes)
const CACHE_LINE: usize = 64;
/// u64 elements per cache line
const STRIDE: usize = CACHE_LINE / 8;

/// Working-set sizes tested, from L1 through DRAM (powers of two).
pub const TEST_SIZES: &[usize] = &[
    4 * 1024,          //   4 KiB  (L1)
    8 * 1024,          //   8 KiB
    16 * 1024,         //  16 KiB
    32 * 1024,         //  32 KiB
    64 * 1024,         //  64 KiB  (L2)
    128 * 1024,        // 128 KiB
    256 * 1024,        // 256 KiB
    512 * 1024,        // 512 KiB
    1024 * 1024,       //  1 MiB  (L3)
    2 * 1024 * 1024,   //  2 MiB
    4 * 1024 * 1024,   //  4 MiB
    8 * 1024 * 1024,   //  8 MiB
    16 * 1024 * 1024,  // 16 MiB
    32 * 1024 * 1024,  // 32 MiB
    64 * 1024 * 1024,  // 64 MiB  (DRAM)
    128 * 1024 * 1024, // 128 MiB
    256 * 1024 * 1024, // 256 MiB
];

// ── Cache topology ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct CacheSizes {
    pub l1d: usize,
    pub l2: usize,
    pub l3: usize,
}

impl CacheSizes {
    pub fn read() -> Self {
        let base = Path::new("/sys/devices/system/cpu/cpu0/cache");
        let mut l1d = 32 * 1024;
        let mut l2 = 512 * 1024;
        let mut l3 = 32 * 1024 * 1024;

        for idx in 0..=8 {
            let dir = base.join(format!("index{idx}"));
            let level: u32 = read_u(&dir.join("level")).unwrap_or(0);
            let ctype = read_s(&dir.join("type")).unwrap_or_default();
            if let Some(sz) = parse_cache_size(&dir.join("size")) {
                match (level, ctype.as_str()) {
                    (1, "Data") => l1d = sz,
                    (2, _) => l2 = sz,
                    (3, _) if sz > l3 => l3 = sz,
                    _ => {}
                }
            }
        }
        Self { l1d, l2, l3 }
    }
}

fn read_s(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_u(path: &Path) -> Option<u32> {
    read_s(path)?.parse().ok()
}

fn parse_cache_size(path: &Path) -> Option<usize> {
    let s = read_s(path)?;
    if let Some(n) = s.strip_suffix('K') {
        return n.trim().parse::<usize>().ok().map(|v| v * 1024);
    }
    if let Some(n) = s.strip_suffix('M') {
        return n.trim().parse::<usize>().ok().map(|v| v * 1024 * 1024);
    }
    s.parse().ok()
}

// ── Result types ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct LatencyPoint {
    pub size_bytes: usize,
    pub latency_ns: f64,
}

#[derive(Clone, Debug, Default)]
pub struct MemLatencyResult {
    pub points: Vec<LatencyPoint>,
    /// 0.0 → 1.0
    pub progress: f32,
    pub current_size: Option<usize>,
    pub running: bool,
    pub complete: bool,
}

// ── Benchmark controller ──────────────────────────────────────────────────────

pub struct MemLatencyBench {
    pub result: Arc<Mutex<MemLatencyResult>>,
    cancel: Arc<AtomicBool>,
}

impl MemLatencyBench {
    pub fn new() -> Self {
        Self {
            result: Arc::new(Mutex::new(MemLatencyResult::default())),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Spawn the benchmark thread.  No-op if already running.
    pub fn start(&self) {
        // Check-and-set `running` in a single locked section so two
        // concurrent start() calls can't both pass the check and spawn two
        // workers racing on the same result (and a second ≥512 MiB alloc).
        {
            let mut r = self.result.lock().unwrap();
            if r.running {
                return;
            }
            *r = MemLatencyResult {
                running: true,
                ..Default::default()
            };
        }

        self.cancel.store(false, Ordering::Relaxed);
        let result = Arc::clone(&self.result);
        let cancel = Arc::clone(&self.cancel);

        std::thread::Builder::new()
            .name("mem-latency".into())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || run_bench(result, cancel))
            .expect("failed to spawn mem-latency thread");
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MemLatencyResult {
        self.result.lock().unwrap().clone()
    }
}

// ── Worker ────────────────────────────────────────────────────────────────────

fn run_bench(result: Arc<Mutex<MemLatencyResult>>, cancel: Arc<AtomicBool>) {
    let n = TEST_SIZES.len();

    for (i, &size) in TEST_SIZES.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }

        {
            let mut r = result.lock().unwrap();
            r.progress = i as f32 / n as f32;
            r.current_size = Some(size);
        }

        if let Some(lat) = measure_latency(size, &cancel) {
            let mut r = result.lock().unwrap();
            r.points.push(LatencyPoint {
                size_bytes: size,
                latency_ns: lat,
            });
        }
    }

    let mut r = result.lock().unwrap();
    r.running = false;
    r.complete = !cancel.load(Ordering::Relaxed);
    // A cancelled run keeps its partial progress — forcing 1.0 would show a
    // full bar for an aborted run.
    if r.complete {
        r.progress = 1.0;
    }
    r.current_size = None;
}

// ── Core measurement logic ────────────────────────────────────────────────────

/// Build a random pointer-chasing array for the given working-set size.
///
/// Each element at index `i` stores the *next index to read*, offset by
/// `STRIDE` so that consecutive accesses are exactly one cache line apart.
/// The permutation forms a single Hamiltonian cycle over all cache lines.
fn build_chase_array(size_bytes: usize) -> Vec<u64> {
    let n_lines = (size_bytes / CACHE_LINE).max(2);
    let buf_len = n_lines * STRIDE;

    // Create a random permutation of line indices (Fisher-Yates)
    let mut order: Vec<usize> = (0..n_lines).collect();
    let mut rng = Lcg64::new(0xDEAD_BEEF_CAFE_BABE ^ size_bytes as u64);
    for i in (1..n_lines).rev() {
        let j = rng.next_bounded(i + 1);
        order.swap(i, j);
    }

    // Build the chase: buf[order[i] * STRIDE] = order[(i+1) % n] * STRIDE
    let mut buf = vec![0u64; buf_len];
    for i in 0..n_lines {
        let src = order[i] * STRIDE;
        let dst = (order[(i + 1) % n_lines] * STRIDE) as u64;
        buf[src] = dst;
    }
    buf
}

fn measure_latency(size_bytes: usize, cancel: &AtomicBool) -> Option<f64> {
    let buf = build_chase_array(size_bytes);

    // Warm-up: bring the chase array into whatever cache level applies
    chase(&buf, 20_000);

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    // Quick estimate (50 K accesses) to calibrate timing
    let est = chase_timed(&buf, 50_000);
    if est <= 0.0 {
        return None;
    }

    // Target ~300 ms of measurement; clamp to avoid runaway on tiny arrays
    let target_ns: f64 = 300_000_000.0;
    let n = ((target_ns / est) as u64).clamp(100_000, 500_000_000);

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    Some(chase_timed(&buf, n))
}

/// Traverse the pointer-chase chain `n` times without timing.
#[inline(never)]
fn chase(buf: &[u64], n: u64) {
    let mut idx = 0u64;
    for _ in 0..n {
        // SAFETY: all indices stored in buf are valid indices into buf (by construction)
        idx = unsafe { *buf.get_unchecked(idx as usize) };
    }
    std::hint::black_box(idx);
}

/// Traverse and return nanoseconds per access.
#[inline(never)]
fn chase_timed(buf: &[u64], n: u64) -> f64 {
    let mut idx = 0u64;
    let t = std::time::Instant::now();
    for _ in 0..n {
        idx = unsafe { *buf.get_unchecked(idx as usize) };
    }
    let elapsed = t.elapsed();
    std::hint::black_box(idx);
    elapsed.as_nanos() as f64 / n as f64
}

// ── Tiny LCG PRNG (no external deps) ─────────────────────────────────────────

struct Lcg64(u64);

impl Lcg64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    #[inline]
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    #[inline]
    fn next_bounded(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

// ── Memory bandwidth benchmark ────────────────────────────────────────────────

/// One test result: label + GB/s for read, write, copy.
#[derive(Clone, Debug, Default)]
pub struct BandwidthResult {
    pub read_gb_s: f64,
    pub write_gb_s: f64,
    pub copy_gb_s: f64,
    pub running: bool,
    pub complete: bool,
    pub progress: f32,
}

pub struct MemBandwidthBench {
    pub result: Arc<Mutex<BandwidthResult>>,
    cancel: Arc<AtomicBool>,
}

impl MemBandwidthBench {
    pub fn new() -> Self {
        Self {
            result: Arc::new(Mutex::new(BandwidthResult::default())),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn snapshot(&self) -> BandwidthResult {
        self.result.lock().map(|r| r.clone()).unwrap_or_default()
    }

    pub fn start(&self) {
        // No-op if already running — a second click would spawn a concurrent
        // thread racing on `result` (and another ≥512 MiB allocation).
        // Check-and-set in one locked section so the check and the set are
        // atomic with respect to a concurrent start().
        {
            let mut r = match self.result.lock() {
                Ok(r) => r,
                Err(_) => return,
            };
            if r.running {
                return;
            }
            *r = BandwidthResult {
                running: true,
                ..Default::default()
            };
        }
        let result = Arc::clone(&self.result);
        let cancel = Arc::clone(&self.cancel);
        cancel.store(false, Ordering::Relaxed);
        std::thread::spawn(move || {
            run_bandwidth(result, cancel);
        });
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Target duration per sub-test (~1.5 seconds).
const BW_TARGET_SECS: f64 = 1.5;

fn run_bandwidth(result: Arc<Mutex<BandwidthResult>>, cancel: Arc<AtomicBool>) {
    // Choose buffer size: 4× the detected L3 cache, minimum 512 MiB.
    // On a 7950X3D (128 MiB L3 with 3D V-Cache), this gives a 512 MiB buffer,
    // ensuring all accesses miss the cache and reach DRAM.
    let l3 = CacheSizes::read().l3;
    let len_u64 = {
        let min_bytes = 512usize * 1024 * 1024;
        let four_x_l3 = l3.saturating_mul(4);
        let bytes = four_x_l3.max(min_bytes);
        // Round down to a multiple of 64 u64 elements (one cache line)
        let elems = bytes / 8;
        elems - (elems % 64)
    };
    let len_bytes = len_u64 * 8;

    // Allocate as u64 for efficient wide reads/writes.
    let mut src: Vec<u64> = vec![0u64; len_u64];
    let mut dst: Vec<u64> = vec![0u64; len_u64];

    // Fully initialise both buffers — writes every element so all pages are
    // committed and cache lines are evicted from L3 before timing starts.
    for (i, v) in src.iter_mut().enumerate() {
        *v = i as u64 ^ 0xDEAD_BEEF_CAFE_1234;
    }
    for v in dst.iter_mut() {
        *v = 0;
    }

    let update = |progress: f32, r: f64, w: f64, c: f64| {
        if let Ok(mut res) = result.lock() {
            res.progress = progress;
            res.read_gb_s = r;
            res.write_gb_s = w;
            res.copy_gb_s = c;
        }
    };

    if cancel.load(Ordering::Relaxed) {
        mark_done(&result, false);
        return;
    }

    // ── Sequential READ ───────────────────────────────────────────────────────
    // Use 8 independent accumulators to avoid a serial dependency chain.
    // The CPU can issue loads for acc0..acc7 in parallel, hiding DRAM latency
    // and saturating the memory bus.
    let read_bw = {
        let mut iters = 0u64;
        let start = std::time::Instant::now();
        let mut a0 = 0u64;
        let mut a1 = 0u64;
        let mut a2 = 0u64;
        let mut a3 = 0u64;
        let mut a4 = 0u64;
        let mut a5 = 0u64;
        let mut a6 = 0u64;
        let mut a7 = 0u64;
        loop {
            // Each iteration steps by 8 u64s = one cache line (64 bytes).
            for chunk in src.as_chunks::<8>().0 {
                a0 = a0.wrapping_add(chunk[0]);
                a1 = a1.wrapping_add(chunk[1]);
                a2 = a2.wrapping_add(chunk[2]);
                a3 = a3.wrapping_add(chunk[3]);
                a4 = a4.wrapping_add(chunk[4]);
                a5 = a5.wrapping_add(chunk[5]);
                a6 = a6.wrapping_add(chunk[6]);
                a7 = a7.wrapping_add(chunk[7]);
            }
            iters += 1;
            if start.elapsed().as_secs_f64() >= BW_TARGET_SECS {
                break;
            }
        }
        std::hint::black_box((a0, a1, a2, a3, a4, a5, a6, a7));
        let elapsed = start.elapsed().as_secs_f64();
        (len_bytes as f64 * iters as f64) / elapsed / 1e9
    };
    update(0.33, read_bw, 0.0, 0.0);

    if cancel.load(Ordering::Relaxed) {
        mark_done(&result, false);
        return;
    }

    // ── Sequential WRITE ──────────────────────────────────────────────────────
    let write_bw = {
        let mut iters = 0u64;
        let start = std::time::Instant::now();
        loop {
            for (i, v) in dst.iter_mut().enumerate() {
                *v = i as u64;
            }
            iters += 1;
            if start.elapsed().as_secs_f64() >= BW_TARGET_SECS {
                break;
            }
        }
        std::hint::black_box(dst[0]);
        let elapsed = start.elapsed().as_secs_f64();
        (len_bytes as f64 * iters as f64) / elapsed / 1e9
    };
    update(0.67, read_bw, write_bw, 0.0);

    if cancel.load(Ordering::Relaxed) {
        mark_done(&result, false);
        return;
    }

    // ── COPY (src → dst) ─────────────────────────────────────────────────────
    let copy_bw = {
        let mut iters = 0u64;
        let start = std::time::Instant::now();
        loop {
            dst.copy_from_slice(&src);
            iters += 1;
            if start.elapsed().as_secs_f64() >= BW_TARGET_SECS {
                break;
            }
        }
        std::hint::black_box(dst[0]);
        let elapsed = start.elapsed().as_secs_f64();
        // copy moves 2× buffer bytes (one read + one write)
        (2.0 * len_bytes as f64 * iters as f64) / elapsed / 1e9
    };
    update(1.0, read_bw, write_bw, copy_bw);
    mark_done(&result, true);
}

fn mark_done(result: &Arc<Mutex<BandwidthResult>>, complete: bool) {
    if let Ok(mut r) = result.lock() {
        r.running = false;
        r.complete = complete;
        r.progress = if complete { 1.0 } else { r.progress };
    }
}
