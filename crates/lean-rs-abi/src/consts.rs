//! Static `lean.h` layout constants: object tag bytes, allocator ceilings, and
//! constructor-shape limits.
//!
//! These are layout facts of the Lean C ABI, identical across the supported
//! toolchain window, so they are plain literals with no build-time probe.
//!
//! Live toolchain identity (the installed version, header path, and header
//! digest) is deliberately *not* here: resolving it requires probing an
//! installed toolchain, which a link-free metadata crate must not do. It lives
//! in `lean-toolchain` (`LEAN_VERSION`, `LEAN_HEADER_PATH`,
//! `LEAN_HEADER_DIGEST`, `LEAN_RESOLVED_VERSION`), the crate whose job is
//! toolchain discovery.

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
