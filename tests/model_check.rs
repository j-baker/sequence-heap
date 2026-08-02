//! Correctness of the heap, three ways:
//!
//! 1. **Bounded exhaustive model checking**: every sequence of operations
//!    up to a depth, over a small key alphabet, on a heap with a tiny
//!    insertion buffer and fan-in (so flushes and merges happen every few
//!    steps), checked after every step against a reference model and
//!    against the structural invariants.
//! 2. **Randomised model testing** over many element types (every layout:
//!    zero-sized, small, wide, with destructors) and both orders, exercising
//!    the whole API.
//! 3. **Drop accounting and panic safety**: every element constructed is
//!    dropped exactly once, including when `Ord` panics mid-operation.

use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fmt::Debug;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};

use sequence_heap::{HeapOrder, Max, MaxHeap, Min, MinHeap, PeekMut, SeqHeap};

// ----------------------------------------------------------------------
// Reference model: an unordered Vec; the top is its min (or max).
// ----------------------------------------------------------------------

struct Model<T, O: HeapOrder>(Vec<T>, std::marker::PhantomData<O>);

impl<T: Ord + Clone, O: HeapOrder> Model<T, O> {
    fn new() -> Self {
        Model(Vec::new(), std::marker::PhantomData)
    }
    fn top_index(&self) -> Option<usize> {
        let mut best: Option<usize> = None;
        for i in 0..self.0.len() {
            best = match best {
                Some(b) if !O::before(&self.0[i], &self.0[b]) => Some(b),
                _ => Some(i),
            };
        }
        best
    }
    fn peek(&self) -> Option<&T> {
        self.top_index().map(|i| &self.0[i])
    }
    fn pop(&mut self) -> Option<T> {
        let i = self.top_index()?;
        Some(self.0.swap_remove(i))
    }
    fn sorted(&self) -> Vec<T> {
        let mut v = self.0.clone();
        v.sort();
        v
    }
}

/// Compare heap and model after an operation.
fn agree<T: Ord + Clone + Debug, O: HeapOrder, const M: usize, const K: usize>(
    h: &SeqHeap<T, O, M, K>,
    m: &Model<T, O>,
) {
    h.check_invariants();
    assert_eq!(h.len(), m.0.len(), "len");
    assert_eq!(h.is_empty(), m.0.is_empty());
    assert_eq!(h.peek(), m.peek(), "peek");
    let mut via_iter: Vec<T> = h.iter().cloned().collect();
    via_iter.sort();
    assert_eq!(via_iter, m.sorted(), "iter contents");
    assert_eq!(h.iter().len(), m.0.len(), "iter len");
}

// ----------------------------------------------------------------------
// 1. Bounded exhaustive model checking.
// ----------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Op {
    Push(u8),
    Pop,
    PushPop(u8),
    Replace(u8),
    PeekMutSet(u8),
    PeekMutPop,
    RetainBelow(u8),
}

const OPS: &[Op] = &[
    Op::Push(0),
    Op::Push(1),
    Op::Push(2),
    Op::Pop,
    Op::PushPop(1),
    Op::Replace(0),
    Op::PeekMutSet(2),
    Op::PeekMutPop,
    Op::RetainBelow(2),
];

fn apply<O: HeapOrder, const M: usize, const K: usize>(
    h: &mut SeqHeap<u8, O, M, K>,
    m: &mut Model<u8, O>,
    op: Op,
) {
    match op {
        Op::Push(k) => {
            h.push(k);
            m.0.push(k);
        }
        Op::Pop => assert_eq!(h.pop(), m.pop()),
        Op::PushPop(k) => {
            let a = h.push_pop(k);
            m.0.push(k);
            let b = m.pop().unwrap();
            assert_eq!(a, b);
        }
        Op::Replace(k) => {
            let a = h.replace(k);
            let b = m.pop();
            m.0.push(k);
            assert_eq!(a, b);
        }
        Op::PeekMutSet(k) => {
            if let Some(mut p) = h.peek_mut() {
                *p = k;
                m.pop().unwrap();
                m.0.push(k);
            } else {
                assert!(m.0.is_empty());
            }
        }
        Op::PeekMutPop => {
            if let Some(p) = h.peek_mut() {
                assert_eq!(Some(PeekMut::pop(p)), m.pop());
            } else {
                assert!(m.0.is_empty());
            }
        }
        Op::RetainBelow(k) => {
            h.retain(|x| *x < k);
            m.0.retain(|x| *x < k);
        }
    }
}

