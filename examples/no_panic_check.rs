//! Verification target for the `no-panic` feature.
//!
//! ```sh
//! cargo build --release --features no-panic --example no_panic_check
//! ```
//!
//! With the feature on, the hot operations of `MinHeap<u64>` carry
//! `#[no_panic]`: if the optimiser cannot eliminate every panicking path
//! from them, this binary fails to link.  A successful build is the proof.
//! The functions must actually be instantiated and optimised for the check
//! to run, hence this example.

use sequence_heap::MinHeap;

fn main() {
    let mut h: MinHeap<u64> = MinHeap::new();
    let mut acc = 0u64;
    for i in 0..100_000u64 {
        h.push(i.wrapping_mul(0x9E3779B97F4A7C15));
        if i % 3 == 0 {
            acc = acc.wrapping_add(h.pop().unwrap_or(0));
            acc = acc.wrapping_add(h.replace(i).unwrap_or(0));
            acc = acc.wrapping_add(h.push_pop(i));
            if let Some(p) = h.peek() {
                acc = acc.wrapping_add(*p);
            }
        }
    }
    println!("{acc}");
}
