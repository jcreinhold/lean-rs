---
name: technical-writing
description: 'Use for writing, revising, or reviewing prose: docstrings, comments, mathematical exposition, issues, design docs, PR summaries, READMEs, commit messages.'
---

# Technical Writing

Help the reader understand. Everything else serves that.

## When to use

- Writing or revising prose in `docs/`, design documents, or READMEs
- Improving comments and docstrings for clarity and rationale
- Drafting or editing issues, PR summaries, blocker reports, commit messages
- Writing or revising mathematical exposition that is not itself a proof
- Any text a human will read and that matters

## When not to use

- Formal proof writing, theorem-proof blocks, or proof decomposition — use `writing-mathematical-proofs`
- Adversarial proof review — use `mathematical-proof-review`
- `goal.md` or stable `proposal.md` when the mathematical center is being set or revised, not just its prose — use
    `mathematical-conjecture-design`

## What to read first

Before editing prose, read the references that set the standard:

1. [references/on-writing.md](./references/on-writing.md) — universal prose craft, then technical documentation, then
    comments
1. [references/on-writing-mathematics.md](./references/on-writing-mathematics.md) — read this as well when the text
    carries mathematical content

These are the baseline. Local convenience does not override them.

## Workflow

### Edit (default)

1. Read the target text and enough surrounding context to know what must not change — the relevant `AGENTS.md`, the
    local `README.md` or `goal.md`, adjacent definitions or theorems.
1. Read the references above.
1. Identify the audience and the purpose. A docstring serves callers; a design doc serves reviewers; a commit message
    serves the next person who blames the line.
1. Find the real problems: buried lede, vague pronouns, jargon used without inline definition, redundancy, comments that
    restate code, paragraphs doing more than one job.
1. Rewrite. Preserve every claim and technical distinction exactly. Change structure, word choice, sentence
    construction, paragraph breaks. Add a brief inline definition the first time a term appears. Replace a
    code-restating comment with rationale, or delete it.
1. Show the before/after for localized changes. For substantial rewrites, show the revised text with a short note on
    what changed and why.

### Review only

When asked to review without editing, report each problem in place — location, what is wrong, how to fix it — and stop.
Reporting is the deliverable.

## Subagents

For minimal prose-only edits, a local worker is available:

- [`agents/edit.md`](./agents/edit.md) — reads the references, identifies prose problems, rewrites for clarity and
    precision.