/// Depth-first enumeration of every op sequence of length `depth`.
fn explore<O: HeapOrder, const M: usize, const K: usize>(
    h: &SeqHeap<u8, O, M, K>,
    m: &Model<u8, O>,
    depth: usize,
    visited: &mut u64,
) {
    if depth == 0 {
        return;
    }
    for &op in OPS {
        let mut h2 = h.clone();
        let mut m2 = Model(m.0.clone(), std::marker::PhantomData);
        apply(&mut h2, &mut m2, op);
        agree(&h2, &m2);
        *visited += 1;
        explore(&h2, &m2, depth - 1, visited);
    }
}

#[test]
fn exhaustive_sequences_min() {
    // 9 ops, depth 7: 9^7 ≈ 4.8M states, each checked against the model.
    let mut visited = 0;
    let h: SeqHeap<u8, Min, 2, 2> = SeqHeap::new();
    explore(&h, &Model::new(), 7, &mut visited);
    assert!(visited > 4_000_000, "{visited}");
}

#[test]
fn exhaustive_sequences_max_wider_params() {
    let mut visited = 0;
    let h: SeqHeap<u8, Max, 3, 4> = SeqHeap::new();
    explore(&h, &Model::new(), 6, &mut visited);
    assert!(visited > 500_000, "{visited}");
}

#[test]
fn exhaustive_from_prefilled_states() {
    // Start from heaps that already have runs in two levels (a sequence of
    // pushes of every key pattern), then explore from there.
    for seed_len in [5usize, 9, 13] {
        for pattern in 0..3u8 {
            let mut h: SeqHeap<u8, Min, 2, 2> = SeqHeap::new();
            let mut m = Model::new();
            for i in 0..seed_len {
                let k = ((i as u8).wrapping_mul(7 + pattern)) % 4;
                h.push(k);
                m.0.push(k);
            }
            agree(&h, &m);
            let mut visited = 0;
            explore(&h, &m, 4, &mut visited);
        }
    }
}

// ----------------------------------------------------------------------
// 2. Randomised model testing over many element types.
// ----------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

trait Gen: Ord + Clone + Debug {
    fn gen(r: &mut Rng) -> Self;
}
impl Gen for u8 {
    fn gen(r: &mut Rng) -> Self {
        r.next() as u8
    }
}
impl Gen for u32 {
    fn gen(r: &mut Rng) -> Self {
        r.below(1000) as u32
    }
}
impl Gen for u64 {
    fn gen(r: &mut Rng) -> Self {
        r.below(1000)
    }
}
impl Gen for i64 {
    fn gen(r: &mut Rng) -> Self {
        r.below(1000) as i64 - 500
    }
}
impl Gen for u128 {
    fn gen(r: &mut Rng) -> Self {
        r.below(1000) as u128
    }
}
impl Gen for (u64, u32) {
    fn gen(r: &mut Rng) -> Self {
        (r.below(100), r.below(100) as u32)
    }
}
impl Gen for [u8; 24] {
    fn gen(r: &mut Rng) -> Self {
        let mut a = [0u8; 24];
        a[0] = r.next() as u8;
        a[23] = r.next() as u8;
        a
    }
}
impl Gen for [u64; 9] {
    fn gen(r: &mut Rng) -> Self {
        let mut a = [0u64; 9];
        a[0] = r.below(100);
        a[8] = r.below(100);
        a
    }
}
impl Gen for String {
    fn gen(r: &mut Rng) -> Self {
        format!("{:03}", r.below(1000))
    }
}
impl Gen for Box<u64> {
    fn gen(r: &mut Rng) -> Self {
        Box::new(r.below(1000))
    }
}
impl Gen for () {
    fn gen(_: &mut Rng) -> Self {}
}

