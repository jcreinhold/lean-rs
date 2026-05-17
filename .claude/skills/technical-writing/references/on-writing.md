# On Writing Well

Good writing is clear thinking made visible.

The hardest part isn't choosing words. It's knowing what you mean. Most bad prose is bad because the writer hadn't yet
figured out what they were trying to say. Vague writing reflects vague thinking. When you catch yourself reaching for
"essentially" or "various factors," stop — you don't know yet. Keep working until you do.

Then say it directly. Strong verbs move sentences forward; concrete nouns hold them in place. Abstractions like "issues"
and "considerations" are usually places where the writer gave up. "Spring" beats "jump" when you mean spring, and "use"
beats "utilize" almost always.

Now cut. First drafts are too long — yours, mine, everyone's. The real work is deletion: the hedges, the
throat-clearing, the sentences you wrote to figure out what you thought. Once you've figured it out, those sentences
have to go. They served you, not the reader.

Read it aloud. Your ear catches what your eye misses. Where you stumble, your reader stumbles too, and they won't be as
patient as you are. Listen for rhythm. Short sentences land hard. Longer ones, the kind that need room to breathe, can
carry a thought through several turns before arriving. Mix them, but let the content choose — not the rule.

The last word of a sentence lingers. "She walked into the room in silence" leaves you with silence; "She walked silently
into the room" leaves you with the room. These are different sentences. Decide which one you mean.

Trust your reader. You don't need "however" and "therefore" lighting every turn. If the logic is clear, the connections
show themselves. Readers are smarter than most writers credit them; insult their intelligence and you'll lose them
faster than any clumsy sentence will.

None of this is law. The writers you love probably break every rule here. Faulkner does. Woolf does. The rules are
defaults — they work most of the time, which means follow them until you have a specific reason not to. Orwell put it
best: break any rule sooner than say something barbarous. Clarity wins.

The test is simple. Did the reader understand? Not "were they impressed," not "did the prose sound writerly" — did they
get it? Everything else, every technique, every careful choice, exists to serve that one thing.

Make yourself understood. Respect the reader's time. Keep working until the prose disappears and only the meaning
remains.

Then revise. Then revise again.

______________________________________________________________________

# Technical Documentation

The craft above is universal. Technical documentation adds constraints — code, APIs, errors, configurations — but the
discipline doesn't change. A few patterns earn their place by recurring.

## Show before you explain

Start with a working example. The reader sees the shape before they read the explanation:

```rust
let numbers = vec![1, 2, 3, 4];
let doubled: Vec<i32> = numbers.iter().map(|x| x * 2).collect();
```

Then name what `map`, `iter`, and `collect` do. A reader who has already watched the code work has somewhere to hang the
explanation.

## Title for the task, not the tool

"Using the HttpClient class" tells the reader nothing about whether to read on. "Making HTTP requests" tells them
exactly. Name the job.

## Document the why

Code shows what. A comment that says `# increment counter` next to `count += 1` is noise. A comment that says
`# ensure we process at least one chunk even if input is empty` justifies the line and survives the next refactor.

## Put the common case first

Most readers want basic usage. Lead with it: quick start, then common patterns, then advanced cases, then the full
reference. Reverse this order and the eighty-percent reader wades through edge cases that don't apply to them.

## Be consistent

If you call it a "handler" in one place, don't call it a "callback" elsewhere. Terminology drift is a tax on every
reader.

## Show the error cases

The happy path is half the contract. Spell out what gets thrown, what gets returned, what fails silently:

```python
# Returns the user if found.
# Raises UserNotFoundError if no such user.
# Raises DatabaseError if the connection fails.
def get_user(user_id: int) -> User:
```

## Warn about gotchas up front

If a subtle bug catches everyone, say so on the way in. A footnote three pages on cannot rescue a reader who has already
followed the wrong path.

## Be precise without being pedantic

"`foo(x: i32) -> bool` returns `true` if `x` is even" beats "the function `foo` accepts a parameter `x` of type `i32` (a
32-bit signed integer) and returns a value of type `bool` (a boolean value representing true or false)." Say what the
function does, in the type system's own words. The reader knows what `i32` is.

## Use concrete types in examples

Generic `T` makes examples abstract before the reader has learned what they're seeing. Write the concrete case first:

```rust
fn process_user(user: User) -> Result<User, Error>
```

Then note that it generalizes: "This works with any `T` that implements `Serialize`."

## Disclose progressively

Level one: "This function adds two numbers." Level two: "It wraps on overflow." Level three: "It uses SIMD when
available." Each level only matters to readers who got something from the previous one.

______________________________________________________________________

# Comments

Comments capture what the code cannot. A comment that restates what the code already says adds noise the maintainer must
keep in sync.

## Interface comments are for callers

They describe _what_, not _how_: what the function does, what its parameters mean (units, bounds, ownership,
nullability), what it returns, what it can throw, what state it touches. The reader should be able to use the interface
without opening the implementation.

## Implementation comments are for the next reader of the code

They explain non-obvious reasoning: why this algorithm and not another, what invariant a tricky line preserves, what
edge case the odd-looking branch handles. These belong inside the function, next to the line they explain.

## Be precise

"The current position" is vague; "the index of the next character to be processed" is useful. State the nullability.
State the units. State whether bounds are inclusive or exclusive. State who owns the allocation.

## Capture rationale

The most valuable comments preserve thinking that would otherwise be lost: why this approach, why this special case, why
this odd-looking default. The code shows what you did; the comment preserves what you were trying to do.

## Write comments first

Drafting the interface comment before the body forces you to state the abstraction in words. If the words come out
tangled, the design is tangled too — better to find that out before writing five hundred lines.

## Maintain comments as the code changes

A stale comment is worse than no comment: it lies confidently. When you change a function, update its comment in the
same commit. If you keep skipping that step, your comments are probably tied to implementation details that should have
been left out — describe the stable abstraction, not the current bytes.
