//! Benchmark harness: `sequence_heap::MinHeap<T>` against
//! `std::collections::BinaryHeap<Reverse<T>>`.
//!
//!     cargo bench -- [--filter SUBSTR] [--impl std|fast|both] [--reps N]
//!                    [--sizes 1024,65536] [--ops N]
//!
//! Each scenario is run `reps` times per size and the fastest run is
//! reported (least noise).  With `--impl` restricted to one implementation
//! and `--filter` naming one scenario the process can be put under
//! `perf stat` or `valgrind --tool=cachegrind`.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::hint::black_box;
use std::time::{Duration, Instant};

use sequence_heap::MinHeap;

// ----------------------------------------------------------------------
// A tiny PRNG (splitmix64) so the workload is deterministic.
// ----------------------------------------------------------------------

#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_add(0x9E3779B97F4A7C15))
    }
    #[inline]
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
}

// ----------------------------------------------------------------------
// Key types.
// ----------------------------------------------------------------------

trait Key: Ord + Clone {
    const NAME: &'static str;
    fn gen(rng: &mut Rng) -> Self;
    /// A key slightly larger than `self` (for the monotone "hold" model).
    fn bump(&self, rng: &mut Rng) -> Self;
    fn fold(&self, acc: u64) -> u64;
}

impl Key for u64 {
    const NAME: &'static str = "u64";
    fn gen(rng: &mut Rng) -> Self {
        rng.next()
    }
    fn bump(&self, rng: &mut Rng) -> Self {
        self.wrapping_add(rng.next() % 1000)
    }
    fn fold(&self, acc: u64) -> u64 {
        acc.wrapping_mul(31).wrapping_add(*self)
    }
}

impl Key for u32 {
    const NAME: &'static str = "u32";
    fn gen(rng: &mut Rng) -> Self {
        rng.next() as u32
    }
    fn bump(&self, rng: &mut Rng) -> Self {
        self.wrapping_add((rng.next() % 1000) as u32)
    }
    fn fold(&self, acc: u64) -> u64 {
        acc.wrapping_mul(31).wrapping_add(*self as u64)
    }
}

/// Dijkstra-style (distance, vertex) entry: 16 bytes, 4-ary.
impl Key for (u64, u32) {
    const NAME: &'static str = "(u64,u32)";
    fn gen(rng: &mut Rng) -> Self {
        (rng.next(), rng.next() as u32)
    }
    fn bump(&self, rng: &mut Rng) -> Self {
        (self.0.wrapping_add(rng.next() % 1000), rng.next() as u32)
    }
    fn fold(&self, acc: u64) -> u64 {
        acc.wrapping_mul(31).wrapping_add(self.0 ^ self.1 as u64)
    }
}

/// 32-byte plain-data record (4-ary).
impl Key for [u64; 4] {
    const NAME: &'static str = "[u64;4]";
    fn gen(rng: &mut Rng) -> Self {
        [rng.next(), rng.next(), rng.next(), rng.next()]
    }
    fn bump(&self, rng: &mut Rng) -> Self {
        [
            self[0].wrapping_add(rng.next() % 1000),
            rng.next(),
            rng.next(),
            rng.next(),
        ]
    }
    fn fold(&self, acc: u64) -> u64 {
        acc.wrapping_mul(31).wrapping_add(self[0])
    }
}

/// 64-byte plain-data record (one cache line), ordered by its first field.
impl Key for [u64; 8] {
    const NAME: &'static str = "[u64;8]";
    fn gen(rng: &mut Rng) -> Self {
        let mut a = [0u64; 8];
        for x in a.iter_mut() {
            *x = rng.next();
        }
        a
    }
    fn bump(&self, rng: &mut Rng) -> Self {
        let mut a = *self;
        a[0] = a[0].wrapping_add(rng.next() % 1000);
        a
    }
    fn fold(&self, acc: u64) -> u64 {
        acc.wrapping_mul(31).wrapping_add(self[0])
    }
}

/// 128-byte plain-data record (two cache lines), ordered by its first field.
impl Key for [u64; 16] {
    const NAME: &'static str = "[u64;16]";
    fn gen(rng: &mut Rng) -> Self {
        let mut a = [0u64; 16];
        for x in a.iter_mut() {
            *x = rng.next();
        }
        a
    }
    fn bump(&self, rng: &mut Rng) -> Self {
        let mut a = *self;
        a[0] = a[0].wrapping_add(rng.next() % 1000);
        a
    }
    fn fold(&self, acc: u64) -> u64 {
        acc.wrapping_mul(31).wrapping_add(self[0])
    }
}

