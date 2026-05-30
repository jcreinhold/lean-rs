//! `LeanObjectRepr`, the crate-private mirror of Lean's object header layout,
//! lives in a `pub(crate)` module. Downstream code cannot name it: the layout
//! knowledge stays hidden inside `lean-rs-sys`, per the safety model.

fn main() {
    let _repr: lean_rs_sys::repr::LeanObjectRepr;
}
