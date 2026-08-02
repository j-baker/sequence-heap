//! Kani proof harnesses (bounded model checking).  Compiled only under
//! `cargo kani`, which sets `cfg(kani)`; see README "Model checking".
//!
//! Each harness quantifies over *all* key values and all operation
//! sequences within its bound, and CBMC additionally checks every memory
//! access, arithmetic overflow and `unsafe` precondition on every path.
//! Bounds are small on purpose: the solver bit-blasts every comparison, so
//! keys are 8-bit values widened to the real key type (the heap code is the
//! same instantiation either way) and element counts are a handful.  The
//! insertion buffer and the level (loser tree + merge) are proved
//! separately; the whole heap composed with `sort_unstable` and the level
//! allocations is beyond what CBMC finishes at a useful bound, and that
//! composition is covered by the exhaustive test in `tests/model_check.rs`.

use crate::buffer::Buffer;
use crate::Min;

/// The insertion buffer on 8-byte keys (8-ary, by-value tournament, hole
/// technique): push up to 4 symbolic keys, then pop everything and check
/// it comes out sorted with nothing lost.
#[kani::proof]
#[kani::unwind(6)]
fn buffer_u64_pushes_then_pops_are_sorted() {
    const N: usize = 4;
    let n: usize = N;
    let mut b: Buffer<u64, Min> = Buffer::with_capacity(N);
    let mut sum: u64 = 0;
    for _ in 0..n {
        let k = kani::any::<u8>() as u64;
        b.push(k);
        sum += k;
    }
    assert!(b.len() == n);
    let mut prev: Option<u64> = None;
    let mut got = 0usize;
    let mut back: u64 = 0;
    while let Some(x) = b.pop() {
        if let Some(p) = prev {
            assert!(p <= x, "pops out of order");
        }
        prev = Some(x);
        back += x;
        got += 1;
    }
    assert!(got == n, "an element was lost or duplicated");
    assert!(back == sum, "the multiset changed");
}

/// 16-byte pair keys take the 4-ary, by-value path: push 4 symbolic pairs,
/// pop everything, order and count preserved.
#[kani::proof]
#[kani::unwind(6)]
fn buffer_pair_pushes_then_pops_are_sorted() {
    const N: usize = 4;
    let mut b: Buffer<(u64, u32), Min> = Buffer::with_capacity(N);
    for _ in 0..N {
        let k: (u64, u32) = (kani::any::<u8>() as u64, kani::any::<u8>() as u32);
        b.push(k);
    }
    assert!(b.len() == N);
    let mut prev: Option<(u64, u32)> = None;
    let mut got = 0usize;
    while let Some(x) = b.pop() {
        if let Some(p) = prev {
            assert!(p <= x, "pops out of order");
        }
        prev = Some(x);
        got += 1;
    }
    assert!(got == N, "an element was lost or duplicated");
}

/// Wide keys take the by-index path (binary layout, references only).
#[kani::proof]
#[kani::unwind(6)]
fn buffer_wide_keys_sorted() {
    let n: usize = 3;
    let mut b: Buffer<[u64; 5], Min> = Buffer::with_capacity(3);
    for _ in 0..n {
        let k: [u64; 5] = [kani::any::<u8>() as u64, 0, 0, 0, kani::any::<u8>() as u64];
        b.push(k);
    }
    let mut prev: Option<[u64; 5]> = None;
    let mut got = 0;
    while let Some(x) = b.pop() {
        if let Some(p) = prev {
            assert!(p <= x);
        }
        prev = Some(x);
        got += 1;
    }
    assert!(got == n);
}

/// A level's loser tree and K-way merge, driven directly with pre-sorted
/// symbolic runs (reverse heap order, so the head is last): popping the
/// level yields the global minimum every time and `merge_all` returns the
/// union, sorted.  This proves the merge logic without asking the solver
/// to unroll `sort_unstable` and the whole structure's allocations.
#[kani::proof]
#[kani::unwind(6)]
fn level_pops_in_order_and_merges() {
    use crate::seq::Level;
    const K: usize = 4;
    let mut lv: Level<u8, K> = Level::new();
    let mut total = 0usize;
    for slot in 0..K {
        let len: usize = kani::any();
        kani::assume(len <= 1);
        let mut run: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(1);
        let mut prev: Option<u8> = None;
        for _ in 0..len {
            let k: u8 = kani::any();
            // reverse heap order for Min: non-increasing
            if let Some(p) = prev {
                kani::assume(k <= p);
            }
            run.push(k);
            prev = Some(k);
        }
        total += run.len();
        lv.runs[slot] = run;
    }
    lv.rebuild::<Min>();
    let mut prev: Option<u8> = None;
    let mut got = 0usize;
    let pops: usize = kani::any();
    kani::assume(pops <= 2);
    for _ in 0..pops {
        match lv.pop::<Min>() {
            Some(x) => {
                if let Some(p) = prev {
                    assert!(p <= x, "level pop out of order");
                }
                prev = Some(x);
                got += 1;
            }
            None => assert!(got == total),
        }
    }
    let merged = lv.merge_all::<Min>();
    assert!(merged.len() + got == total, "merge lost or duplicated");
    // merged is in reverse heap order: non-increasing
    for w in merged.windows(2) {
        assert!(w[0] >= w[1], "merge output unsorted");
    }
    if let (Some(p), Some(&first)) = (prev, merged.last()) {
        assert!(p <= first, "merge output precedes earlier pops");
    }
}