macro_rules! wide_key {
    ($n:literal, $name:literal) => {
        impl Key for [u64; $n] {
            const NAME: &'static str = $name;
            fn gen(rng: &mut Rng) -> Self {
                let mut a = [0u64; $n];
                for x in a.iter_mut() {
                    *x = rng.next();
                }
                a
            }
            fn bump(&self, rng: &mut Rng) -> Self {
                let mut a = *self;
                a[0] = a[0].wrapping_add(rng.next() % 1000);
                a
            }
            fn fold(&self, acc: u64) -> u64 {
                acc.wrapping_mul(31).wrapping_add(self[0])
            }
        }
    };
}
wide_key!(32, "[u64;32]");
wide_key!(64, "[u64;64]");

impl Key for String {
    const NAME: &'static str = "String";
    fn gen(rng: &mut Rng) -> Self {
        format!("{:016x}", rng.next())
    }
    fn bump(&self, rng: &mut Rng) -> Self {
        let v = u64::from_str_radix(self, 16).unwrap_or(0);
        format!("{:016x}", v.wrapping_add(rng.next() % 1000))
    }
    fn fold(&self, acc: u64) -> u64 {
        acc.wrapping_mul(31)
            .wrapping_add(self.len() as u64 + self.as_bytes()[0] as u64)
    }
}

// ----------------------------------------------------------------------
// Priority-queue abstraction over both implementations.
// ----------------------------------------------------------------------

trait Pq<T: Ord> {
    fn with_capacity(n: usize) -> Self;
    fn from_vec(v: Vec<T>) -> Self;
    fn push(&mut self, x: T);
    fn pop(&mut self) -> Option<T>;
    fn push_pop(&mut self, x: T) -> T;
    fn replace(&mut self, x: T) -> Option<T>;
    fn into_sorted_vec(self) -> Vec<T>;
    fn len(&self) -> usize;
}

impl<T: Ord> Pq<T> for BinaryHeap<Reverse<T>> {
    fn with_capacity(n: usize) -> Self {
        BinaryHeap::with_capacity(n)
    }
    fn from_vec(v: Vec<T>) -> Self {
        BinaryHeap::from(v.into_iter().map(Reverse).collect::<Vec<_>>())
    }
    #[inline]
    fn push(&mut self, x: T) {
        BinaryHeap::push(self, Reverse(x))
    }
    #[inline]
    fn pop(&mut self) -> Option<T> {
        BinaryHeap::pop(self).map(|r| r.0)
    }
    #[inline]
    fn push_pop(&mut self, x: T) -> T {
        BinaryHeap::push(self, Reverse(x));
        BinaryHeap::pop(self).unwrap().0
    }
    #[inline]
    fn replace(&mut self, x: T) -> Option<T> {
        let r = BinaryHeap::pop(self).map(|r| r.0);
        BinaryHeap::push(self, Reverse(x));
        r
    }
    fn into_sorted_vec(self) -> Vec<T> {
        let mut v = BinaryHeap::into_sorted_vec(self);
        v.reverse();
        v.into_iter().map(|r| r.0).collect()
    }
    fn len(&self) -> usize {
        BinaryHeap::len(self)
    }
}

impl<T: Ord> Pq<T> for MinHeap<T> {
    fn with_capacity(n: usize) -> Self {
        MinHeap::with_capacity(n)
    }
    fn from_vec(v: Vec<T>) -> Self {
        MinHeap::from(v)
    }
    #[inline]
    fn push(&mut self, x: T) {
        MinHeap::push(self, x)
    }
    #[inline]
    fn pop(&mut self) -> Option<T> {
        MinHeap::pop(self)
    }
    #[inline]
    fn push_pop(&mut self, x: T) -> T {
        MinHeap::push_pop(self, x)
    }
    #[inline]
    fn replace(&mut self, x: T) -> Option<T> {
        MinHeap::replace(self, x)
    }
    fn into_sorted_vec(self) -> Vec<T> {
        MinHeap::into_sorted_vec(self)
    }
    fn len(&self) -> usize {
        MinHeap::len(self)
    }
}

// ----------------------------------------------------------------------
// Scenarios.  Each returns (elapsed, number of "operations").
// ----------------------------------------------------------------------