fn run_model<T: Gen, O: HeapOrder, const M: usize, const K: usize>(seed: u64, steps: usize) {
    let mut r = Rng(seed | 1);
    let mut h: SeqHeap<T, O, M, K> = SeqHeap::new();
    let mut m: Model<T, O> = Model::new();
    for step in 0..steps {
        match r.below(20) {
            0..=8 => {
                let x = T::gen(&mut r);
                h.push(x.clone());
                m.0.push(x);
            }
            9..=12 => assert_eq!(h.pop(), m.pop(), "step {step}"),
            13 => {
                let x = T::gen(&mut r);
                let a = h.push_pop(x.clone());
                m.0.push(x);
                assert_eq!(a, m.pop().unwrap());
            }
            14 => {
                let x = T::gen(&mut r);
                let a = h.replace(x.clone());
                let b = m.pop();
                m.0.push(x);
                assert_eq!(a, b);
            }
            15 => {
                if let Some(mut p) = h.peek_mut() {
                    let x = T::gen(&mut r);
                    *p = x.clone();
                    m.pop().unwrap();
                    m.0.push(x);
                }
                if let Some(p) = h.peek_mut() {
                    assert_eq!(Some(PeekMut::pop(p)), m.pop());
                }
            }
            16 => {
                let k = r.below(3 * M as u64 + 5) as usize;
                let xs: Vec<T> = (0..k).map(|_| T::gen(&mut r)).collect();
                if r.below(2) == 0 {
                    h.extend(xs.iter().cloned());
                } else {
                    let mut o: SeqHeap<T, O, M, K> = xs.iter().cloned().collect();
                    h.append(&mut o);
                    assert!(o.is_empty());
                    o.check_invariants();
                }
                m.0.extend(xs);
            }
            17 => {
                let pivot = T::gen(&mut r);
                h.retain(|x| *x < pivot);
                m.0.retain(|x| *x < pivot);
            }
            18 => {
                let c = h.clone();
                c.check_invariants();
                assert_eq!(c.into_sorted_vec(), m.sorted());
                let c2 = h.clone();
                let sorted: Vec<T> = c2.into_iter_sorted().collect();
                let mut expect = m.sorted();
                if !O::POPS_ASCENDING {
                    expect.reverse();
                }
                assert_eq!(sorted, expect);
            }
            _ => {
                let mut v: Vec<T> = h.clone().into_iter().collect();
                v.sort();
                assert_eq!(v, m.sorted());
                let mut v: Vec<T> = h.iter().rev().cloned().collect();
                v.sort();
                assert_eq!(v, m.sorted());
            }
        }
        agree(&h, &m);
    }
    let mut c = h.clone();
    let drained: Vec<T> = c.drain_sorted().collect();
    let mut expect = m.sorted();
    if !O::POPS_ASCENDING {
        expect.reverse();
    }
    assert_eq!(drained, expect);
    assert!(c.is_empty());
    let mut v: Vec<T> = h.drain().collect();
    v.sort();
    assert_eq!(v, m.sorted());
    assert!(h.is_empty());
    h.check_invariants();
}

macro_rules! model_tests {
    ($($name:ident: $t:ty),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                for seed in 1..5u64 {
                    run_model::<$t, Min, 4, 2>(seed, 1200);
                    run_model::<$t, Max, 8, 4>(seed * 7919, 1200);
                    run_model::<$t, Min, 1, 1>(seed * 31, 600);
                }
            }
        )*
    };
}

model_tests! {
    model_u8: u8,
    model_u32: u32,
    model_u64: u64,
    model_i64: i64,
    model_u128: u128,
    model_pair: (u64, u32),
    model_arr24: [u8; 24],
    model_arr72: [u64; 9],
    model_string: String,
    model_box: Box<u64>,
    model_zst: (),
}

