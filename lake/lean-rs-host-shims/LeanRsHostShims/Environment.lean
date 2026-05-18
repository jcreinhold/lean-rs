import Lean

/-! Capability category: session-scoped environment queries. The Rust
    `LeanSession` looks up these seven `@[export]` symbols by name at
    `LeanCapabilities::load_capabilities` time and dispatches every query
    through the cached addresses. Path-layout knowledge (where the
    `.olean` files live for `Lean.importModules`) stays on the Rust side
    — the import shim only receives a ready-made search-path string. -/

namespace LeanRsFixture.Environment

open Lean

/-- Initialise the Lean search path and import the named modules into a
    fresh environment. The Rust caller passes the resolved
    `.lake/build/lib/lean` search-path entries (one per Lake package
    whose `.olean`s the import list may reach into — minimally the
    user's own capability package; for `lean-rs-host` consumers also
    the `lean-rs-host-shims` package). The shim does not need to know
    the Lake layout; it just unions whatever entries Rust provides. -/
@[export lean_rs_host_session_import]
def sessionImport (searchPaths : Array String) (importNames : Array String) : IO Environment := do
  let sysroot ← Lean.findSysroot
  Lean.initSearchPath sysroot (searchPaths.toList.map System.FilePath.mk)
  let imports := importNames.map fun n => { module := n.toName : Import }
  -- `loadExts := true` activates scoped environment extensions on
  -- import — including the parser extension. Without it, the
  -- imported environment has only the bootstrap-level builtin
  -- parsers (~7 trailing parsers in the `term` category), so
  -- `Lean.Elab.Frontend.process` / `Parser.runParserCategory`
  -- cannot parse declarations that use library-defined trailing
  -- operators like `+`, `=`, `∧`. With it set, the prompt-15
  -- `elaborate` / `kernel_check` shims see the full operator set
  -- the prelude defines.
  Lean.importModules imports Lean.Options.empty 0 (loadExts := true)

/-- Convert a dotted Rust string into a `Lean.Name`. Pure (no IO);
    `Lean.Name.toName` parses the dotted form (`"Foo.Bar"` ⇒
    `Name.mkStr (Name.mkStr Name.anonymous "Foo") "Bar"`). -/
@[export lean_rs_host_name_from_string]
def nameFromString (s : String) : Name :=
  s.toName

/-- Look up `name` in `env` and return its declaration, if present and
    convertible. Constants that don't round-trip to `Declaration`
    (constructors, recursors, inductive types, the `Quot` builtin) yield
    `none`; callers that need to know which kind a name is should use
    [`envDeclarationKind`] instead. -/
@[export lean_rs_host_env_query_declaration]
def envQueryDeclaration (env : Environment) (name : Name) : IO (Option Declaration) := do
  match env.find? name with
  | none    => pure none
  | some ci =>
    match ci with
    | .axiomInfo  v => pure (some (.axiomDecl v))
    | .defnInfo   v => pure (some (.defnDecl v))
    | .thmInfo    v => pure (some (.thmDecl v))
    | .opaqueInfo v => pure (some (.opaqueDecl v))
    | _             => pure none

@[export lean_rs_host_env_list_declarations]
def envListDeclarations (env : Environment) : IO (Array Name) := do
  pure <| env.constants.fold (init := #[]) fun acc name _ => acc.push name

@[export lean_rs_host_env_declaration_type]
def envDeclarationType (env : Environment) (name : Name) : IO (Option Expr) := do
  pure <| (env.find? name).map ConstantInfo.type

@[export lean_rs_host_env_declaration_kind]
def envDeclarationKind (env : Environment) (name : Name) : IO String := do
  match env.find? name with
  | none                 => pure "missing"
  | some (.axiomInfo  _) => pure "axiom"
  | some (.defnInfo   _) => pure "definition"
  | some (.thmInfo    _) => pure "theorem"
  | some (.opaqueInfo _) => pure "opaque"
  | some (.quotInfo   _) => pure "quot"
  | some (.inductInfo _) => pure "inductive"
  | some (.ctorInfo   _) => pure "constructor"
  | some (.recInfo    _) => pure "recursor"

@[export lean_rs_host_env_declaration_name]
def envDeclarationName (_env : Environment) (name : Name) : IO String := do
  pure name.toString

/-- Bulk variant of [`envQueryDeclaration`]: a single IO traversal that
    folds the singular lookup across `names`. One Lean traversal, one
    `MessageLog`-less FFI crossing. Iteration semantics are identical to a
    Rust-side fold over the singular path. -/
@[export lean_rs_host_env_query_declarations_bulk]
def envQueryDeclarationsBulk (env : Environment) (names : Array Name)
    : IO (Array (Option Declaration)) := do
  names.mapM (envQueryDeclaration env)

end LeanRsFixture.Environment
