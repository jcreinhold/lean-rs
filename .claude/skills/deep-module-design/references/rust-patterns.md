# Complecting Checklist: Concrete Rust Patterns

Read this when you've identified complecting in a module but aren't sure how to separate the concerns. Each entry shows
a complected pattern and its decomplected alternative.

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

**Complected:** A mutable struct where reading a value depends on when you ask.

```rust
// Complected: value and time braided
struct Counter {
    count: usize,
    last_increment: Instant,
}
impl Counter {
    fn get(&self) -> usize { self.count }  // answer depends on when you ask
    fn increment(&mut self) { ... }
}
```

**Decomplected:** Separate the value (immutable data) from the identity (the thing that changes over time).

```rust
// Decomplected: value is a snapshot, identity manages transitions
#[derive(Clone)]
struct CounterState { count: usize, last_increment: Instant }

struct Counter {
    state: CounterState,
}
impl Counter {
    fn snapshot(&self) -> CounterState { self.state.clone() }
    fn increment(&mut self) -> CounterState { ... }
}
```

The snapshot can be passed around, compared, stored — independent of time.

______________________________________________________________________

## Mechanism + Policy

**Complected:** A module that both provides a capability and decides when/how to use it.

```rust
// Complected: caching mechanism braided with eviction policy
struct Cache<K, V> {
    entries: HashMap<K, (V, Instant)>,
    max_age: Duration,  // policy baked in
}
impl<K, V> Cache<K, V> {
    fn get(&self, key: &K) -> Option<&V> {
        let (val, inserted) = self.entries.get(key)?;
        if inserted.elapsed() > self.max_age { return None; }  // policy
        Some(val)
    }
}
```

**Decomplected:** The cache provides storage; eviction policy is a separate concern.

```rust
// Decomplected: mechanism and policy separated
trait EvictionPolicy<K> {
    fn should_evict(&self, key: &K, metadata: &EntryMetadata) -> bool;
}

struct Cache<K, V, P: EvictionPolicy<K>> {
    entries: HashMap<K, (V, EntryMetadata)>,
    policy: P,
}
```

Now the cache works with any eviction strategy. The mechanism doesn't know the policy.

______________________________________________________________________

## Storage + Domain Logic

**Complected:** A type that mixes how data is stored with what it means.

```rust
// Complected: storage representation braided with domain operations
struct DefRegistry {
    defs: Vec<StoredDefInfo>,  // storage detail
}
impl DefRegistry {
    fn lookup(&self, id: DefId) -> Option<&StoredDefInfo> {
        self.defs.get(id.index())  // Vec indexing leaked
    }
    fn is_value_type(&self, id: DefId) -> bool {
        // domain logic mixed with storage access
        matches!(self.lookup(id), Some(info) if info.kind == DefKind::ValueType)
    }
}
```

**Decomplected:** Storage is an implementation detail. Domain queries go through a domain interface.

```rust
// Decomplected: storage hidden, domain interface exposed
struct DefRegistry { /* storage details private */ }

impl DefRegistry {
    // Domain interface — callers don't know about Vec, StoredDefInfo, or indexing
    fn is_value_type(&self, id: DefId) -> bool { ... }
    fn def_kind(&self, id: DefId) -> DefKind { ... }
}
```

______________________________________________________________________

## Traversal + Computation

**Complected:** A function that decides both what to compute and how to walk the structure.

```rust
// Complected: tree walking braided with the specific computation
fn count_free_vars(term: &Term) -> usize {
    match term {
        Term::Var(v) => if v.is_free() { 1 } else { 0 },
        Term::App(f, a) => count_free_vars(f) + count_free_vars(a),
        Term::Lam(body) => count_free_vars(body),
        // ... every variant spelled out
    }
}

fn collect_names(term: &Term) -> Vec<String> {
    match term {
        Term::Var(v) => vec![v.name.clone()],
        Term::App(f, a) => { /* same traversal, different computation */ }
        // ... same structure again
    }
}
```

**Decomplected:** Separate the traversal from the fold operation.

```rust
// Decomplected: traversal is generic, computation is plugged in
fn fold_term<A>(term: &Term, f: &mut impl FnMut(&Term) -> A, combine: impl Fn(A, A) -> A, empty: A) -> A {
    // traversal logic in one place
}

let free_var_count = fold_term(&term, &mut |t| matches!(t, Term::Var(v) if v.is_free()) as usize, |a, b| a + b, 0);
```

______________________________________________________________________

## Error Path + Happy Path

**Complected:** Error handling interleaved with business logic at every step.

```rust
// Complected: error checking at every line obscures the actual logic
fn process(input: &str) -> Result<Output> {
    let parsed = parse(input).map_err(|e| ProcessError::Parse(e))?;
    if !validate(&parsed) { return Err(ProcessError::Invalid); }
    let transformed = transform(parsed).map_err(|e| ProcessError::Transform(e))?;
    if transformed.is_empty() { return Err(ProcessError::Empty); }
    Ok(finalize(transformed))
}
```

