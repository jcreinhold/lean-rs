# Complecting Patterns: Lean 4

Read this when you've identified complecting in a Lean 4 file or namespace and aren't sure how to separate the concerns.
Each entry shows a complected pattern and its decomplected alternative.

This file mirrors `rust-patterns.md` slot-for-slot so cross-language intuition transfers cleanly. For Lean
proof-engineering discipline (search, tactics, mathlib lookup) see the `translating-proofs-to-lean4` skill — it is
orthogonal to module design and should be loaded alongside this file when the change involves both shape and proofs.

## Table of Contents

1. [State + Identity](#state--identity)
1. [Mechanism + Policy](#mechanism--policy)
1. [Storage + Domain Logic](#storage--domain-logic)
1. [Traversal + Computation](#traversal--computation)
1. [Error Path + Happy Path](#error-path--happy-path)
1. [Interface + Implementation Detail](#interface--implementation-detail)
1. [Caller Knowledge + Module Logic](#caller-knowledge--module-logic)
1. [Temporal Steps + Independent Operations](#temporal-steps--independent-operations)
1. [Type Family + Generic Wrapper](#type-family--generic-wrapper)
1. [Ordering + Logic](#ordering--logic)

______________________________________________________________________

## State + Identity

**Complected:** A mutable cell whose observed value depends on when you ask.

```lean
structure Counter where
  ref : IO.Ref Nat

def Counter.get (c : Counter) : IO Nat := c.ref.get
def Counter.incr (c : Counter) : IO Unit := c.ref.modify (· + 1)
```

The "value" is braided with the act of observing it. Logging, testing, and equational reasoning all require running
`IO`.

**Decomplected:** Separate the immutable snapshot value from the mutable identity that produces it.

```lean
structure CounterState where
  count : Nat
  deriving DecidableEq, Repr

structure Counter where
  private ref : IO.Ref CounterState

def Counter.snapshot (c : Counter) : IO CounterState := c.ref.get
def Counter.incr (c : Counter) : IO CounterState :=
  c.ref.modifyGet fun s => let s' := { s with count := s.count + 1 }; (s', s')
```

`CounterState` is plain data — comparable, printable, provable about.

______________________________________________________________________

## Mechanism + Policy

**Complected:** A typeclass that bakes both a capability and a policy decision into one instance.

```lean
class Cache (α β : Type) where
  store : α → β → IO Unit
  fetch : α → IO (Option β)
  -- policy: hard-coded TTL inside fetch implementation
```

**Decomplected:** Split mechanism (storage) from policy (eviction).

```lean
class Cache (α β : Type) where
  store : α → β → IO Unit
  fetch : α → IO (Option (β × CacheMeta))

class EvictionPolicy (α : Type) where
  shouldEvict : CacheMeta → Bool

def Cache.lookup [Cache α β] [EvictionPolicy α] (k : α) : IO (Option β) := ...
```

A new policy is a new instance, not a forked cache.

______________________________________________________________________

## Storage + Domain Logic

**Complected:** A structure whose public field is a storage container the caller must know how to index.

```lean
structure DefRegistry where
  defs : Array DefInfo

def DefRegistry.lookup (r : DefRegistry) (id : DefId) : Option DefInfo :=
  r.defs[id.toNat]?  -- Array indexing leaked
```

**Decomplected:** Hide the field with `private`; expose a domain interface.

```lean
structure DefRegistry where
  private defs : Array DefInfo

def DefRegistry.defKind (r : DefRegistry) (id : DefId) : Option DefKind := ...
def DefRegistry.isValueType (r : DefRegistry) (id : DefId) : Bool := ...
```

Switching to a `HashMap` (or a `RBMap`, or a persistent trie) is now a private refactor.

______________________________________________________________________

## Traversal + Computation

**Complected:** Each new analysis pass spells out the tree walk again.

```lean
def Term.countFreeVars : Term → Nat
  | .var v       => if v.isFree then 1 else 0
  | .app f a     => f.countFreeVars + a.countFreeVars
  | .lam b       => b.countFreeVars

def Term.collectNames : Term → List String
  | .var v       => [v.name]
  | .app f a     => f.collectNames ++ a.collectNames
  | .lam b       => b.collectNames
  -- ... same shape, different combine
```

**Decomplected:** Push traversal into one combinator and plug computation in.

```lean
def Term.fold {α} (combine : α → α → α) (empty : α) (f : Term → α) : Term → α
  | t@(.var _)   => f t
  | t@(.app a b) => combine (f t) (combine (Term.fold combine empty f a)
                                          (Term.fold combine empty f b))
  | t@(.lam b)   => combine (f t) (Term.fold combine empty f b)

def Term.countFreeVars (t : Term) : Nat :=
  t.fold (· + ·) 0 fun
    | .var v => if v.isFree then 1 else 0
    | _      => 0
```

In Lean the recursor `Term.rec` already gives this for free; the point is to use it instead of repeating the match.

______________________________________________________________________

## Error Path + Happy Path

**Complected:** `Except`/`Option` chained at every step, with checking interleaved with logic.

```lean
def process (input : String) : Except ProcessError Output := do
  let parsed ← parse input |>.mapError ProcessError.parse
  if !validate parsed then throw ProcessError.invalid
  let xformed ← transform parsed |>.mapError ProcessError.xform
  if xformed.isEmpty then throw ProcessError.empty
  pure (finalize xformed)
```

**Decomplected:** Define error cases out of existence by tightening the operations or using total functions.

```lean
def process (input : String) : Except ParseError Output := do
  let parsed ← parse input          -- parse rejects invalid inputs
  let xformed := transform parsed   -- transform handles empty as success
  pure (finalize xformed)
```

Or sharpen the type: instead of `Option α` returned from a lookup, expose `α := default` via `Option.getD`, or return a
`Finset` (empty is fine) instead of `Option (NonemptyFinset α)`.

______________________________________________________________________

## Interface + Implementation Detail

**Complected:** A `structure` whose fields name the storage container.

```lean
structure Index where
  tree : Std.RBMap String (Array DocId) compare

def Index.insertIntoTree (i : Index) (k : String) (d : DocId) : Index := ...
```

**Decomplected:** Hide the storage; expose the domain operation.

```lean
structure Index where
  private tree : Std.RBMap String (Array DocId) compare

def Index.indexDocument (i : Index) (doc : Document) : Index := ...
def Index.search       (i : Index) (q : String)    : Array Document := ...
```

______________________________________________________________________

## Caller Knowledge + Module Logic

**Complected:** Method names encode what one specific caller does.

```lean
namespace TextBuffer
  def handleBackspace        (b : TextBuffer) : TextBuffer := ...
  def handlePaste            (b : TextBuffer) (s : String) : TextBuffer := ...
  def handleSelectionDelete  (b : TextBuffer) (s : Selection) : TextBuffer := ...
end TextBuffer
```

**Decomplected:** Two general operations, every caller composes.

```lean
namespace TextBuffer
  def insert (b : TextBuffer) (pos : Nat) (s : String) : TextBuffer := ...
  def delete (b : TextBuffer) (r : Range)              : TextBuffer := ...
end TextBuffer
```

`handleBackspace` is `delete (cursor - 1, cursor)`. The UI lives at the call site.

______________________________________________________________________

## Temporal Steps + Independent Operations

**Complected:** A builder whose ordering is enforced by `decide`-time checks or `panic`-on-misuse.

```lean
structure Builder where
  configured : Bool := false
  validated  : Bool := false
  -- fields...

def Builder.validate (b : Builder) : Except String Builder :=
  if !b.configured then .error "must configure first" else
  pure { b with validated := true }
```

**Decomplected:** Index the type by its phase. Lean's dependent types make this the natural choice.

```lean
inductive BuilderPhase where
  | unconfigured | configured | validated

structure Builder : BuilderPhase → Type where
  -- fields parameterized over phase

def Builder.configure : Builder .unconfigured → Config → Builder .configured := ...
def Builder.validate  : Builder .configured   → Except String (Builder .validated) := ...
def Builder.build     : Builder .validated    → Output := ...
```

Calling `validate` before `configure` is now a type error, not a runtime one.

______________________________________________________________________

## Type Family + Generic Wrapper

**Complected:** A definition that throws away known structure by parameterizing over `Type*` when a specific bundled
object was in hand.

```lean
def process {α : Type*} (x : α) : α := ...
-- caller had a `Cat`, downstream loses category structure
```

**Decomplected:** Preserve the bundled structure through the pipeline.

```lean
def process (C : Cat) : Cat := ...
```

If genuine polymorphism is needed, parameterize over the _bundled_ type (`Cat.{u, v}`), not over `Type*` plus
reconstructed instances at every use site.

______________________________________________________________________

## Ordering + Logic

**Complected:** Returning a `List` when the caller would never use ordering.

```lean
def requiredFeatures : List Feature :=
  [.unicode, .compression, .auth]
```

**Decomplected:** Pick the type that says what you mean.

```lean
def requiredFeatures : Finset Feature :=
  {.unicode, .compression, .auth}
```

`Finset` (or `Set`) advertises that the order is irrelevant; the type system stops a caller from depending on it. Use
`List` only when sequence is part of the meaning.