#[test]
fn default_params_match_std_at_scale() {
    let mut r = Rng(5);
    let mut h = MinHeap::new();
    let mut s = BinaryHeap::new();
    for i in 0..600_000u64 {
        let x = r.next();
        h.push(x);
        s.push(Reverse(x));
        if r.below(3) == 0 {
            assert_eq!(h.pop(), s.pop().map(|r| r.0), "at {i}");
        }
        if i % 100_000 == 0 {
            assert_eq!(h.len(), s.len());
            h.check_invariants();
        }
    }
    let a = h.into_sorted_vec();
    let mut b: Vec<u64> = s.into_sorted_vec().into_iter().map(|r| r.0).collect();
    b.reverse();
    assert_eq!(a, b);

    let mut h = MaxHeap::new();
    let mut s = BinaryHeap::new();
    for _ in 0..200_000 {
        let x = (r.next(), r.next() as u32);
        h.push(x);
        s.push(x);
        if r.below(3) == 0 {
            assert_eq!(h.pop(), s.pop());
        }
    }
    assert_eq!(h.into_sorted_vec(), s.into_sorted_vec());
}

// ----------------------------------------------------------------------
// 3. Drop accounting and panic safety.
// ----------------------------------------------------------------------

#[test]
fn drops_exact() {
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    #[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
    struct D(u64);
    impl Drop for D {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    DROPS.store(0, Ordering::SeqCst);
    {
        let mut h: SeqHeap<D, Min, 4, 2> = (0..100).map(D).collect();
        for _ in 0..30 {
            h.pop();
        }
        h.retain(|d| d.0 % 3 != 0);
        h.extend((0..10).map(D));
        let _ = h.drain().take(3).count();
        let mut g: SeqHeap<D, Min, 4, 2> = (0..20).map(D).collect();
        g.push_pop(D(5));
        g.replace(D(6));
        let mut it = g.into_iter();
        it.next();
        it.next_back();
    }
    assert_eq!(DROPS.load(Ordering::SeqCst), 100 + 10 + 20 + 2);
}

#[test]
fn panicking_ord_keeps_the_heap_usable_and_drops_everything_once() {
    thread_local! { static PANIC_AFTER: Cell<usize> = const { Cell::new(usize::MAX) }; }
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    static CREATED: AtomicUsize = AtomicUsize::new(0);
    #[derive(Debug)]
    struct P(u64);
    fn p(v: u64) -> P {
        CREATED.fetch_add(1, Ordering::SeqCst);
        P(v)
    }
    impl PartialEq for P {
        fn eq(&self, o: &Self) -> bool {
            self.0 == o.0
        }
    }
    impl Eq for P {}
    impl PartialOrd for P {
        fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for P {
        fn cmp(&self, o: &Self) -> std::cmp::Ordering {
            PANIC_AFTER.with(|c| {
                let n = c.get();
                if n == 0 {
                    panic!("boom");
                }
                c.set(n.saturating_sub(1));
            });
            self.0.cmp(&o.0)
        }
    }
    impl Drop for P {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    for budget in 0..60 {
        DROPS.store(0, Ordering::SeqCst);
        CREATED.store(0, Ordering::SeqCst);
        std::panic::set_hook(Box::new(|_| {}));
        {
            PANIC_AFTER.with(|c| c.set(usize::MAX));
            let mut h: SeqHeap<P, Min, 4, 2> = (0..40).rev().map(p).collect();
            PANIC_AFTER.with(|c| c.set(budget));
            let _ = catch_unwind(AssertUnwindSafe(|| {
                h.push(p(1000));
                h.pop();
                h.push(p(3));
                h.pop();
                h.push_pop(p(5));
                h.extend((0..9).map(p));
                h.retain(|x| x.0 % 2 == 0);
                h.pop();
            }));
            PANIC_AFTER.with(|c| c.set(usize::MAX));
            let _ = std::panic::take_hook();
            // Whatever happened, the heap is still usable: every element
            // it still holds comes out, in order, and the count is right.
            let n = h.iter().count();
            let mut prev: Option<u64> = None;
            let mut popped = 0;
            while let Some(x) = h.pop() {
                if let Some(pv) = prev {
                    assert!(pv <= x.0);
                }
                prev = Some(x.0);
                popped += 1;
            }
            assert_eq!(popped, n);
        }
        assert_eq!(
            DROPS.load(Ordering::SeqCst),
            CREATED.load(Ordering::SeqCst),
            "budget {budget}: every element must be dropped exactly once"
        );
    }
}
