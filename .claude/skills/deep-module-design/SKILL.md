---
name: deep-module-design
description: 'Use for designing Rust/Lean modules, crates, namespaces, or APIs: public/private boundaries, refactoring surface area, facade crates, information hiding, single-implementor traits.'
---

# Deep Module Design

A module's value is the ratio of what it does for callers to what callers must learn. A deep module hides significant
complexity behind a simple interface. A shallow module exposes nearly as much complexity as it contains — it moves the
problem without solving it.

This skill helps you design modules that are deep and simple (not complected), rather than shallow and merely easy
(familiar but tangled). The principles are language-neutral. Concrete patterns live in two language adapters:

- `references/rust-patterns.md` — Rust idioms.
- `references/lean4-patterns.md` — Lean 4 idioms (mirrors the Rust file slot-for-slot).

## Defaults to Resist

You are an LLM agent. The principles below are the design discipline. _This_ section is about the failure modes you
exhibit by default, because of how you were trained, that the principles alone won't fix. Read this section _before_ the
principles, and check yourself against it before submitting code.

- **Reflexive abstraction.** Asked to add a function, you make it `pub`, generic, and trait-bounded "for flexibility."
    The user asked for one caller; you shipped an extension point. _Counter:_ keep things `private` and concrete until a
    real second caller exists.
- **Pass-through generation.** Asked for a "facade" or "wrapper," you produce 1:1 method forwarding because that matches
    the shape of "facade" in your training data. _Counter:_ a wrapper that doesn't hide complexity isn't a wrapper, it's
    noise. See §7 below.
- **Over-decomposition.** You split one file into three because "modularity is good," with no information-sharing
    analysis. _Counter:_ see §8 below — the pressure to combine is real and you underweight it.
- **Speculative parameters.** You add `Option<Config>`, builder methods, feature flags for cases the user didn't ask
    for. _Counter:_ before adding a parameter, name a real caller that needs a non-default value. If you can't, don't
    add it.
- **Reflexive `Result`/`Except`.** You add error variants because the signature "feels" partial. _Counter:_ before
    adding an error case, name a caller that meaningfully recovers from it. If recovery is `unwrap` or propagation, the
    variant is a complication, not a feature. See §5.
- **Pattern-matching to canonical idioms.** "This looks like it should be a typeclass / trait / monad / builder."
    _Counter:_ the abstraction must be earned by use cases, not picked by frequency in your prior. One implementor ⇒ no
    trait. One ordering ⇒ no builder.
- **Comment inflation.** You write multi-line docstrings restating what the type signature already says. _Counter:_ doc
    comments describe _why_ (constraints, invariants, surprising behavior), not _what_.
- **Stop-when-it-compiles.** Your signal is "type checker green, tests pass." That is Ousterhout's _tactical_ default
    and it is sharper for you than for a human. _Counter:_ the design isn't done when the diff works. Run the audit
    script. Re-read the callers. Check the depth ratio.
- **Skipping the second design.** Once the first attempt compiles you commit. _Counter:_ Ousterhout's "design it twice"
    (ch 11) is the rule for hard problems — and the failure mode he names is the _smart-people trap_: people who got
    good grades on easy problems with their first idea develop the habit of trusting the first idea, even on problems
    where it isn't good enough. Your training signal — closing the goal, type-checker green — is exactly that bias
    amplified. The remedy is _substantively different_ alternatives, not a refactor of the first attempt: different
    abstraction boundary, different invariants, different decomposition. Then compare. The comparison is the value, not
    just the better design — Ousterhout: _"the process of devising and comparing multiple approaches will teach you
    about the factors that make designs better or worse."_ See "New module" below for the concrete instruction.
- **Volatile-decisions amnesia.** You guess what might change because that's what humans do. You don't have decades of
    intuition. _Counter:_ run `git log --oneline -- <path>` and read the recent diffs. Decide boundaries by what _has_
    changed, not what might.

## Orient First

Before editing, name the design pressure you face:

