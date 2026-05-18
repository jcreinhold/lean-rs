import LeanRsFixture.Scalars
import LeanRsFixture.Strings
import LeanRsFixture.Containers
import LeanRsFixture.Effects
import LeanRsFixture.Evidence
import LeanRsFixture.Capability
import LeanRsFixture.Handles

/-! Roll-up module for the `LeanRsFixture` Lake library.

    Lean source files in `fixtures/lean/LeanRsFixture/` define the
    workspace-internal test fixtures (containers, effects, scalars, …)
    that the in-tree integration and codegen tests drive.

    The 13 mandatory + 3 optional `@[export] lean_rs_host_*` shims live
    in the sibling Lake package `lean-rs-host-shims` at
    `/lake/lean-rs-host-shims/`; the `require` line in
    `fixtures/lean/lakefile.lean` builds the shim package and places
    its `.olean` files where Lean's `importModules` can reach them at
    runtime via the search-path entry the host stack adds.

    Critically this fixture does **not** `import LeanRsHostShims`. The
    L1 tests in `crates/lean-rs/src/{module,handle}/tests.rs` open this
    fixture's dylib directly via `LeanLibrary::open` (no
    `LeanCapabilities` orchestration); if the fixture's dylib carried a
    static dependency on the shim's `initialize_*` symbols, those L1
    tests would SIGSEGV on the unresolved symbol because they don't
    pre-load the shim dylib with `RTLD_GLOBAL`. Keeping the L1 fixture
    shim-independent at link time preserves the test-isolation
    discipline; L2 tests reach the shim's modules at *runtime* by
    naming them in `caps.session(&["LeanRsHostShims.Elaboration", …])`.

    The split (test-only fixtures here; capability contract in the
    shim package; runtime two-dylib load in Rust) is the hybrid layout
    documented in `docs/downstream-integration.md`. -/
