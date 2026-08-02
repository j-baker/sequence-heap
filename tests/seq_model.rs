//! Model-based tests for `SeqHeap`, with tiny buffer / fan-in parameters so
//! that run flushes and level merges happen constantly.

use sequence_heap::{HeapOrder, Max, Min, MinHeap, PeekMut, SeqHeap};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

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

fn pop_model<T: Ord, O: HeapOrder>(m: &mut Vec<T>) -> Option<T> {
    if m.is_empty() {
        return None;
    }
    let i = if O::POPS_ASCENDING {
        (0..m.len()).min_by_key(|&i| &m[i]).unwrap()
    } else {
        (0..m.len()).max_by_key(|&i| &m[i]).unwrap()
    };
    Some(m.swap_remove(i))
}

fn run<T, O: HeapOrder, const M: usize, const K: usize>(
    seed: u64,
    steps: usize,
    gen: impl Fn(&mut Rng) -> T,
) where
    T: Ord + Clone + std::fmt::Debug,
{
    let mut r = Rng(seed | 1);
    let mut h: SeqHeap<T, O, M, K> = SeqHeap::new();
    let mut model: Vec<T> = Vec::new();
    for step in 0..steps {
        match r.below(20) {
            0..=9 => {
                let x = gen(&mut r);
                h.push(x.clone());
                model.push(x);
            }
            10..=14 => assert_eq!(h.pop(), pop_model::<T, O>(&mut model), "step {step}"),
            15 => {
                let x = gen(&mut r);
                let a = h.push_pop(x.clone());
                model.push(x);
                assert_eq!(a, pop_model::<T, O>(&mut model).unwrap());
            }
            16 => {
                let x = gen(&mut r);
                let a = h.replace(x.clone());
                let b = pop_model::<T, O>(&mut model);
                model.push(x);
                assert_eq!(a, b);
            }
            17 => {
                if let Some(mut p) = h.peek_mut() {
                    let x = gen(&mut r);
                    *p = x.clone();
                    pop_model::<T, O>(&mut model).unwrap();
                    model.push(x);
                } else {
                    assert!(model.is_empty());
                }
                if r.below(2) == 0 {
                    if let Some(p) = h.peek_mut() {
                        let a = PeekMut::pop(p);
                        assert_eq!(Some(a), pop_model::<T, O>(&mut model));
                    }
                }
            }
            18 => {
                let k = r.below(3 * M as u64 + 5) as usize;
                let xs: Vec<T> = (0..k).map(|_| gen(&mut r)).collect();
                if r.below(2) == 0 {
                    h.extend(xs.iter().cloned());
                } else {
                    let mut o: SeqHeap<T, O, M, K> = xs.iter().cloned().collect();
                    h.append(&mut o);
                    assert!(o.is_empty());
                }
                model.extend(xs);
            }
            _ => {
                let pivot = gen(&mut r);
                h.retain(|x| *x < pivot);
                model.retain(|x| *x < pivot);
            }
        }
        assert_eq!(h.len(), model.len(), "len at step {step}");
        let top = if O::POPS_ASCENDING {
            model.iter().min()
        } else {
            model.iter().max()
        };
        assert_eq!(h.peek(), top, "peek at step {step}");
        if step % 97 == 0 {
            let mut a: Vec<T> = h.iter().cloned().collect();
            let mut b = model.clone();
            a.sort();
            b.sort();
            assert_eq!(a, b);
            assert_eq!(h.clone().into_sorted_vec(), b);
        }
    }
    let mut v = h.into_vec();
    v.sort();
    model.sort();
    assert_eq!(v, model);
}

#[test]
fn tiny_params_u64() {
    for seed in 1..8 {
        run::<u64, Min, 4, 2>(seed, 3000, |r| r.below(500));
        run::<u64, Max, 4, 2>(seed * 31, 3000, |r| r.below(500));
        run::<u64, Min, 8, 4>(seed * 7, 4000, |r| r.below(1000));
        run::<u64, Min, 1, 1>(seed * 11, 2000, |r| r.below(100));
    }
}

#[test]
fn tiny_params_string_and_pairs() {
    for seed in 1..4 {
        run::<String, Min, 8, 4>(seed, 2000, |r| format!("{:03}", r.below(300)));
        run::<(u64, u32), Max, 4, 4>(seed, 2000, |r| (r.below(50), r.below(50) as u32));
    }
}

#[test]
fn default_params_match_std() {
    let mut r = Rng(5);
    let mut h = MinHeap::new();
    let mut s = BinaryHeap::new();
    for i in 0..400_000u64 {
        let x = r.next();
        h.push(x);
        s.push(Reverse(x));
        if r.below(3) == 0 {
            assert_eq!(h.pop(), s.pop().map(|r| r.0), "at {i}");
        }
        if i % 50_000 == 0 {
            assert_eq!(h.len(), s.len());
            assert_eq!(h.peek(), s.peek().map(|r| &r.0));
        }
    }
    let a = h.into_sorted_vec();
    let mut b: Vec<u64> = s.into_sorted_vec().into_iter().map(|r| r.0).collect();
    b.reverse();
    assert_eq!(a, b);
}

#[test]
fn drops_exact() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    #[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
    struct D(u64);
    impl Drop for D {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
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
        let _ = g.into_sorted_vec();
    }
    assert_eq!(DROPS.load(Ordering::SeqCst), 100 + 10 + 20 + 2);
}

#[test]
fn binary_heap_surface() {
    // Everything `BinaryHeap` offers (bar `as_slice`), on the sequence heap.
    let mut h: SeqHeap<u32, Min, 8, 4> = SeqHeap::from([5u32, 1, 4]);
    assert_eq!(h.len(), 3);
    assert!(h.capacity() >= 3);
    h.reserve(100);
    h.reserve_exact(10);
    assert!(h.try_reserve(10).is_ok());
    assert!(h.try_reserve_exact(10).is_ok());
    h.shrink_to_fit();
    h.shrink_to(2);
    h.extend(&[9u32, 0]);
    h.extend((100..200).map(|x| x as u32)); // > M: filed as a run
    assert_eq!(h.len(), 105);
    assert_eq!(h.peek(), Some(&0));
    let n = h.iter().len();
    assert_eq!(n, 105);
    let mut fwd: Vec<u32> = h.iter().copied().collect();
    let mut rev: Vec<u32> = h.iter().rev().copied().collect();
    fwd.sort();
    rev.sort();
    assert_eq!(fwd, rev);
    assert_eq!((&h).into_iter().count(), 105);
    let mut own: Vec<u32> = h.clone().into_iter().rev().collect();
    own.sort();
    assert_eq!(own, fwd);
    let v: Vec<u32> = h.clone().into();
    assert_eq!(v.len(), 105);
    assert_eq!(
        h.clone().into_iter_sorted().take(4).collect::<Vec<_>>(),
        vec![0, 1, 4, 5]
    );
    let mut c = h.clone();
    {
        let mut d = c.drain_sorted();
        assert_eq!(d.len(), 105);
        assert_eq!(d.next(), Some(0));
    }
    assert!(c.is_empty());
    let mut c = h.clone();
    assert_eq!(c.drain().len(), 105);
    assert!(c.is_empty());
    let p = h.peek_mut().unwrap();
    assert_eq!(format!("{p:?}"), "PeekMut(0)");
    assert_eq!(PeekMut::pop(p), 0);
    assert_eq!(h.into_sorted_vec()[..3], [1, 4, 5]);
}