| Pressure                | What to look for                                                                        |
| ----------------------- | --------------------------------------------------------------------------------------- |
| **Shallow module**      | Interface nearly as complex as implementation; thin wrappers that just delegate         |
| **Leaky abstraction**   | Callers must know internal details to use the API correctly                             |
| **Complected concerns** | Two independent things braided into one type, trait/class, or module                    |
| **Growing surface**     | Public items accumulating faster than the capabilities they provide                     |
| **Temporal coupling**   | API requires callers to call methods in a specific order without structural enforcement |
| **Information loss**    | Abstraction discards information callers need, forcing them to reconstruct it           |

If the pressure is unclear, run the audit script first:
`bash .claude/skills/deep-module-design/scripts/audit-module.sh <path>`. The dispatcher selects the Rust or Lean backend
by file extension and project layout.

## Core Design Principles

These are ordered by importance. When two principles conflict, the higher one wins.

### 1. Simple means not complected

"Simple" does not mean "easy" (familiar, at hand). Simple means one concern per component — no braiding of independent
things into one mechanism. Simplicity is objective: you can inspect whether two concerns are interleaved. Ease is
relative to who you are and what you know.

When someone proposes an API "because it's simpler," ask: does it have fewer braided concerns, or is it just more
familiar? These are different claims.

**Diagnostic:** List the independent concerns the type or function handles. If two concerns can change independently and
they share one mechanism, the design is complected.

### 2. Deep modules: maximize the depth ratio

A module is a rectangle. The top edge is its interface (what callers must learn). The area is its functionality (what it
does). Deep modules have narrow tops and large areas. Shallow modules have wide tops and thin areas.

**Measure:** Count the public items (methods, types, constants). Count the distinct use cases they enable. High use-case
count divided by low public-item count = deep. The reverse = shallow.

This is why Unix file I/O is a great design: five system calls (`open`, `read`, `write`, `lseek`, `close`) serve every
file use case. The implementation (disk layout, caching, permissions, concurrency) is enormous and hidden.

### 3. Hide decisions that might change

Organize modules around volatile decisions, not around steps in the current algorithm. If you organize by "parse,
validate, transform, save," every requirement change ripples through all four. If you organize by "hide the data
format," "hide the validation rules," "hide the storage mechanism," changes stay local.

**Probe, don't guess.** You don't have human intuition for what's volatile. Replace introspection with evidence:

```bash
git log --oneline -10 -- <path>          # what has actually changed?
git log -p -5 -- <path> | head -200      # how did it change?
```

Boundaries should hide the things that have changed. If three of the last five commits all touched the same private
detail, that detail belongs behind a boundary. If a public type's signature has stayed constant while its implementation
churned, the boundary is in the right place.

### 4. Pull complexity downward

When complexity must exist, push it into the module's implementation rather than its interface. The implementation is
written once; the interface is used by every caller, every time.

Before adding a parameter, flag, or configuration option, ask: "Can I compute a reasonable default inside?" If yes, do
it internally. Every parameter eliminated is cognitive load removed from every caller.

### 5. Define errors out of existence

Before adding a new error case, ask: can I redefine the operation so the "error" case is handled by the normal path? An
`unset` that succeeds on nonexistent keys. A `delete` that succeeds on missing files. A `merge` that treats "nothing to
merge" as success.

Fewer error paths means fewer code paths means fewer places for bugs to hide.

### 6. Somewhat general-purpose, not special-purpose — and not maximal either

Seek interfaces that serve multiple use cases without encoding knowledge of any single caller. The target is _somewhat
general-purpose_, not maximally general.

The canonical case (Ousterhout, ch 6): a text class with `backspace()` and `deleteSelection()` braids UI concepts into
the data layer. Replacing them with `insert(position, string)` and `delete(range)` removes that coupling — every UI
operation is now a composition. The lesson is _not_ "fewer methods is better"; it's "don't encode caller knowledge."

The same text class parameterised over "any sequence of any element" is _over_-generalised: the abstraction has been
pushed past where any caller benefits, and now every caller pays in indirection and turbid signatures.

