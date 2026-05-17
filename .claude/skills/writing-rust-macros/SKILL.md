---
name: writing-rust-macros
description: Use for Rust macros — declarative (macro_rules!) and procedural (derive, attribute, function-like); boilerplate reduction, trait impls, DSLs, repetition not solvable with generics or functions.
---

# Writing Rust Macros

Rust macros eliminate repetitive code that functions and generics cannot abstract.

**Core principle:** Plan the macro architecture before writing a single rule. Decide declarative vs procedural _first_ —
switching mid-implementation wastes significant effort.

See `references/macro-patterns.md` in this skill directory for the complete pattern reference (fragment specifiers,
named patterns, proc macro structure).

## Workflow

1. **Decide** — Walk the decision flowchart below. Choose `macro_rules!` or proc macro _before_ writing code.
1. **Plan** — For `macro_rules!`: sketch parse→emit phases. For proc macros: sketch parse+transform split.
1. **Write** — Implement using patterns from `macro-patterns.md`.
1. **Verify expansion** — `cargo expand` (or `cargo expand module::name`). Invisible bugs are common.
1. **Test** — For proc macros: `trybuild` compile-fail tests. For all: test each input shape.
1. **Check hygiene** — `#[macro_export]` macros must use `$crate::` for all paths.

## Decision Flowchart

```
Can a function or generic solve this?
  YES → Don't use a macro.
  NO  ↓

Do you need to:
  - Accept variadic arguments?
  - Use file!()/line!()/column!()?
  - Stamp identical impls across a type list?
  - Generate code from a simple, uniform pattern?
    YES → macro_rules! (declarative)
    NO  ↓

Do you need to:
  - Inspect struct/enum fields by name and type?
  - Parse custom attributes (#[my_attr(...)])?
  - Generate new type definitions from existing ones?
  - Handle complex per-field logic (skip, rename, validate)?
    YES → Proc macro (derive, attribute, or function-like)
    NO  ↓

Do you need to:
  - Concatenate identifiers (visit_ + variant_name)?
  - Convert case (snake_case, CamelCase)?
    → macro_rules! + `paste` crate
    → OR proc macro if logic is already complex

Still unsure?
  - Pattern is uniform across variants/types → macro_rules!
  - Pattern requires per-item decisions → proc macro
```

## Quick Decision Table

| Situation                        | Tool              | Why                                        |
| -------------------------------- | ----------------- | ------------------------------------------ |
| Same impl for a list of types    | `macro_rules!`    | Stamp-out pattern; no introspection needed |
| `From<X>` for N error variants   | `macro_rules!`    | Uniform boilerplate                        |
| `file!()`/`line!()` at call site | `macro_rules!`    | Built-in macros must expand at call site   |
| Builder pattern with attributes  | proc macro derive | Per-field attribute parsing                |
| Visitor over enum variants       | proc macro derive | Needs field indexing and name generation   |
| Trait impls for primitives       | `macro_rules!`    | Type list stamp-out                        |
| DSL with custom syntax           | either            | Depends on syntax complexity               |

## Debugging

```bash
cargo expand                  # expand all macros
cargo expand module::name     # expand specific module
```

**Always check `cargo expand` output** before considering a macro done.

For `macro_rules!` on nightly: `trace_macros!(true)` before invocation. For proc macros:
`eprintln!("GENERATED:\n{}", output)` in the impl function.

## Common Mistakes

1. **Choosing `macro_rules!` for field-introspection.** Use a proc macro.
1. **Not planning phases.** Always design parse → emit.
1. **Ignoring follow-set restrictions.** `$x:expr` can only be followed by `=>`, `,`, `;`.
1. **Repeated side effects.** `$x:expr` used twice = expression evaluated twice. Bind to `let` first.
1. **Missing `$crate::`** in exported macros.
1. **Skipping `cargo expand`.** Always verify expansion.
1. **Using `panic!` in proc macros.** Use `syn::Error::new_spanned()`.
1. **Fragment opacity.** `$x:expr` passed to another macro becomes opaque. Use `$($x:tt)*` for re-parsing.

## Escalation Ladder

```
Plain function  →  Can abstract over values
    ↓ not enough
Generics + traits  →  Can abstract over types
    ↓ not enough
macro_rules!  →  Can abstract over code fragments
    ↓ not enough (need field introspection, indexing, attributes)
Proc macro  →  Full Rust at compile time
```

Each step adds complexity. Start at the top; escalate only when needed.

## API Design (Rust API Guidelines)

- **C-EVOCATIVE**: Macro input should mirror the output syntax
- **C-MACRO-ATTR**: Allow `#[cfg(...)]`, `#[derive(...)]` on generated items
- **C-ANYWHERE**: Item macros must work at module scope AND inside functions
- **C-MACRO-VIS**: Accept `$vis:vis` to let callers control visibility
