import LeanRsFixture.Scalars
import LeanRsFixture.Strings
import LeanRsFixture.Containers
import LeanRsFixture.Effects
import LeanRsFixture.Evidence
import LeanRsFixture.Capability
import LeanRsFixture.Handles
import LeanRsFixture.Meta
import LeanRsFixture.SourceRanges

/-! Roll-up module for the `LeanRsFixture` Lake library.

    Lean source files in `fixtures/lean/LeanRsFixture/` define the
    workspace-internal test fixtures (containers, effects, scalars, …)
    that the in-tree integration and codegen tests drive.

    The 28 mandatory + 6 optional `@[export] lean_rs_host_*` shims travel
    with the `lean-rs-host` crate. The host stack builds and loads those
    bundled shims directly, then adds their `.olean` directory to the search
    path when a session imports `LeanRsHostShims.*`.

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

    The split keeps this test fixture as ordinary consumer Lean code while
    `lean-rs-host` owns its theorem-prover host shims. -/