**Decomplected:** Define errors out of existence where possible; batch the rest.

```rust
// Decomplected: fewer error cases, clearer logic
fn process(input: &str) -> Result<Output> {
    let parsed = parse(input)?;           // parse validates internally
    let transformed = transform(parsed);  // transform handles empty case
    Ok(finalize(transformed))
}
```

The key insight: `validate` as a separate step was unnecessary if `parse` rejects invalid input. `is_empty()` was
unnecessary if `transform` handles empty input as a normal case.

______________________________________________________________________

## Interface + Implementation Detail

**Complected:** A public API that exposes how the module works, not what it does.

```rust
// Complected: callers must know about the B-tree
pub struct Index {
    pub tree: BTreeMap<String, Vec<DocId>>,
}
impl Index {
    pub fn insert_into_tree(&mut self, key: String, doc: DocId) {
        self.tree.entry(key).or_default().push(doc);
    }
}
```

**Decomplected:** The interface describes the domain operation; the implementation is hidden.

```rust
// Decomplected: callers see "search index," not "B-tree"
pub struct Index { /* private */ }
impl Index {
    pub fn index_document(&mut self, doc: Document) { ... }
    pub fn search(&self, query: &str) -> Vec<Document> { ... }
}
```

______________________________________________________________________

## Caller Knowledge + Module Logic

**Complected:** Methods named after what a specific caller does, not what the module provides.

```rust
// Complected: the module knows about the caller's domain
impl TextBuffer {
    pub fn handle_backspace(&mut self) { ... }
    pub fn handle_paste(&mut self, text: &str) { ... }
    pub fn handle_selection_delete(&mut self, sel: Selection) { ... }
}
```

**Decomplected:** General operations that serve any caller.

```rust
// Decomplected: two methods serve every use case
impl TextBuffer {
    pub fn insert(&mut self, pos: usize, text: &str) { ... }
    pub fn delete(&mut self, range: Range<usize>) { ... }
}
```

`handle_backspace` is `delete(cursor - 1..cursor)`. `handle_paste` is `insert(cursor, text)`. `handle_selection_delete`
is `delete(sel.range())`. Three special-purpose methods become two general ones.

______________________________________________________________________

## Temporal Steps + Independent Operations

**Complected:** An API that requires methods called in a specific order, enforced only by documentation.

```rust
// Complected: ordering enforced by convention, not structure
struct Builder {
    configured: bool,
    validated: bool,
}
impl Builder {
    fn configure(&mut self, cfg: Config) { self.configured = true; ... }
    fn validate(&mut self) -> Result<()> {
        assert!(self.configured, "must configure first");  // runtime check
        self.validated = true;
        Ok(())
    }
    fn build(&self) -> Output {
        assert!(self.validated, "must validate first");  // runtime check
        ...
    }
}
```

**Decomplected:** Use typestate to make invalid orderings unrepresentable.

```rust
// Decomplected: ordering enforced by the type system
struct Unconfigured;
struct Configured;
struct Validated;

struct Builder<State> { _state: PhantomData<State>, /* ... */ }

impl Builder<Unconfigured> {
    fn configure(self, cfg: Config) -> Builder<Configured> { ... }
}
impl Builder<Configured> {
    fn validate(self) -> Result<Builder<Validated>> { ... }
}
impl Builder<Validated> {
    fn build(self) -> Output { ... }
}
```

______________________________________________________________________

## Type Family + Generic Wrapper

**Complected:** Using a generic wrapper that discards known information.

This is the most common complecting pattern in the kan codebase. When code knows it has a value type but wraps it in a
generic `OpenTerm`, it braids the specific type information with a generic container — and every downstream consumer
must re-discover what the code already knew.

```rust
// Complected: known value type wrapped in generic
fn process(term: OpenValueTerm<'tcx>) -> OpenTerm<'tcx> {
    // caller had a value term, now downstream must figure that out again
    term.into_generic()
}
```

**Decomplected:** Preserve the specific type through the pipeline.

```rust
// Decomplected: value stays value
fn process(term: OpenValueTerm<'tcx>) -> OpenValueTerm<'tcx> {
    // downstream knows exactly what it has
    ...
}
```

See the `kernel-boundary-enforcement` skill for the full set of split wrappers and when to use them.

______________________________________________________________________

## Ordering + Logic

**Complected:** Using a list or vector when the ordering is meaningless.

```rust
// Complected: Vec implies ordering matters
fn required_features() -> Vec<Feature> {
    vec![Feature::Unicode, Feature::Compression, Feature::Auth]
}
// Caller might depend on the order even though it's arbitrary
```

**Decomplected:** Use the type that matches the semantics.

```rust
// Decomplected: set means "these are required, order doesn't matter"
fn required_features() -> HashSet<Feature> {
    [Feature::Unicode, Feature::Compression, Feature::Auth].into()
}
```

If ordering truly matters, document why. If it doesn't, use a type that says so.
