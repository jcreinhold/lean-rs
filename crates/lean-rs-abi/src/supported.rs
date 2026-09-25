//! The supported Lean toolchain window.
//!
//! `lean-rs-abi` accepts the active toolchain at build time iff its `lean.h`
//! digest matches one entry in [`SUPPORTED_TOOLCHAINS`]. The table is the
//! single source of truth for the v1.0 compatibility promise.
//!
//! Each entry records the SHA-256 of one `include/lean/lean.h`, the
//! `LEAN_VERSION_STRING` values that ship that exact header (Lean does not
//! always bump the header between releases—header-identical releases share
//! one entry), and the set of [`REQUIRED_SYMBOLS`](crate::REQUIRED_SYMBOLS)
//! that are absent from this toolchain. Runtime layout assumptions in
//! `lean-rs-sys` are checked against this same window (see
//! `docs/architecture/02-versioning-and-compatibility.md`).
//!
//! See `docs/bump-toolchain.md` for the procedure to extend the window.

/// One ABI-equivalence class in the supported toolchain window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportedToolchain {
    /// `LEAN_VERSION_STRING` values that ship this exact header. Releases
    /// with byte-identical `lean.h` share one entry.
    pub versions: &'static [&'static str],
    /// SHA-256 of `include/lean/lean.h`, lowercase hex.
    pub header_digest: &'static str,
    /// Entries of [`crate::REQUIRED_SYMBOLS`] that are absent from this
    /// toolchain. Empty when the full surface is available.
    pub missing_symbols: &'static [&'static str],
}

impl SupportedToolchain {
    /// Return `true` iff `version` (the `LEAN_VERSION_STRING`) is one of
    /// this entry's grouped releases.
    #[must_use]
    pub fn includes(&self, version: &str) -> bool {
        self.versions.contains(&version)
    }
}

