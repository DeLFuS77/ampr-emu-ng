//! The p5z check of an index takes no heap memory: the runtime runs it in
//! `no_std` at module_start, with only the slice the caller lends it. A
//! counting allocator, per thread so parallel tests do not mix, shows that
//! `sort_u64` and `Index::open_p5z` allocate nothing.
use ampr_format::sort::sort_u64;
use ampr_format::zstd::Index;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

mod common;
use common::backwards;

const M_INDEX: &[u8] = include_bytes!("fixtures/p5z-multi/ampr_zstd.index");

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every call goes to `System` as it is; the count is a side note.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with`: nothing is counted while the thread's locals go away.
        let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The heap allocations `f` makes on this thread.
fn allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    f();
    ALLOCATIONS.with(Cell::get) - before
}

#[test]
fn the_counter_sees_an_allocation() {
    assert!(allocations(|| drop(std::hint::black_box(vec![1u8; 10]))) > 0);
}

#[test]
fn sort_u64_allocates_nothing() {
    let mut keys: Vec<u64> = (0..100_000u64)
        .map(|i| i.wrapping_mul(0x9e37_79b9_7f4a_7c15))
        .collect();
    assert!(!keys.is_sorted());
    assert_eq!(allocations(|| sort_u64(&mut keys)), 0);
    assert!(keys.is_sorted());
}

/// The real seven-volume index, chunks in record order, and a copy whose
/// chunks the check has to sort.
#[test]
fn open_p5z_allocates_nothing() {
    let moved = backwards(M_INDEX);
    let n = Index::open(M_INDEX).unwrap().chunk_count() as usize;
    let mut sorted = vec![0u64; n];
    for bytes in [M_INDEX, &moved[..]] {
        let mut opened = false;
        let taken = allocations(|| opened = Index::open_p5z(bytes, &mut sorted).is_ok());
        assert_eq!((taken, opened), (0, true));
    }
}