fn gen_keys<T: Key>(n: usize, rng: &mut Rng) -> Vec<T> {
    (0..n).map(|_| T::gen(rng)).collect()
}

/// Push n random keys, then pop them all.  One op = one push + one pop.
fn push_pop_all<T: Key, H: Pq<T>>(n: usize, _ops: usize, rng: &mut Rng) -> (Duration, usize) {
    let keys = gen_keys::<T>(n, rng);
    let mut h = H::with_capacity(n);
    let t = Instant::now();
    for k in keys {
        h.push(k);
    }
    let mut acc = 0u64;
    while let Some(k) = h.pop() {
        acc = k.fold(acc);
    }
    let d = t.elapsed();
    black_box(acc);
    (d, n)
}

/// Heapify n keys (From<Vec>) then pop them all.
fn heapify_pop_all<T: Key, H: Pq<T>>(n: usize, _ops: usize, rng: &mut Rng) -> (Duration, usize) {
    let keys = gen_keys::<T>(n, rng);
    let t = Instant::now();
    let mut h = H::from_vec(keys);
    let mut acc = 0u64;
    while let Some(k) = h.pop() {
        acc = k.fold(acc);
    }
    let d = t.elapsed();
    black_box(acc);
    (d, n)
}

/// Heapify then into_sorted_vec.
fn sort<T: Key, H: Pq<T>>(n: usize, _ops: usize, rng: &mut Rng) -> (Duration, usize) {
    let keys = gen_keys::<T>(n, rng);
    let t = Instant::now();
    let h = H::from_vec(keys);
    let v = h.into_sorted_vec();
    let d = t.elapsed();
    black_box(v);
    (d, n)
}

/// Steady state: heap of n keys; each op pops one and pushes a fresh random
/// key.
fn hold_uniform<T: Key, H: Pq<T>>(n: usize, ops: usize, rng: &mut Rng) -> (Duration, usize) {
    let keys = gen_keys::<T>(n, rng);
    let fresh = gen_keys::<T>(ops, rng);
    let mut h = H::from_vec(keys);
    let t = Instant::now();
    let mut acc = 0u64;
    for k in fresh {
        let p = h.pop().unwrap();
        acc = p.fold(acc);
        h.push(k);
    }
    let d = t.elapsed();
    black_box((acc, h.len()));
    (d, ops)
}

/// Event-simulation / Dijkstra steady state: each op pops the minimum and
/// pushes a key slightly larger than it, so keys are monotone.
fn hold_monotone<T: Key, H: Pq<T>>(n: usize, ops: usize, rng: &mut Rng) -> (Duration, usize) {
    let keys = gen_keys::<T>(n, rng);
    let mut h = H::from_vec(keys);
    let mut r = rng.clone();
    let t = Instant::now();
    let mut acc = 0u64;
    for _ in 0..ops {
        let p = h.pop().unwrap();
        acc = p.fold(acc);
        h.push(p.bump(&mut r));
    }
    let d = t.elapsed();
    black_box((acc, h.len()));
    (d, ops)
}

/// Like hold_monotone but via the fused `replace` (pop-then-push).
fn replace_monotone<T: Key, H: Pq<T>>(n: usize, ops: usize, rng: &mut Rng) -> (Duration, usize) {
    let keys = gen_keys::<T>(n, rng);
    let mut h = H::from_vec(keys);
    let mut r = rng.clone();
    let t = Instant::now();
    let mut acc = 0u64;
    let mut next = T::gen(&mut r);
    for _ in 0..ops {
        let p = h.replace(next).unwrap();
        acc = p.fold(acc);
        next = p.bump(&mut r);
    }
    let d = t.elapsed();
    black_box((acc, h.len()));
    (d, ops)
}

/// Like hold_uniform but via the fused `push_pop` (push-then-pop).
fn push_pop_uniform<T: Key, H: Pq<T>>(n: usize, ops: usize, rng: &mut Rng) -> (Duration, usize) {
    let keys = gen_keys::<T>(n, rng);
    let fresh = gen_keys::<T>(ops, rng);
    let mut h = H::from_vec(keys);
    let t = Instant::now();
    let mut acc = 0u64;
    for k in fresh {
        let p = h.push_pop(k);
        acc = p.fold(acc);
    }
    let d = t.elapsed();
    black_box((acc, h.len()));
    (d, ops)
}

// ----------------------------------------------------------------------
// Driver.
// ----------------------------------------------------------------------

