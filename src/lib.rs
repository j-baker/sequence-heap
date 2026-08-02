//! A cache-efficient priority queue with the API of
//! `std::collections::BinaryHeap`, no dependencies, `no_std` + `alloc`.
//! The public structure is safe Rust; the only `unsafe` is in the small,
//! private insertion buffer.
//!
//! ```
//! use sequence_heap::MinHeap;
//!
//! let mut h = MinHeap::new();
//! h.push(5u64);
//! h.push(1);
//! h.push(3);
//! assert_eq!(h.peek(), Some(&1));
//! assert_eq!(h.pop(), Some(1));
//! assert_eq!(h.into_sorted_vec(), vec![3, 5]);
//! ```
//!
//! [`MaxHeap<T>`] is a drop-in replacement for `BinaryHeap<T>`;
//! [`MinHeap<T>`] replaces `BinaryHeap<Reverse<T>>` without the wrapper.
//! Both are aliases of [`SeqHeap`], a *sequence heap* (Sanders): an in-cache
//! insertion buffer plus sorted runs merged in levels, so the cost per
//! operation is flat in the heap size (about 45 ns per pop+push on 16M
//! `u64` keys, where `BinaryHeap` takes 400; see the README for the full
//! numbers and the design).
//!
//! # Differences from `BinaryHeap`
//!
//! * There is no `as_slice`: the elements are not contiguous.
//! * `peek_mut` is implemented as pop + push, so even an unmodified peek
//!   costs both.
//! * `into_vec` and `From<Vec<T>>` are O(n) and O(n log n) respectively
//!   (the latter sorts the input into a run).
//! * The capacity family (`reserve`, `shrink_to_fit`, ...) acts on the
//!   insertion buffer; runs are allocated exactly as they are produced.
//! * Extras: [`SeqHeap::push_pop`], [`SeqHeap::replace`],
//!   [`SeqHeap::into_iter_sorted`], [`SeqHeap::drain_sorted`].
//!
//! # Panics
//!
//! Besides allocation, the only thing that can panic is the element type's
//! `Ord`.  If it does, the heap stays memory safe and usable, but the
//! elements being sorted or merged at that moment may be dropped.  With the
//! `no-panic` Cargo feature the compiler is made to prove that `pop`,
//! `peek`, `push_pop` and `replace` on `u64` keys contain no panicking path
//! at all (see `examples/no_panic_check.rs`).

#![no_std]
#![deny(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

mod buffer;
#[cfg(kani)]
mod kani_proofs;
mod order;
mod seq;

pub use order::{HeapOrder, Max, Min};
pub use seq::{
    Drain, DrainSorted, IntoIter, IntoIterSorted, Iter, MaxHeap, MinHeap, PeekMut, SeqHeap,
};