/// The supported Lean toolchain window.
///
/// Ordered by the first `versions` entry. To add a new toolchain, follow
/// the checklist in `docs/bump-toolchain.md`.
// Lower bound of the window is **4.30.0**. Releases 4.27.0–4.29.1 were
// dropped on 2026-07-28: their `libleanshared` does not export
// `_l___private_Lean_Util_CollectAxioms_0__Lean_CollectAxioms_collectAndGet___boxed`,
// which the compiled `lean-rs-host` shim dylib references through
// `Lean.collectAxioms` (in the shims since v0.1.18), so the mandatory host
// shim fails `dlopen` on those toolchains — the window claimed support the
// runtime never had. Verified by `nm -gU`: 4.29.1 exports zero
// `collectAndGet` symbols, 4.30.0 and later export four. The earlier 4.26.0
// drop (2026-07-19) was for shim *build* failures; ≤ 4.25.x is excluded by
// the refcount divergence that crashes inside `lean_dec_ref_cold`.
pub const SUPPORTED_TOOLCHAINS: &[SupportedToolchain] = &[
    SupportedToolchain {
        versions: &["4.30.0"],
        header_digest: "5a25125970f4f1dcf85a4c403463b387a8ff93535cd4a3054cafdee1759017d7",
        missing_symbols: &[],
    },
    SupportedToolchain {
        versions: &["4.31.0-rc1", "4.31.0-rc2"],
        header_digest: "99ef35d69709e38caf836cf9ebbdf94d4474801e04157b8a72622dbdc653ec87",
        missing_symbols: &[],
    },
    SupportedToolchain {
        versions: &["4.31.0"],
        header_digest: "486fe204404c0fdfb753b7e089c1c0d38fbdb396206030497696165e31218992",
        missing_symbols: &[],
    },
    SupportedToolchain {
        versions: &["4.32.0-rc1", "4.32.0", "4.32.2"],
        header_digest: "22eed50aa703c4403010fabc12a7231ffa34dc979bd59ca1bfbac13c29a1dad2",
        missing_symbols: &[],
    },
    // 4.33.0-rc1 ships a *new* `lean.h` digest, but the change is confined to
    // two C11 `_Atomic(...)` qualifiers—`m_canceled` (a `uint8_t` inside the
    // opaque `lean_task_imp`, reached only via our `*mut c_void` `imp` field)
    // and `m_imp` (a pointer in `lean_task_object`). `_Atomic(T)` for a
    // lock-free scalar/pointer has the same size and alignment as `T`, so a
    // probe against both headers reports byte-identical size, alignment, and
    // field offsets for all 10 mirrored structs. `repr.rs` is unchanged; all
    // 88 REQUIRED_SYMBOLS resolve. Added 2026-07-19 as the new head.
    SupportedToolchain {
        versions: &["4.33.0-rc1", "4.33.0-rc2", "4.33.0"],
        header_digest: "9018878554c5552ff3754865780d21825c2d0c5c4b47491b37bf6fe046adcd56",
        missing_symbols: &[],
    },
    // 4.33.1 ships a *new* `lean.h` digest: it backports the TSan
    // instrumentation that 4.34.0-rc1 introduced (`#define LEAN_TSAN` guards
    // and the `lean_internal_*_rc` static-inline helpers) and documents the
    // sticky-rc band in comments. No struct declaration changes, so the
    // probe against the 4.33.0 header reports byte-identical size,
    // alignment, and field offsets for all 10 mirrored structs. `repr.rs`
    // is unchanged; all 88 REQUIRED_SYMBOLS resolve. Added 2026-08-15.
    SupportedToolchain {
        versions: &["4.33.1"],
        header_digest: "02af0040283143b264e3a44b06b348b6973ae6037fa8f550701c83dd7a062ace",
        missing_symbols: &[],
    },
    // 4.34.0-rc1 ships a *new* `lean.h` digest, but the change is confined to
    // TSan instrumentation: `#define LEAN_TSAN` guards and three
    // `lean_internal_*_rc` static-inline helpers that read/write `m_rc`
    // through seq-cst atomics only when compiled under ThreadSanitizer. No
    // struct declaration changes, so the probe against both headers reports
    // byte-identical size, alignment, and field offsets for all 10 mirrored
    // structs. `repr.rs` is unchanged; all 88 REQUIRED_SYMBOLS resolve.
    // Added 2026-08-11 as the new head.
    SupportedToolchain {
        versions: &["4.34.0-rc1"],
        header_digest: "19510ea01b07c55bd49066566e586179fe77f11120eeab7a29da50fa93cb1c8a",
        missing_symbols: &[],
    },
    // 4.34.0-rc2 ships a *new* `lean.h` digest implementing the sticky-rc
    // band it previously only documented: `LEAN_RC_STICKY` /
    // `LEAN_RC_STICKY_DROP` / `LEAN_RC_INC_MAX` macros, unsigned wrap-around
    // arithmetic in `lean_inc_ref_n`, a new cold-path helper
    // `lean_inc_ref_huge_n`, and a new `lean_nat_size_in_bytes` export.
    // No struct declaration changes, so the probe against the 4.34.0-rc1
    // header reports byte-identical size, alignment, and field offsets for
    // all 10 mirrored structs. `repr.rs` is unchanged; all 88
    // REQUIRED_SYMBOLS resolve (the two new exports are additive and not
    // part of the required surface). Added 2026-08-15 as the new head. The
    // final 4.34.0 ships a byte-identical header and joined on 2026-09-24.
    SupportedToolchain {
        versions: &["4.34.0-rc2", "4.34.0"],
        header_digest: "982c731a1fbacc7688006f44f9f7af9d9e3e0247d310a728827f49e7f1466c13",
        missing_symbols: &[],
    },
    // 4.34.1 ships a *new* `lean.h` digest. It factors the sticky-rc test in
    // `lean_inc_ref_n` into a `lean_is_unstuck_mt` inline (the same unsigned
    // comparison), adds a `lean_is_never_freed` inline, and documents
    // `LEAN_RC_STUCK_ST`, where the runtime's `lean_inc_ref_huge_n` freezes an
    // overflowing single-threaded count. The inline refcount paths
    // `lean-rs-sys` mirrors behave identically; layouts are byte-identical and
    // all 88 REQUIRED_SYMBOLS resolve. Added 2026-09-25.
    SupportedToolchain {
        versions: &["4.34.1"],
        header_digest: "866d80c309a9945b401e61fcf335a3b8545fb75c8b0a8b721fb1d3d3143d5d9c",
        missing_symbols: &[],
    },
    // 4.35.0-rc1 ships a *new* `lean.h` digest. No struct declaration
    // changes, so all 10 mirrored structs keep byte-identical size,
    // alignment, and field offsets, and all 88 REQUIRED_SYMBOLS resolve. One
    // change is semantic: `LEAN_LINEAR_MARK_MASK` (0x80) claims the top bit
    // of `m_other` in arrays, scalar arrays, and strings as a linearity
    // marker set by `Array.markLinear`, and `lean_sarray_elem_size` masks it
    // out; the `lean-rs-sys` mirror masks it too. The rest is additive or
    // off our surface: the `lean_*_mark_linear` / `propagate_mark` inlines,
    // `lean_copy_sarray{,_nonlinear}` and `lean_copy_string{,_nonlinear}`
    // replacing `lean_copy_byte_array` / `lean_copy_float_array`,
    // `lean_st_ref_set` / `reset` renamed to `put` / `take`, and an exported
    // mimalloc fast path (`lean_alloc_small_object_core`) behind the inline
    // allocator—`lean-rs-sys` calls none of these. Added 2026-09-24.
    SupportedToolchain {
        versions: &["4.35.0-rc1"],
        header_digest: "322d6bd8ab2646dfe3aa781a5f02e7d63ef7f121889b450fd55a1118b1afe3dc",
        missing_symbols: &[],
    },
    // 4.35.0-rc2 ships a *new* `lean.h` digest whose only change from
    // 4.35.0-rc1 is one additive export, `lean_nat_powmod`, which is not
    // part of the required surface; layouts are byte-identical and all 88
    // REQUIRED_SYMBOLS resolve. Added 2026-09-24.
    SupportedToolchain {
        versions: &["4.35.0-rc2"],
        header_digest: "071e79d039717e51b1bd44bf0e1cdd950bc8fc2495a9e05f7aea481b42fa9926",
        missing_symbols: &[],
    },
    // 4.35.0-rc3 ships a *new* `lean.h` digest whose only change from
    // 4.35.0-rc2 is the refcount-helper patch 4.34.1 also carries
    // (`lean_is_unstuck_mt`, `lean_is_never_freed`, `LEAN_RC_STUCK_ST`);
    // mirrored inline behavior is unchanged, layouts are byte-identical, and
    // all 88 REQUIRED_SYMBOLS resolve. Added 2026-09-25 as the new head.
    SupportedToolchain {
        versions: &["4.35.0-rc3"],
        header_digest: "eb951e1171d39b828e7ad60aed0b90759598b678bfe58cf85c65a1559c021bf9",
        missing_symbols: &[],
    },
];