Ousterhout's sharper rule (ch 6.1): _"the module's functionality should reflect your current needs, but its interface
should not. Instead, the interface should be general enough to support multiple uses."_ The split matters: don't build
functionality for use cases you don't have, but do shape the interface so the class isn't tied to one particular
caller's vocabulary. The text class has functionality for editing the file in front of you, but its interface (`insert`,
`delete`, `Position`) doesn't mention the editor.

**You will over-generalise unless you stop yourself.** "For flexibility" and "in case we ever need it" are the words
that precede the bad version. The check Ousterhout offers (ch 6.5): _"Is this API easy to use for my current needs?"_ —
if you have to write a lot of additional code at the call site to use the class, you've gone too abstract.

**Test (Ousterhout):** "If you reduce the number of methods in an API without reducing its overall capabilities, you are
probably creating more general-purpose methods." Conversely, if making something more general adds methods or parameters
without enabling new use cases, you've gone past _somewhat_ into _over_.

### 7. Different layers, different abstractions

If two adjacent layers expose the same abstraction, one of them is unnecessary. The diagnostic case is the
**pass-through method**: a public method that does nothing but forward arguments to a method of the same name on an
inner field.

```rust
// Pass-through: this layer adds nothing.
impl Outer {
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}
```

The canonical anti-example (Ousterhout, ch 7) is Java's `FileInputStream` / `InputStreamReader` / `BufferedReader`
chain: each layer is a thin decorator with many pass-through methods, and common usage requires composing all three
correctly. Each layer charges interface complexity without paying it back in hidden implementation.

**Diagnostic:** if a wrapper exposes the same operation set as the thing it wraps, with the same signatures, it is
shallow. Either merge it into the inner type, or push real complexity into it (e.g., buffering, transcoding, retry
policy). The same applies to **pass-through variables**: arguments threaded through several function signatures only to
reach the bottom — a sign of a missing context object or misplaced state.

**Apply when reviewing your own code:** before submitting a wrapper, list what it adds. If the list is empty, delete the
wrapper.

**Decorators (§7.3) are the prepackaged version of this trap.** A decorator wraps an existing object and exposes a
similar API with small additions; in Java I/O the entire chain is decorators. Ousterhout: _"decorator classes tend to be
shallow: they introduce a large amount of boilerplate for a small amount of new functionality."_ Before writing a
decorator, run his four-question test:

1. Could the new functionality go directly into the underlying class? (If most users will want it, it should be the
    default — buffering belongs with `FileInputStream`.)
1. If specialised, would it merge cleanly into the use case instead of becoming its own class?
1. Could it merge with an existing decorator into a single deeper one?
1. Does it really need to _wrap_? A standalone class that doesn't re-expose the wrapped API may be cleaner.

**Internal representation should differ from the interface (§7.4).** If your class's internal representation is the same
shape as its public abstraction, the class isn't deep. Ousterhout's example: a text class storing data as lines, with
`getLine` / `putLine` methods, is shallow — every caller has to split and join lines for mid-line edits. Storing data as
lines but exposing a _character_- oriented `insert(position, …)` / `delete(start, end)` interface encapsulates the line
splitting inside, and the class is suddenly deep. The asymmetry between representation and interface _is_ the hidden
complexity.

### 8. Better together or better apart — both pressures are real

Decompose by information sharing, not by surface similarity.

The skill so far has emphasised separation. The opposite pressure is also real and you underweight it: two pieces of
code should be _combined_ when they share information, when combining simplifies the interface, or when they would
otherwise duplicate logic. Ousterhout's example (ch 9): the cursor and the selection in a text editor look like they
belong together — both manage text positions — but their concerns change independently, so separation is cleaner. The
reverse case: parsing and lexing share representation and sequencing constraints; splitting them often produces a
fragile interface that has to leak both sides.

**Combine when:**

- Two modules share information that has to be threaded through both.
- Combining lets you delete duplication.
- The combined interface is _narrower_ than the sum of the parts.

**Separate when:**

- One concern is general-purpose and the other is special-purpose.
- The two can change independently — a change in one rarely forces a change in the other.
- The combined module can't be described in one sentence.

If you split things mechanically by file or feature without doing this analysis, you produce _premature modularity_:
false boundaries that have to be crossed by every interesting operation.

