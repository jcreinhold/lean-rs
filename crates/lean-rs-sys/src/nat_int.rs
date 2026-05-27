//! Bignum dispatch—externs from `lean.h:1334–1853` plus the small-int
//! ceiling constants.

use crate::types::{b_lean_obj_arg, lean_obj_res, lean_object};

/// Largest `Nat` that fits in a scalar pointer (`lean.h:1336`).
/// On 64-bit hosts this is `usize::MAX >> 1`.
pub const LEAN_MAX_SMALL_NAT: usize = usize::MAX >> 1;

/// Largest scalar-encodable `Int` (`lean.h:1544`). On 64-bit hosts this is
/// `i32::MAX`; on 32-bit hosts it is `i32::MAX >> 1`.
pub const LEAN_MAX_SMALL_INT: i64 = if core::mem::size_of::<*mut ()>() == 8 {
    i32::MAX as i64
} else {
    (i32::MAX >> 1) as i64
};

/// Smallest scalar-encodable `Int` (`lean.h:1545`).
pub const LEAN_MIN_SMALL_INT: i64 = if core::mem::size_of::<*mut ()>() == 8 {
    i32::MIN as i64
} else {
    (i32::MIN >> 1) as i64
};

unsafe extern "C" {
    // Nat arithmetic (`lean.h:1338–1351`)
    pub fn lean_nat_big_succ(a: *mut lean_object) -> *mut lean_object;
    pub fn lean_nat_big_add(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_nat_big_sub(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_nat_big_mul(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_nat_overflow_mul(a1: usize, a2: usize) -> *mut lean_object;
    pub fn lean_nat_big_div(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_nat_big_div_exact(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_nat_big_mod(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_nat_big_eq(a1: *mut lean_object, a2: *mut lean_object) -> bool;
    pub fn lean_nat_big_le(a1: *mut lean_object, a2: *mut lean_object) -> bool;
    pub fn lean_nat_big_lt(a1: *mut lean_object, a2: *mut lean_object) -> bool;

    // Int arithmetic (`lean.h:1546–1558`)
    pub fn lean_int_big_neg(a: *mut lean_object) -> *mut lean_object;
    pub fn lean_int_big_add(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_int_big_sub(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_int_big_mul(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_int_big_div(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_int_big_div_exact(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_int_big_mod(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_int_big_ediv(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_int_big_emod(a1: *mut lean_object, a2: *mut lean_object) -> *mut lean_object;
    pub fn lean_int_big_eq(a1: *mut lean_object, a2: *mut lean_object) -> bool;
    pub fn lean_int_big_le(a1: *mut lean_object, a2: *mut lean_object) -> bool;
    pub fn lean_int_big_lt(a1: *mut lean_object, a2: *mut lean_object) -> bool;
    pub fn lean_int_big_nonneg(a: *mut lean_object) -> bool;

    // Widening conversions (`lean.h:1353–1355`, `lean.h:1560–1563`)
    pub fn lean_cstr_to_nat(s: *const core::ffi::c_char) -> lean_obj_res;
    pub fn lean_big_usize_to_nat(n: usize) -> lean_obj_res;
    pub fn lean_big_uint64_to_nat(n: u64) -> lean_obj_res;
    pub fn lean_cstr_to_int(s: *const core::ffi::c_char) -> lean_obj_res;
    pub fn lean_big_int_to_int(n: i32) -> lean_obj_res;
    pub fn lean_big_size_t_to_int(n: usize) -> lean_obj_res;
    pub fn lean_big_int64_to_int(n: i64) -> lean_obj_res;

    // Narrowing (`lean.h:1871`)
    pub fn lean_uint8_of_big_nat(a: b_lean_obj_arg) -> u8;
}