/// Return the [`SupportedToolchain`] entry that includes `version`, if any.
#[must_use]
pub fn supported_for(version: &str) -> Option<&'static SupportedToolchain> {
    SUPPORTED_TOOLCHAINS.iter().find(|t| t.includes(version))
}

/// Return the [`SupportedToolchain`] entry whose `header_digest` matches the
/// given lowercase-hex SHA-256 string, if any.
#[must_use]
pub fn supported_by_digest(digest: &str) -> Option<&'static SupportedToolchain> {
    SUPPORTED_TOOLCHAINS.iter().find(|t| t.header_digest == digest)
}

/// Return `true` iff no [`SupportedToolchain`] entry lists `symbol` under
/// `missing_symbols`. Combine with [`crate::REQUIRED_SYMBOLS`] for a
/// membership check via [`crate::symbol_in_all`].
#[must_use]
pub fn symbol_present_in_window(symbol: &str) -> bool {
    SUPPORTED_TOOLCHAINS
        .iter()
        .all(|t| !t.missing_symbols.contains(&symbol))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SemVer` precedence key for a Lean version string: numeric release
    /// core (e.g. `4.31.0`) first, then a flag that ranks a final release
    /// *after* its pre-releases (`false` for `-rcN`, `true` for a final),
    /// then the pre-release identifier. Tuple `Ord` composes these in the
    /// right priority. The naive `&str` comparison gets the rc/final pair
    /// backwards—`"4.31.0" < "4.31.0-rc1"` lexically—so the ordering
    /// invariant compares these keys instead (`SemVer` §11).
    fn precedence_key(version: &str) -> (Vec<u64>, bool, &str) {
        let (core, pre) = match version.split_once('-') {
            Some((core, pre)) => (core, pre),
            None => (version, ""),
        };
        let core_nums = core.split('.').map(|n| n.parse().unwrap_or(0)).collect();
        (core_nums, pre.is_empty(), pre)
    }

    #[test]
    fn window_is_non_empty_and_ordered_by_first_version() {
        assert!(!SUPPORTED_TOOLCHAINS.is_empty());
        for w in SUPPORTED_TOOLCHAINS.windows(2) {
            let (Some(prev), Some(next)) = (w.first(), w.get(1)) else {
                continue;
            };
            let (Some(a), Some(b)) = (prev.versions.first(), next.versions.first()) else {
                continue;
            };
            assert!(
                precedence_key(a) < precedence_key(b),
                "SUPPORTED_TOOLCHAINS must be sorted ascending by first version: {a} >= {b}",
            );
        }
    }

    #[test]
    fn every_entry_lists_at_least_one_version() {
        for t in SUPPORTED_TOOLCHAINS {
            assert!(
                !t.versions.is_empty(),
                "entry with digest {} has no versions",
                t.header_digest
            );
        }
    }

    #[test]
    fn digests_are_distinct() {
        for (i, a) in SUPPORTED_TOOLCHAINS.iter().enumerate() {
            let Some(rest) = SUPPORTED_TOOLCHAINS.get(i + 1..) else {
                continue;
            };
            for b in rest {
                assert_ne!(
                    a.header_digest, b.header_digest,
                    "{:?} and {:?} share a header digest \u{2014} merge their `versions` arrays",
                    a.versions, b.versions,
                );
            }
        }
    }

    #[test]
    fn versions_are_distinct_across_entries() {
        let mut seen: Vec<&str> = Vec::new();
        for t in SUPPORTED_TOOLCHAINS {
            for &v in t.versions {
                assert!(
                    !seen.contains(&v),
                    "version {v} appears in more than one SupportedToolchain entry",
                );
                seen.push(v);
            }
        }
    }

    #[test]
    fn digests_are_64_lowercase_hex() {
        for t in SUPPORTED_TOOLCHAINS {
            assert_eq!(
                t.header_digest.len(),
                64,
                "entry for {:?}: digest is not 64 chars",
                t.versions,
            );
            assert!(
                t.header_digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "entry for {:?}: digest is not lowercase hex",
                t.versions,
            );
        }
    }

    #[test]
    fn lookups_round_trip() {
        for t in SUPPORTED_TOOLCHAINS {
            for &v in t.versions {
                assert_eq!(supported_for(v), Some(t));
            }
            assert_eq!(supported_by_digest(t.header_digest), Some(t));
        }
        assert!(supported_for("0.0.0").is_none());
        assert!(supported_by_digest("0").is_none());
    }

    #[test]
    fn fully_present_symbols_pass_window_check() {
        for &s in crate::REQUIRED_SYMBOLS {
            assert!(symbol_present_in_window(s), "{s} should be in all supported toolchains");
        }
    }

    #[test]
    fn unknown_symbol_passes_window_check() {
        // No entry can possibly list an unknown symbol under missing_symbols,
        // so the window-only check trivially passes; the membership check
        // (`crate::symbol_in_all`) is what catches non-required symbols.
        assert!(symbol_present_in_window("lean_does_not_exist_zzz"));
    }
}