## The Audit

Ask these questions in order for the module you're examining. Stop at the first "no" — that's your design problem.

1. **Is this module deep?** Does the public interface hide significantly more complexity than it exposes? If the
    interface is nearly as complex as the implementation, the module is shallow. Either merge it into its caller or
    push more complexity inside.

1. **Is it simple (not complected)?** List the independent concerns. Can each change without forcing changes to the
    others? If not, separate them. (Read the language-appropriate patterns file.)

1. **Does it hide volatile decisions?** Could the implementation change without breaking callers? If callers must know
    the storage layout, the internal sequencing, or the optimization strategy, the module is leaking.

1. **Does it pull complexity down?** Are callers' lives simpler because this module exists? Or did the module just move
    the problem and add indirection?

1. **Are errors defined out of existence where possible?** Does the API create error cases that could be eliminated by
    redefining the operation's semantics?

1. **Is the interface general-purpose?** Does it encode knowledge of specific callers, or does it provide general
    operations that serve multiple use cases?

If the suspicion is that surface is dead rather than that the design is wrong, switch to the `dead-code-removal` skill.

## Working Rules by Context

### New module, crate, or namespace

Start with the interface, not the implementation. Write the public API signatures and their doc comments first. If you
can't write a clear one-sentence description of a method, the abstraction isn't clear yet — fix the design before
writing code.

Prefer fewer, broader methods over many narrow ones. A module with 3 methods that serve 10 use cases is deeper than a
module with 10 methods that serve 10 use cases.

**Design it twice.** For any non-trivial public boundary, develop _at least two substantively different_ designs and
compare them before committing. Ousterhout (ch 11) is explicit that this takes real time — _"an hour or two to consider
alternatives"_ for a small module — and that the time pays back because hard problems' first ideas are rarely good
enough.

The alternatives must be _different_, not refinements: different abstraction boundary, different invariants, different
decomposition. A second sketch that lives entirely inside the first design's frame is not a second design. Ousterhout's
framing: "consider a second possibility, or perhaps a third."

The comparison is the point, not just the better design. Ousterhout: _"the process of devising and comparing multiple
approaches will teach you about the factors that make designs better or worse."_ For an agent in one task, that means
writing the comparison out — depth, complecting, change-amplification, caller cost — so the tradeoffs surface and inform
the next decisions.

