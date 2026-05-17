//! Link-time probe: take the address of every symbol in [`REQUIRED_SYMBOLS`].
//! If `libleanshared` is missing any of them the test binary fails to link.

use lean_rs_sys::REQUIRED_SYMBOLS;

#[test]
fn all_required_symbols_resolve_at_link_time() {
    let addresses: &[(*const (), &str)] = &[
        // init / runtime
        (
            lean_rs_sys::init::lean_initialize as *const (),
            "lean_initialize",
        ),
        (
            lean_rs_sys::init::lean_initialize_runtime_module as *const (),
            "lean_initialize_runtime_module",
        ),
        (
            lean_rs_sys::init::lean_initialize_thread as *const (),
            "lean_initialize_thread",
        ),
        (
            lean_rs_sys::init::lean_finalize_thread as *const (),
            "lean_finalize_thread",
        ),
        (
            lean_rs_sys::init::lean_setup_args as *const (),
            "lean_setup_args",
        ),
        (
            lean_rs_sys::init::lean_init_task_manager as *const (),
            "lean_init_task_manager",
        ),
        (
            lean_rs_sys::init::lean_init_task_manager_using as *const (),
            "lean_init_task_manager_using",
        ),
        (
            lean_rs_sys::init::lean_finalize_task_manager as *const (),
            "lean_finalize_task_manager",
        ),
        // refcount + marking
        (
            lean_rs_sys::refcount::lean_dec_ref_cold as *const (),
            "lean_dec_ref_cold",
        ),
        (
            lean_rs_sys::refcount::lean_mark_mt as *const (),
            "lean_mark_mt",
        ),
        (
            lean_rs_sys::refcount::lean_mark_persistent as *const (),
            "lean_mark_persistent",
        ),
        // object / allocators
        (
            lean_rs_sys::object::lean_alloc_object as *const (),
            "lean_alloc_object",
        ),
        (
            lean_rs_sys::object::lean_free_object as *const (),
            "lean_free_object",
        ),
        (
            lean_rs_sys::object::lean_object_byte_size as *const (),
            "lean_object_byte_size",
        ),
        (
            lean_rs_sys::object::lean_object_data_byte_size as *const (),
            "lean_object_data_byte_size",
        ),
        // arrays
        (
            lean_rs_sys::array::lean_array_mk as *const (),
            "lean_array_mk",
        ),
        (
            lean_rs_sys::array::lean_array_push as *const (),
            "lean_array_push",
        ),
        (
            lean_rs_sys::array::lean_array_to_list as *const (),
            "lean_array_to_list",
        ),
        (
            lean_rs_sys::array::lean_array_get_panic as *const (),
            "lean_array_get_panic",
        ),
        (
            lean_rs_sys::array::lean_array_set_panic as *const (),
            "lean_array_set_panic",
        ),
        // strings
        (
            lean_rs_sys::string::lean_mk_string as *const (),
            "lean_mk_string",
        ),
        (
            lean_rs_sys::string::lean_mk_string_unchecked as *const (),
            "lean_mk_string_unchecked",
        ),
        (
            lean_rs_sys::string::lean_mk_string_from_bytes as *const (),
            "lean_mk_string_from_bytes",
        ),
        (
            lean_rs_sys::string::lean_mk_string_from_bytes_unchecked as *const (),
            "lean_mk_string_from_bytes_unchecked",
        ),
        (
            lean_rs_sys::string::lean_mk_ascii_string_unchecked as *const (),
            "lean_mk_ascii_string_unchecked",
        ),
        (
            lean_rs_sys::string::lean_string_push as *const (),
            "lean_string_push",
        ),
        (
            lean_rs_sys::string::lean_string_append as *const (),
            "lean_string_append",
        ),
        (
            lean_rs_sys::string::lean_string_mk as *const (),
            "lean_string_mk",
        ),
        (
            lean_rs_sys::string::lean_string_data as *const (),
            "lean_string_data",
        ),
        (
            lean_rs_sys::string::lean_string_utf8_get as *const (),
            "lean_string_utf8_get",
        ),
        (
            lean_rs_sys::string::lean_string_utf8_next as *const (),
            "lean_string_utf8_next",
        ),
        (
            lean_rs_sys::string::lean_string_utf8_prev as *const (),
            "lean_string_utf8_prev",
        ),
        (
            lean_rs_sys::string::lean_string_utf8_set as *const (),
            "lean_string_utf8_set",
        ),
        (
            lean_rs_sys::string::lean_string_utf8_extract as *const (),
            "lean_string_utf8_extract",
        ),
        (
            lean_rs_sys::string::lean_string_eq_cold as *const (),
            "lean_string_eq_cold",
        ),
        (
            lean_rs_sys::string::lean_string_lt as *const (),
            "lean_string_lt",
        ),
        (
            lean_rs_sys::string::lean_string_hash as *const (),
            "lean_string_hash",
        ),
        (
            lean_rs_sys::string::lean_utf8_strlen as *const (),
            "lean_utf8_strlen",
        ),
        (
            lean_rs_sys::string::lean_utf8_n_strlen as *const (),
            "lean_utf8_n_strlen",
        ),
        // Nat bignum
        (
            lean_rs_sys::nat_int::lean_nat_big_succ as *const (),
            "lean_nat_big_succ",
        ),
        (
            lean_rs_sys::nat_int::lean_nat_big_add as *const (),
            "lean_nat_big_add",
        ),
        (
            lean_rs_sys::nat_int::lean_nat_big_sub as *const (),
            "lean_nat_big_sub",
        ),
        (
            lean_rs_sys::nat_int::lean_nat_big_mul as *const (),
            "lean_nat_big_mul",
        ),
        (
            lean_rs_sys::nat_int::lean_nat_big_div as *const (),
            "lean_nat_big_div",
        ),
        (
            lean_rs_sys::nat_int::lean_nat_big_mod as *const (),
            "lean_nat_big_mod",
        ),
        (
            lean_rs_sys::nat_int::lean_nat_big_eq as *const (),
            "lean_nat_big_eq",
        ),
        (
            lean_rs_sys::nat_int::lean_nat_big_le as *const (),
            "lean_nat_big_le",
        ),
        (
            lean_rs_sys::nat_int::lean_nat_big_lt as *const (),
            "lean_nat_big_lt",
        ),
        (
            lean_rs_sys::nat_int::lean_nat_overflow_mul as *const (),
            "lean_nat_overflow_mul",
        ),
        // Int bignum
        (
            lean_rs_sys::nat_int::lean_int_big_neg as *const (),
            "lean_int_big_neg",
        ),
        (
            lean_rs_sys::nat_int::lean_int_big_add as *const (),
            "lean_int_big_add",
        ),
        (
            lean_rs_sys::nat_int::lean_int_big_sub as *const (),
            "lean_int_big_sub",
        ),
        (
            lean_rs_sys::nat_int::lean_int_big_mul as *const (),
            "lean_int_big_mul",
        ),
        (
            lean_rs_sys::nat_int::lean_int_big_div as *const (),
            "lean_int_big_div",
        ),
        (
            lean_rs_sys::nat_int::lean_int_big_mod as *const (),
            "lean_int_big_mod",
        ),
        (
            lean_rs_sys::nat_int::lean_int_big_eq as *const (),
            "lean_int_big_eq",
        ),
        (
            lean_rs_sys::nat_int::lean_int_big_le as *const (),
            "lean_int_big_le",
        ),
        (
            lean_rs_sys::nat_int::lean_int_big_lt as *const (),
            "lean_int_big_lt",
        ),
        (
            lean_rs_sys::nat_int::lean_int_big_nonneg as *const (),
            "lean_int_big_nonneg",
        ),
        // Widening
        (
            lean_rs_sys::nat_int::lean_big_usize_to_nat as *const (),
            "lean_big_usize_to_nat",
        ),
        (
            lean_rs_sys::nat_int::lean_big_uint64_to_nat as *const (),
            "lean_big_uint64_to_nat",
        ),
        (
            lean_rs_sys::nat_int::lean_cstr_to_nat as *const (),
            "lean_cstr_to_nat",
        ),
        (
            lean_rs_sys::nat_int::lean_big_int_to_int as *const (),
            "lean_big_int_to_int",
        ),
        (
            lean_rs_sys::nat_int::lean_big_size_t_to_int as *const (),
            "lean_big_size_t_to_int",
        ),
        (
            lean_rs_sys::nat_int::lean_big_int64_to_int as *const (),
            "lean_big_int64_to_int",
        ),
        (
            lean_rs_sys::nat_int::lean_cstr_to_int as *const (),
            "lean_cstr_to_int",
        ),
        (
            lean_rs_sys::nat_int::lean_uint8_of_big_nat as *const (),
            "lean_uint8_of_big_nat",
        ),
        // closure dispatch
        (
            lean_rs_sys::closure::lean_apply_1 as *const (),
            "lean_apply_1",
        ),
        (
            lean_rs_sys::closure::lean_apply_2 as *const (),
            "lean_apply_2",
        ),
        (
            lean_rs_sys::closure::lean_apply_3 as *const (),
            "lean_apply_3",
        ),
        (
            lean_rs_sys::closure::lean_apply_4 as *const (),
            "lean_apply_4",
        ),
        (
            lean_rs_sys::closure::lean_apply_5 as *const (),
            "lean_apply_5",
        ),
        (
            lean_rs_sys::closure::lean_apply_6 as *const (),
            "lean_apply_6",
        ),
        (
            lean_rs_sys::closure::lean_apply_7 as *const (),
            "lean_apply_7",
        ),
        (
            lean_rs_sys::closure::lean_apply_8 as *const (),
            "lean_apply_8",
        ),
        (
            lean_rs_sys::closure::lean_apply_9 as *const (),
            "lean_apply_9",
        ),
        (
            lean_rs_sys::closure::lean_apply_10 as *const (),
            "lean_apply_10",
        ),
        (
            lean_rs_sys::closure::lean_apply_11 as *const (),
            "lean_apply_11",
        ),
        (
            lean_rs_sys::closure::lean_apply_12 as *const (),
            "lean_apply_12",
        ),
        (
            lean_rs_sys::closure::lean_apply_13 as *const (),
            "lean_apply_13",
        ),
        (
            lean_rs_sys::closure::lean_apply_14 as *const (),
            "lean_apply_14",
        ),
        (
            lean_rs_sys::closure::lean_apply_15 as *const (),
            "lean_apply_15",
        ),
        (
            lean_rs_sys::closure::lean_apply_16 as *const (),
            "lean_apply_16",
        ),
        (
            lean_rs_sys::closure::lean_apply_n as *const (),
            "lean_apply_n",
        ),
        (
            lean_rs_sys::closure::lean_apply_m as *const (),
            "lean_apply_m",
        ),
        // IO
        (
            lean_rs_sys::io::lean_io_mark_end_initialization as *const (),
            "lean_io_mark_end_initialization",
        ),
        // external
        (
            lean_rs_sys::external::lean_register_external_class as *const (),
            "lean_register_external_class",
        ),
    ];

    // Every address must be non-null (failing to link manifests as a build
    // error, but null after relocation is also a regression worth catching).
    for (addr, name) in addresses {
        assert!(!addr.is_null(), "{name} resolved to a null address");
    }

    // The hand-maintained list and the probe list above must stay in sync;
    // catching drift here is cheaper than chasing it through the doc tools.
    assert_eq!(
        addresses.len(),
        REQUIRED_SYMBOLS.len(),
        "tests/linkage.rs probes {} symbols but REQUIRED_SYMBOLS lists {}; \
         keep them in lockstep",
        addresses.len(),
        REQUIRED_SYMBOLS.len(),
    );
}
