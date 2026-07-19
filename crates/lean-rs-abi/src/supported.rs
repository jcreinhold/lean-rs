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
// Lower bound of the window is **4.26.0**, not 4.23.0. Empirical
// verification (multi-toolchain sweep on 2026-05-18) showed that Lean
// ≤ 4.25.x crashes inside `lean_dec_ref_cold` from the service layer—
// a refcount-path divergence between 4.25 and 4.26 that the current
// mirrors do not cover. Narrowing the window to the empirically green
// range (4.26.0 → current head) is the honest v0.1.0 promise; reopening
// the lower bound is its own follow-up (investigate the 4.25→4.26
// refcount divergence).
pub const SUPPORTED_TOOLCHAINS: &[SupportedToolchain] = &[
    SupportedToolchain {
        versions: &["4.26.0"],
        header_digest: "e0ea3efaccceb5b75c7e9e1ab92952c8aa85c3faee28ee949dfeb8ab428ad218",
        missing_symbols: &[],
    },
    SupportedToolchain {
        versions: &["4.27.0"],
        header_digest: "42255d180910bb063d97c87cfb2a61550009ca9ceb6f495069c56bfaa6c92e13",
        missing_symbols: &[],
    },
    SupportedToolchain {
        versions: &["4.28.0"],
        header_digest: "624726e5f1f10fd77cd95b8fe8f30389312e57c8fc98e6c2f1989289bdb5fb0e",
        missing_symbols: &[],
    },
    SupportedToolchain {
        versions: &["4.28.1"],
        header_digest: "648ecfb615ef0222cd63b5f1bbbc379a06749bc0f5f4c2eb16ffca26fd18fe81",
        missing_symbols: &[],
    },
    SupportedToolchain {
        versions: &["4.29.0"],
        header_digest: "671683950ef412474bede2c6a2b50aecf4f99bc29e1ddaf2222ee54ad4ffb91c",
        missing_symbols: &[],
    },
    SupportedToolchain {
        versions: &["4.29.1"],
        header_digest: "2e481a0dac7215eb16123eaef97298ae5a6d0bd0c28c534c2818e2d2f2a28efc",
        missing_symbols: &[],
    },
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
        versions: &["4.32.0-rc1", "4.32.0"],
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
        versions: &["4.33.0-rc1"],
        header_digest: "9018878554c5552ff3754865780d21825c2d0c5c4b47491b37bf6fe046adcd56",
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