The trap to expect is the smart-people trap (Ousterhout's term): people who got good grades on easy problems with their
first idea develop the habit of trusting the first idea. Closing the goal feels like that signal. Treat it as evidence
the design works, not as evidence it is good.

### Refactoring an existing API

Identify the deepest useful boundary: the place where you can draw a line and hide the most volatile decisions behind
the simplest interface. Don't split modules before you understand what changes together.

**Premature modularity** creates false boundaries. When you find modules that always change together, they're likely one
module with an artificial split. Merge first, then re-split along actual volatility lines.

**Long methods aren't always bad (§9.8).** Ousterhout: a method with five 20-line blocks executed in order is fine if
the blocks are independent and the method has a simple signature; splitting introduces interfaces and forces the reader
to flip between fragments. Don't break a method up unless the split _makes the overall system simpler_. Two valid
splits: (a) factor a cleanly separable subtask into a child method (parent invokes child); (b) divide functionality into
two methods each visible to callers when the original interface was complex because it tried to do multiple things. If
callers must invoke both new methods and pass state between them, the split was wrong.

### Library boundary decisions

Boundaries are commitments to a stable interface. The two concrete forms in this codebase are Rust **crate** boundaries
and Lean **namespace** / file boundaries. Don't create a boundary just for organization — create it when you've
identified a volatile decision to hide and a stable interface to commit to.

**Facade modules** (a Rust facade crate, or a top-level Lean namespace that exports a curated set of names) earn their
existence by narrowing the interface. If the facade re-exports everything from its dependencies, it's a shallow wrapper.
If it curates a focused API that hides internal restructuring, it's deep.

### Reviewing for complexity

Use the **three symptoms** as diagnostic tools:

- **Change amplification:** Does a single logical change require edits in many places? Look for a missing abstraction
    that would localize the change.
- **Cognitive load:** Must a developer hold many facts in working memory to make one change? Look for an interface that
    hides those facts.
- **Unknown unknowns:** Could a developer make a change that seems correct but silently breaks something elsewhere? This
    is the worst symptom. Look for a hidden dependency to make explicit.

## Failure Smells

Each smell indicates a specific design problem. Don't fix the smell — fix the underlying problem it points to.

| Smell                                                                               | What it means                                        | Fix direction                                                                                |
| ----------------------------------------------------------------------------------- | ---------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| Public type with many public fields                                                 | Module is inside-out; no information hiding          | Make fields private, add methods that hide layout                                            |
| Type that only delegates to an inner type                                           | Shallow wrapper; no depth                            | Merge into caller, or push real complexity inside                                            |
| Facade re-exporting items with zero external callers                                | Dead surface area                                    | Delete the re-export                                                                         |
| API requiring calls in a specific order                                             | Temporal coupling without structural enforcement     | Use typestate (Rust phantom param, Lean indexed type), builder, or scoped API                |
| Module mixing storage, policy, and presentation                                     | Three concerns complected                            | Separate into three modules along volatility lines                                           |
| Generic wrapper when the caller knows the bundled type                              | Information loss; forces reconstruction              | Use the specific bundled type through the pipeline                                           |
| Parameter that every caller passes the same value for                               | Complexity leaked upward                             | Pull the default inside; define the parameter out of existence                               |
| Method named after what a specific caller does with it                              | Caller knowledge leaked into module                  | Rename to describe the general operation                                                     |
| Trait or `class` with only one implementor and no planned variance                  | Premature abstraction; indirection without value     | Remove abstraction, use concrete type                                                        |
| Module you can't describe in one sentence                                           | Complected concerns                                  | Split along the concern boundary                                                             |
| Item exposed (`pub` in Rust, non-`private` in Lean) "because it might be useful"    | Speculative surface area                             | Keep private until a real caller needs it                                                    |
| Error variant for a case the caller can never trigger                               | Unnecessary error surface                            | Redefine the operation so the case doesn't arise                                             |
| Pass-through method (forwards 1:1 with same signature, no added behaviour)          | Layer adds no abstraction                            | Delete the wrapper or push real complexity into it                                           |
| Many small types/classes/namespaces, each shallow ("classitis")                     | Over-decomposition; cost is paid at every boundary   | Merge until each boundary hides something worth hiding                                       |
| Getter/setter that exposes a private field 1:1                                      | Field is conceptually public; the layer is theatre   | Either expose the field, or replace with a method that hides the layout                      |
| Decorator class with mostly pass-through methods                                    | Shallow wrapper paying boilerplate for little gain   | Apply Ousterhout's four-question test; usually merge into the underlying class               |
| Internal representation has the same shape as the public interface                  | Class isn't deep; no asymmetry to hide complexity in | Reshape the interface to a different abstraction (e.g., character API over line-stored text) |
| Same design decision shows up in multiple modules ("Information Leakage")           | Coupling without an explicit boundary                | Move the decision behind one module's interface; callers stop knowing it                     |
| Module structure follows execution order (parse → validate → transform → save)      | Temporal Decomposition; no information hiding        | Reorganise around volatile decisions, not steps                                              |
| API forces callers to know rarely-used features to use common ones ("Overexposure") | Cost paid at every call site                         | Make the common case simple; gate uncommon options separately                                |
| Two methods/types that can't be understood independently ("Conjoined")              | Hidden mutual dependency                             | Merge them, or extract the shared concept into a third object                                |
| Hard to pick a precise name; documentation must be long to be complete              | Concept is unclear or complected                     | Simplify or split the concept until naming becomes obvious                                   |
| Code whose behaviour can't be understood by quick reading ("Nonobvious Code")       | Hidden assumptions, surprising control flow          | Rename, comment a non-obvious _why_, or restructure                                          |

## Proofs as Modules

A theorem-and-proof is a deep module. The statement is the interface; the proof is the implementation; a lemma is a
function. Depth ratio, classitis, cost-is-interface, complecting, and "design it twice" all carry over. _Lemma-itis_ is
the proof analogue of classitis. _Tactic soup_ is the proof analogue of stop-when-it-compiles. \_Reflexive `Type_`
polymorphism\* is the proof analogue of reflexive abstraction. Tao's rule for lemma statements — \*"easy to use, not
easy to prove"\* — is "the cost is the interface, not the implementation" in proof clothing.

For substantive guidance, load `writing-mathematical-proofs` for the philosophy and cross-language structured-proof
discipline, or `translating-proofs-to-lean4` for the Lean tactical apparatus (`have` / `suffices` / `calc`, the
Lamport-primitive ↔ Lean-tactic mapping, and "tactic soup" defaults).

## Before Declaring Done

You stop too early by default. The diff compiling is not the finish line. Run this checklist _before_ you say the change
is ready:

**Surface-area check:**

- [ ] No new `pub` (Rust) or non-`private` (Lean) items the user didn't explicitly request.
- [ ] Every new public item names at least one concrete caller that needs it. "It might be useful" doesn't count.
- [ ] No new error variants without a caller that meaningfully recovers (vs `unwrap` / propagate).
- [ ] No new parameters every caller passes the same value for.
- [ ] No new traits / typeclasses with a single implementor.
- [ ] No new pass-through methods (forwards 1:1, adds no abstraction).

**Depth check:**

- [ ] Audit script run: `bash .claude/skills/deep-module-design/scripts/audit-module.sh <path>`.
- [ ] Depth estimate (LOC / public items) improved or held vs the pre-change run, _or_ you can name what new capability
    justifies the surface growth.
- [ ] No new complecting (each new public type handles one independently-varying concern).

**Caller check:**

- [ ] Read 2–3 callers of the changed API. Did they get simpler, or just different? "Different" is not progress.
- [ ] Change amplification reduced: a future logical change of the same kind would touch fewer files than before.

**Language gate:**

- Rust: `cargo clippy -p <crate>` and `cargo nextest run -p <crate>`.
- Lean 4: run `make lean-build` in the Lean checkout. The `lean-lsp` MCP tools (`lean_build`,
    `lean_diagnostic_messages`, `lean_goal`) are the fast inner loop during iteration.

If any box doesn't tick, the design isn't done — fix the design, don't relax the box.

## Dispatch Agents

For significant design work, dispatch the pre-design audit agent before starting and the post-design verification agent
when done:

- **Pre-design audit:** `agents/pre-design-audit.md` — Classifies the design pressure, identifies complecting risks,
    lists applicable principles, and produces a constraint set before you write code.
- **Post-design verify:** `agents/post-design-verify.md` — Checks that depth improved, no new complecting was
    introduced, surface area shrank or held, and callers got simpler.

## Deep References

Read these only when you need the full theory or concrete patterns:

- `references/design-principles.md` — Full treatment of Ousterhout's deep module theory and Hickey's simplicity
    framework. Read when you need to justify a design decision or think through a novel tradeoff.
- `references/rust-patterns.md` — Concrete Rust patterns of complecting with decomplected alternatives.
- `references/lean4-patterns.md` — The same 10 patterns rendered in Lean 4 idioms (`private`, indexed types, bundled
    structures, `Finset`).

## Related Skills

Pick by what the boundary is _for_, not by what language the file is in.

- `dead-code-removal` — when the primary problem is dead surface area, not design.
- `mathematical-structure-design` — when the module boundary also encodes algebraic structure.

Rust-side neighbors:

- `kernel-boundary-enforcement` — when the boundary is a trust/safety boundary, not just a design issue.
- `ir-architecture-design` — when designing a new IR from scratch, not refactoring an existing module.

Lean-side neighbors:

- `writing-mathematical-proofs` — when the boundary in question is a theorem-and-its-proof rather than a code module.
    The same philosophy carries over (theorem statement = interface; lemma = function); the proof-side disciplines
    (Tao's "easy to use, not easy to prove"; Lamport's hierarchical method; Leron's level test) live there.
