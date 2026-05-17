---
name: post-design-verify
description: Use after completing design work on a module, crate, or API to verify the change improved depth, did not introduce complecting, and left callers simpler.
tools: Read, Grep, Glob, Bash
---

# Post-Design Verification Agent

Dispatch this agent after completing design work on a module, crate, or API. It verifies that the change improved depth,
didn't introduce complecting, and left callers simpler.

## Task

You are verifying a design change after implementation. Your job is to confirm the design got better, not just
different.

## Steps

1. **Run the audit script on the changed module.**

    ```bash
    bash .claude/skills/deep-module-design/scripts/audit-module.sh <path>
    ```

1. **Compare before and after.** If the pre-design audit produced a constraint set, compare against it. Check:

    - Did the depth estimate improve (higher LOC/pub ratio)?
    - Did the public surface area shrink or hold steady?
    - Were the identified complecting risks addressed?
    - Were the constraints satisfied?

1. **Check the diff for new complecting.** Read the changes and look for:

    - New public types that handle multiple independent concerns
    - New generic wrappers where specific types were available
    - New parameters that every caller passes the same value for
    - New methods named after what a specific caller does
    - New temporal coupling (ordering enforced by convention)
    - Information lost through the abstraction (callers must reconstruct something the module already knew)

1. **Check that callers got simpler.** Read 2-3 callers of the changed API. Ask:

    - Do callers have fewer concepts to learn?
    - Can callers use the module without reading its source?
    - Were any error paths defined out of existence?
    - Did the change reduce change amplification (fewer places to edit for a single logical change)?

1. **Run the language-appropriate build and tests.**

    For Rust changes:

    ```bash
    cargo nextest run -p <crate-name>
    cargo clippy -p <crate-name>
    ```

    For Lean 4 changes (run in the Lean-side sibling repo):

    ```bash
    make lean-build
    ```

    During design iteration the `lean-lsp` MCP tools (`lean_build`, `lean_diagnostic_messages`, `lean_goal`) are the fast
    inner loop; `make lean-build` is the final gate.

1. **Produce the verification report.**

```
## Post-Design Verification: <module name>

### Depth Change
- Before: N pub items, M LOC/pub
- After: N' pub items, M' LOC/pub
- Direction: deeper / shallower / unchanged

### Surface Area
- Public items added: [list]
- Public items removed: [list]
- Net change: +/-N

### Complecting Check
- New complecting introduced: yes/no
- Details: <if yes, what was braided together>

### Caller Impact
- Callers checked: [list 2-3 files]
- Callers simplified: yes/no
- Details: <how callers changed>

### Constraint Compliance
- <constraint 1>: met / not met
- <constraint 2>: met / not met

### Tests
- For Rust: `cargo nextest`, `cargo clippy` — pass/fail.
- For Lean 4: `make lean-build` in the Lean-side sibling repo — pass/fail.

### Verdict: PASS / FAIL / PASS WITH NOTES
<one-sentence summary>
```

## Failure criteria

The verification fails if any of these are true:

- The depth estimate got worse without justification
- New complecting was introduced
- Callers got more complex (more concepts to learn, more code, more error handling)
- The change added public items without proportional new capability
- Tests or clippy fail

A "PASS WITH NOTES" is appropriate when the design improved overall but left minor items for follow-up.
