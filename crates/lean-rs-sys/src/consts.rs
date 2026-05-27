//! Compile-time constants resolved by `build.rs` (version + header digest)
//! plus the tag enum from `lean.h:83–95`.

/// `LEAN_VERSION_STRING` as read from the active toolchain's `version.h`
/// (e.g. `"4.29.1"`).
pub const LEAN_VERSION: &str = env!("LEAN_VERSION");

/// Version string from the matched supported-toolchain entry.
///
/// The build script resolves this from the
/// [`SupportedToolchain`](crate::SupportedToolchain) entry whose
/// `header_digest` matched at build time. Equal to one of the `versions`
/// entries in [`SUPPORTED_TOOLCHAINS`](crate::SUPPORTED_TOOLCHAINS); in
/// practice equal to [`LEAN_VERSION`], but the build accepts any version
/// whose `lean.h` digest matches a supported entry.
pub const LEAN_RESOLVED_VERSION: &str = env!("LEAN_RESOLVED_VERSION");

/// Filesystem path to the `lean.h` that the build was resolved against.
pub const LEAN_HEADER_PATH: &str = env!("LEAN_HEADER_PATH");

/// SHA-256 of the resolved `lean.h`. Computed by `build.rs` and equal to
/// the `header_digest` of the matched
/// [`SupportedToolchain`](crate::SupportedToolchain) entry.
pub const LEAN_HEADER_DIGEST: &str = env!("LEAN_HEADER_DIGEST");

// Tag constants—`lean.h:83–95`.
pub const LEAN_MAX_CTOR_TAG: u8 = 243;
pub const LEAN_PROMISE: u8 = 244;
pub const LEAN_CLOSURE: u8 = 245;
pub const LEAN_ARRAY: u8 = 246;
pub const LEAN_STRUCT_ARRAY: u8 = 247;
pub const LEAN_SCALAR_ARRAY: u8 = 248;
pub const LEAN_STRING: u8 = 249;
pub const LEAN_MPZ: u8 = 250;
pub const LEAN_THUNK: u8 = 251;
pub const LEAN_TASK: u8 = 252;
pub const LEAN_REF: u8 = 253;
pub const LEAN_EXTERNAL: u8 = 254;
pub const LEAN_RESERVED: u8 = 255;

// Object-allocator constants—`lean.h:30–32`.
pub const LEAN_CLOSURE_MAX_ARGS: usize = 16;
pub const LEAN_OBJECT_SIZE_DELTA: usize = 8;
pub const LEAN_MAX_SMALL_OBJECT_SIZE: usize = 4096;

// Constructor-shape ceilings—`lean.h:97–98`.
pub const LEAN_MAX_CTOR_FIELDS: usize = 256;
pub const LEAN_MAX_CTOR_SCALARS_SIZE: usize = 1024;
