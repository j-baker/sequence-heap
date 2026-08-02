//! Mirrors the `hold_uniform/String` benchmark and checks every popped value
//! against `BinaryHeap<Reverse<String>>`.
use sequence_heap::MinHeap;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

#[test]
fn string_hold_matches_std() {
    let mut x = 0x9E3779B97F4A7C15u64;
    let mut next = move || {
        x = x.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        format!("{:016x}", z ^ (z >> 31))
    };
    for &n in &[1usize, 2, 3, 7, 256, 4096] {
        let keys: Vec<String> = (0..n).map(|_| next()).collect();
        let mut h = MinHeap::from(keys.clone());
        let mut s: BinaryHeap<Reverse<String>> = keys.into_iter().map(Reverse).collect();
        for _ in 0..50_000 {
            let a = h.pop().unwrap();
            let b = s.pop().unwrap().0;
            assert_eq!(a, b);
            let k = next();
            h.push(k.clone());
            s.push(Reverse(k));
        }
        assert_eq!(h.into_sorted_vec(), {
            let mut v: Vec<String> = s.into_iter().map(|r| r.0).collect();
            v.sort();
            v
        });
    }
}
