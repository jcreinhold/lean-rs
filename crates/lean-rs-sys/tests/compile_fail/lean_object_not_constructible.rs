//! The published `lean_object` is opaque: it is only ever held by pointer.
//! Its `_data` / `_marker` fields are private, so downstream code cannot
//! construct one with a struct literal—the only way to obtain object state is
//! through the crate's `pub unsafe fn` helpers.

fn main() {
    let _obj = lean_rs_sys::lean_object {
        _data: [],
        _marker: core::marker::PhantomData,
    };
}