struct Case {
    name: String,
    /// Largest n worth running (the O(n log n) cases get slow).
    max_n: usize,
    std_fn: fn(usize, usize, u64) -> (Duration, usize),
    fast_fn: fn(usize, usize, u64) -> (Duration, usize),
}

macro_rules! case {
    ($scn:ident, $t:ty, $max:expr) => {
        Case {
            name: format!("{}/{}", stringify!($scn), <$t as Key>::NAME),
            max_n: $max,
            std_fn: |n, ops, seed| $scn::<$t, BinaryHeap<Reverse<$t>>>(n, ops, &mut Rng::new(seed)),
            fast_fn: |n, ops, seed| $scn::<$t, MinHeap<$t>>(n, ops, &mut Rng::new(seed)),
        }
    };
}

fn cases() -> Vec<Case> {
    const BIG: usize = 1 << 24;
    const MID: usize = 1 << 22;
    vec![
        case!(hold_uniform, u64, BIG),
        case!(hold_monotone, u64, BIG),
        case!(push_pop_all, u64, MID),
        case!(heapify_pop_all, u64, MID),
        case!(sort, u64, MID),
        case!(replace_monotone, u64, BIG),
        case!(push_pop_uniform, u64, BIG),
        case!(hold_uniform, u32, BIG),
        case!(hold_monotone, u32, BIG),
        case!(hold_uniform, (u64, u32), BIG),
        case!(hold_monotone, (u64, u32), BIG),
        case!(push_pop_all, (u64, u32), MID),
        case!(hold_uniform, [u64; 4], MID),
        case!(hold_uniform, [u64; 8], MID),
        case!(hold_monotone, [u64; 8], MID),
        case!(push_pop_all, [u64; 8], MID),
        case!(hold_uniform, [u64; 16], MID),
        case!(push_pop_all, [u64; 16], MID),
        case!(hold_uniform, [u64; 32], MID),
        case!(push_pop_all, [u64; 32], MID),
        case!(hold_uniform, [u64; 64], 1 << 20),
        case!(push_pop_all, [u64; 64], 1 << 20),
        case!(hold_uniform, String, 1 << 20),
        case!(push_pop_all, String, 1 << 20),
    ]
}

struct Opts {
    filter: Option<String>,
    which: String,
    reps: usize,
    sizes: Vec<usize>,
    ops: usize,
}

fn parse_opts() -> Opts {
    let mut o = Opts {
        filter: None,
        which: "both".into(),
        reps: 3,
        sizes: vec![1 << 8, 1 << 12, 1 << 16, 1 << 20, 1 << 24],
        ops: 1 << 21,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--filter" => {
                o.filter = Some(args[i + 1].clone());
                i += 1;
            }
            "--impl" => {
                o.which = args[i + 1].clone();
                i += 1;
            }
            "--reps" => {
                o.reps = args[i + 1].parse().unwrap();
                i += 1;
            }
            "--ops" => {
                o.ops = args[i + 1].parse().unwrap();
                i += 1;
            }
            "--sizes" => {
                o.sizes = args[i + 1].split(',').map(|s| s.parse().unwrap()).collect();
                i += 1;
            }
            "--bench" | "--nocapture" => {}
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    o
}

fn best(f: fn(usize, usize, u64) -> (Duration, usize), n: usize, ops: usize, reps: usize) -> f64 {
    let mut best = f64::INFINITY;
    for rep in 0..reps {
        let (d, count) = f(n, ops, 42 + rep as u64);
        best = best.min(d.as_nanos() as f64 / count as f64);
    }
    best
}

fn main() {
    let o = parse_opts();
    let run_std = o.which == "std" || o.which == "both";
    let run_fast = o.which == "fast" || o.which == "both";
    println!(
        "{:<26} {:>9} {:>10} {:>10} {:>8}",
        "scenario", "n", "std", "ours", "speedup"
    );
    for c in cases() {
        if let Some(f) = &o.filter {
            if !c.name.contains(f.as_str()) {
                continue;
            }
        }
        for &n in &o.sizes {
            if n > c.max_n {
                continue;
            }
            let s = if run_std {
                best(c.std_fn, n, o.ops, o.reps)
            } else {
                f64::NAN
            };
            let f = if run_fast {
                best(c.fast_fn, n, o.ops, o.reps)
            } else {
                f64::NAN
            };
            println!(
                "{:<26} {:>9} {:>10.1} {:>10.1} {:>7.2}x",
                c.name,
                n,
                s,
                f,
                s / f
            );
        }
    }
}
