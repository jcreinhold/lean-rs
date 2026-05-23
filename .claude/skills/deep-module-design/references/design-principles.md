# Design Principles: Full Theory

Read this when you need the full reasoning behind a design decision, not just the rule. The SKILL.md gives the
principles; this file explains why they work and how to apply them in hard cases.

## Table of Contents

1. [Deep Modules (Ousterhout)](#deep-modules)
1. [Different Layer, Different Abstraction](#different-layer-different-abstraction)
1. [Better Together or Better Apart](#better-together-or-better-apart)
1. [Modifying Existing Code](#modifying-existing-code)
1. [Simple vs Easy (Hickey)](#simple-vs-easy)
1. [Information Hiding and Volatility](#information-hiding-and-volatility)
1. [Pulling Complexity Downward](#pulling-complexity-downward)
1. [Defining Errors Out of Existence](#defining-errors-out-of-existence)
1. [General-Purpose Interfaces](#general-purpose-interfaces)
1. [Strategic vs Tactical Programming](#strategic-vs-tactical-programming)
1. [Comments as a Design Tool](#comments-as-a-design-tool)
1. [The Three Symptoms of Complexity](#the-three-symptoms)

______________________________________________________________________

## Deep Modules

**Source:** John Ousterhout, _A Philosophy of Software Design_ (2018)

The central claim: complexity is the root problem of software engineering. Not performance, not features — complexity.
Everything that makes software hard to understand and modify flows from it.

A module's value is the ratio of **functionality provided** to **interface complexity**. Ousterhout visualizes this as a
rectangle: the top edge is the interface (what callers learn), and the area is the functionality (what the module does).

### Deep module: narrow interface, large area

Unix file I/O: five system calls serve every file use case. The internals — disk layout, block caching, permissions,
memory mapping, concurrent access — are hidden. The interface hasn't changed meaningfully in decades while the
implementation was rewritten many times.

Garbage collectors: enormously complex algorithms, but the interface to the programmer is effectively zero — you just
allocate.

### Shallow module: wide interface, small area

Java's buffered file reading: `new BufferedReader(new InputStreamReader(new FileInputStream(path)))`. Three classes must
be composed correctly. Each class does little; the complexity is pushed to the caller.

A `DatabaseManager` that wraps a `Connection` and delegates every call: the interface is the same as the implementation.
No information is hidden. The module is pure indirection.

### The depth test

Count the public items (methods, types, constants). Count the distinct use cases they enable. Divide use cases by public
items. Deep modules have high ratios.

If adding a public method doesn't enable at least one new use case, the module is getting shallower.

### When shallow is unavoidable

Some modules genuinely have little to hide: a small data type, a thin adapter for an external API. That's fine. The test
is whether the module is shallow _relative to the complexity it could hide_. A `Point { x: f64, y: f64 }` is shallow and
should be — there's no hidden complexity. A `FileManager` that delegates to `File` is shallow and shouldn't be — it
should either hide real complexity or not exist.

### The cost is the interface, not the implementation

Two related primary-text claims:

- §4.4: "the cost of a module (in terms of system complexity) is its interface."
- Summary principle 6: "It's more important for a module to have a simple interface than a simple implementation."

A class with a complex interface is a burden even if its implementation is small, because every caller pays for it; a
class with a simple interface is valuable even if its implementation is large, because the implementation is written
once. This is the asymmetry that justifies pulling complexity downward and that explains why removing a public item is
almost never neutral — interface removed is cost paid back to every caller, every future change.

### Classitis

Ousterhout (ch 4): the "classes are good, more classes are better" trap. Splitting a system into many small classes
makes each one shallow; the cost (interface burden, indirection, scattering of related behaviour) is paid at every
boundary. The fix is not "fewer classes" but "boundaries that hide something worth hiding." If you can't say what a
small class (or Lean structure, or namespace) hides, fold it into its caller.

______________________________________________________________________

## Different Layer, Different Abstraction

**Source:** Ousterhout, ch 7

If two adjacent layers expose the same abstraction, one is unnecessary. The diagnostic case is the **pass-through
method**: a public method that does nothing but forward arguments to a method of the same name on an inner field. Each
pass-through charges interface complexity at every layer without paying it back in hidden implementation.

The canonical anti-example is Java's I/O hierarchy: composing a buffered file reader requires three classes
(`FileInputStream`, `InputStreamReader`, `BufferedReader`), each a thin decorator with many pass-through methods. Common
usage requires the caller to assemble the chain correctly. The lesson is not "don't use layers" but "if you add a layer,
it must add abstraction" — buffering, transcoding, retry policy, format translation. A wrapper that only renames or
rebrands is shallow.

The same reasoning applies to **pass-through variables**: arguments threaded through several function signatures only to
reach the bottom. Every intermediate function now "knows" about a parameter it doesn't use. Fix with a context object,
by storing the value in shared state, or by changing the boundary so the parameter doesn't have to traverse it.

______________________________________________________________________

## Better Together or Better Apart

**Source:** Ousterhout, ch 9

Two pieces of code should be **combined** when they share information that has to be threaded through both, when
combining lets you delete duplication, or when the combined interface is narrower than the sum of the parts. They should
be **kept apart** when one concern is general-purpose and the other is special-purpose, when they can change
independently, or when the combined module can no longer be described in one sentence.

Ousterhout's example: an insertion cursor and a selection in a text editor look like they belong together — both manage
text positions — but their concerns change independently (cursor moves with keyboard navigation, selection with mouse
drags or keyboard shortcuts), so separation is cleaner. The reverse case: lexing and parsing share representation and
sequencing constraints; splitting them mechanically often produces a fragile interface that has to leak both sides.

The trap to avoid is **decomposition by surface similarity** — splitting things into separate files or classes because
they "feel related" or because "modules are good." This produces _premature modularity_: false boundaries that every
interesting operation has to cross. The corrective question is not "are these related?" but "what _information_ do they
share, and does the boundary force that information to be threaded across it?"

______________________________________________________________________

## Modifying Existing Code

**Source:** Ousterhout, ch 16

The strategic-vs-tactical distinction (above) compounds across every change you make to existing code. The standing rule
(Ousterhout, p. 101): _"if you're not making the design better, you are probably making it worse."_ Every touch is an
opportunity for incremental design improvement: a clearer name, a tighter boundary, a parameter pulled inside, an error
variant defined out of existence. The opportunity is not free — refactoring takes time — but the cost of accumulated
tactical decisions is higher.

The rule for an LLM agent: when touching a file to fix or extend something, finish the task first, then _before
submitting_ spend a small budget improving the design of what you touched. Not a rewrite — a targeted improvement at the
boundary you touched. If the diff for the design improvement would be larger than the diff for the original change, stop
and propose the refactor as a separate change.

______________________________________________________________________

## Simple vs Easy

**Source:** Rich Hickey, "Simple Made Easy" (Strange Loop, 2011)

Hickey makes a precise distinction between two words programmers use interchangeably:

**Simple** (from Latin _sim-plex_, "one fold"): A thing has one role, one task, one concept, not interleaved with other
things. Simplicity is about the absence of braiding — concerns are either tangled or they aren't. This is **objective**:
you can inspect a design and determine whether concerns are interleaved.

**Easy** (from a root meaning "to lie near"): A thing is familiar, installed, at hand. It resembles what you already
know. This is **relative**: what's easy depends on who you are and what you've worked with.

### Why the distinction matters

Programmers systematically reach for what's familiar and call it "simple." But familiarity doesn't reduce entanglement.
A complex framework you've used for five years is still complex — you've just memorized its complections.

When someone says "this approach is simpler," ask: fewer braided concerns, or more familiar? These are different claims
with different consequences.

### Complecting

Hickey resurrects the verb "complect" — to interleave, entwine, braid together. Complecting is the mechanism that turns
simple things into complex things. Every time you fold two independent concerns into one mechanism, you complect them.

The metaphor: a knitted castle vs a Lego castle. The knitted castle can't be changed without unraveling. The Lego castle
is composed of independent pieces you rearrange.

### The guardrail argument

Tests and types do not fix complecting. They catch errors after the fact. A complex system with 100% test coverage is
still complex — you still can't reason about it when you need to change it.

> "Who drives their car around banging against the guardrails saying 'I'm glad I have these'?"

Tests and types are necessary safety nets, not substitutes for simplicity. Invest in simplicity first, then add
guardrails on top.

### Reliability requires simplicity

Humans hold 3-5 things in working memory. Each complection forces related concerns into that space together. As
complections stack, the combinatorial burden grows until no one can hold the whole picture.

> "How can we possibly make things that are reliable that we don't understand?"

The velocity curve: choosing easy tools gives fast initial progress, but complexity accumulates. Each iteration
accomplishes less. Choosing simple tools has a slower start (you must learn unfamiliar things), but the curve stays flat
because you can still reason about the system as it grows.

______________________________________________________________________

## Information Hiding and Volatility

**Sources:** Ousterhout (2018), Parnas (1972)

### Parnas's original insight

Don't organize modules by execution step (parse, validate, transform, save). Organize by **what changes**. Each volatile
decision becomes a module boundary.

If you organize by step, every requirement change ripples through all steps. If you organize by volatile decision ("hide
the file format," "hide the validation rules," "hide the storage mechanism"), changes stay local.

### The volatility heuristic

For each module boundary, ask: what decisions are hidden inside? If the answer is "none — callers must know everything,"
the boundary is in the wrong place.

Good boundary markers:

- Storage format or location
- Caching strategy
- Optimization technique
- Concurrency approach
- External API contract
- Error recovery policy

Bad boundary markers:

- Current execution order
- Current data flow direction
- "It felt like it should be separate"

### Information leakage

When the same design decision appears in multiple modules, changing that decision forces changes in all of them. This is
the primary source of coupling.

**Pass-through variables** are a common symptom: an intermediate function accepts a parameter it never uses, only
forwarding it to a callee. Every function in the chain now "knows" about that parameter. Fix with context objects or by
adding state to an already-shared object.

**Pass-through methods** are another symptom: a class delegates to an inner class with the same signature. No
information is hidden. Either merge the classes or push real complexity into the outer one.

______________________________________________________________________

## Pulling Complexity Downward

**Source:** Ousterhout (2018)

> "Most modules have more users than developers, so it is better for the developers to suffer than the users."

When complexity must exist somewhere, push it into the module's implementation rather than its interface. The
implementation is written once; the interface is used by every caller, every time.

### Configuration parameters

A network protocol that exposes a "retry interval" parameter forces every caller to understand retry behavior and pick a
value. Better: measure response times at runtime and compute a reasonable retry interval automatically.

Every parameter eliminated is cognitive load removed from every caller. Before adding one, ask: "Can the module compute
a reasonable default?" If yes, do it internally.

### The caveat

This works only when it reduces total system complexity. If pulling complexity down makes the implementation
unmaintainably convoluted, you've relocated the problem, not solved it. The goal is net reduction.

When to expose complexity: if callers genuinely need the option — different callers legitimately need different behavior
that can't be inferred — then a parameter is appropriate.

______________________________________________________________________

## Defining Errors Out of Existence

**Source:** Ousterhout (2018)

Exception handling is one of the worst complexity sources. Every exception creates a code path that's hard to test and
easy to get wrong. Most exception-handling code is itself buggy because it runs rarely.

### The strategy

Redefine the operation's semantics so the "error" case is handled by the normal path.

**File deletion:**

- Windows: deleting an open file raises an error. Both processes need exception handling.
- Unix: the file is marked for deletion, the call succeeds, cleanup happens when the last handle closes. Neither process
  needs exception handling.

**Deleting a variable:**

- Tcl's `unset` originally threw on nonexistent variables. Every call was wrapped in try/catch. Redefining `unset` to
  mean "ensure this doesn't exist" eliminated the exception without changing useful behavior.

### Other techniques

- **Mask at low levels:** TCP resends lost packets. NFS retries. The caller never sees transient failures.
- **Aggregate handlers:** Handle multiple exception types with a single high-level handler rather than scattering
  try/catch.

### Application in typed languages

Type systems already help: `Option`/`Result` (Rust) or `Option`/`Except` (Lean 4) make error paths explicit. But
"defining errors out of existence" still applies — before adding a new error variant, ask whether the operation could be
redefined so the case doesn't arise:

- A `remove` that returns `Ok(())` whether the key existed or not.
- A `get_or_default` (Rust) / `Option.getD` (Lean) that never fails.
- A lookup that returns an empty `Vec` / `List` / `Finset` instead of `None`. In Lean specifically, prefer
  `Finset.erase` (idempotent) over a partial deletion that errors on absent keys.

______________________________________________________________________

## General-Purpose Interfaces

**Source:** Ousterhout (2018)

This is not about building frameworks or adding features speculatively. It's about choosing interfaces that aren't tied
to one specific caller.

### The text class example

- **Special-purpose:** `backspace()`, `delete()`, `deleteSelection()` — each shallow, each encoding UI knowledge.
- **General-purpose:** `insert(position, string)` and `delete(range)` — two methods that handle every use case with no
  UI coupling.

The general-purpose version has fewer methods, a simpler interface, better information hiding (no UI concepts leak in),
and works in contexts the designer never imagined.

### The test

> "If you reduce the number of methods in an API without reducing its overall capabilities, you are probably creating
> more general-purpose methods."

### When to specialize

When a general interface would be contorted or when the domain genuinely requires specialized operations. A `Matrix`
type should have `multiply`, not just "apply a generic bilinear form." But check first whether the specialized method
could be expressed as a composition of general ones.

______________________________________________________________________

## Strategic vs Tactical Programming

**Source:** Ousterhout (2018)

**Tactical:** goal is "get this working." Each shortcut seems small. Complexity is incremental — dozens of shortcuts
accumulate into a system that's expensive to change. By then, refactoring can't be scheduled because features keep
shipping.

**Strategic:** goal is a great design that also works. Invest 10-20% of time in design improvements: better naming,
simpler interfaces, cleaning up messes as you find them. The investment pays back within months.

This is not perfectionism. It's recognizing that small, continuous design investments compound. When you touch code,
leave it slightly better.

______________________________________________________________________

## Comments as a Design Tool

**Source:** Ousterhout (2018)

### Write comments first

1. Write the module-level interface comment.
1. Write interface comments and signatures for key public methods.
1. Iterate until the structure feels right.
1. Document important fields and invariants.
1. Implement, adding implementation comments as needed.

If the comment is hard to write or requires a long explanation, the design likely isn't clear — fix the design, not the
comment.

### Two levels

**Interface comments** describe what, not how. What the method does, what parameters mean (units, bounds, ownership),
what it returns, what can fail. The caller should use the interface without reading the implementation.

**Implementation comments** explain non-obvious reasoning within the code: why this approach, what invariants are
maintained, what edge cases are handled.

### Precision for low-level comments

"The current position" is vague. "The index of the next character to be processed" is useful. Specify nullability,
units, inclusive vs exclusive bounds, ownership.

______________________________________________________________________

## The Three Symptoms

**Source:** Ousterhout (2018)

Total system complexity = the complexity of each part weighted by how often developers work on it. Complexity in a
rarely-touched module barely matters. Complexity in a hot path multiplies across every developer, every day.

### Change amplification

A single logical change requires edits in many places. Look for a missing abstraction that would localize the change.

### Cognitive load

A developer must hold many facts in working memory to make one change. Look for an interface that hides those facts.
More lines of code isn't always worse — sometimes a longer but clearer implementation has lower cognitive load than a
clever short one.

### Unknown unknowns

You don't know what you don't know — no way to discover what needs changing until a bug appears. This is the worst
symptom because you can't even estimate the risk.

**Fix:** make dependencies explicit through types, interfaces, and comments. If changing module A could break module B,
either eliminate the dependency or document it so no one misses it.
