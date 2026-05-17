# On Mathematical Exposition

Mathematical writing fails the same way most writing fails: the author hadn't yet figured out what they were trying to
say. The symbols pile up, the definitions accumulate, and somewhere underneath there's an idea the reader never gets to
meet.

The fix is what Halmos called the first rule of exposition: have something to say. If you can't state your central point
in a sentence of plain English, you don't yet know what you're proving. Keep working until you do. Then write to a
specific person — a colleague, a former student, anyone real — and ask whether they'd follow what you've written.

The writers who do this well agree on more than you'd expect. Halmos, Tao, Knuth, Gowers, Carter, Sanderson, Azad —
different fields, different formats, same instincts. Their advice rhymes because they're all solving the same problem.

## Resist Symbols

Halmos put it bluntly: the best notation is no notation. Every Greek letter you introduce is cognitive load. Use English
where English works.

Tao gives this its sharpest form: use soft words and hard arguments. The mathematics should be rigorous; the language
carrying it should be plain. The connectives that feel humble — "also," "but," "since," "however" — do real work. They
tell the reader how each step fits into the larger argument. A page that reads like a sequence of lemmas tells you
nothing about why those lemmas are in that order.

When you do introduce notation, name it in English the moment it appears. Not "Let φ: G → H." Instead: "Let φ: G → H be
a group homomorphism — a function that preserves the group operation, so that φ(gh) = φ(g)φ(h)." The reader should never
have to flip back.

## Show the Path, Not Just the Destination

A theorem is a record of understanding, not a route to it. The reader needs the route.

Carter's *Visual Group Theory* opens not with axioms but with a Rubik's cube. Group theory, he writes, is not primarily
about numbers; it's about patterns. Then he asks the reader to flip a paper rectangle, building a map of its
configurations. By the time the word "group" appears formally, the reader has discovered the Klein four-group with their
hands. The definition feels like a name for something they already know.

Gowers explains imaginary numbers by attacking the confusion first. Why does √-1 seem absurd? Because we think of
numbers as quantities. Once you accept that a number can be defined by what it does rather than what it "is," imaginary
numbers stop being mystical. The reader's confusion isn't an obstacle to the explanation. It is the explanation.

Azad introduces *e* through continuous compounding interest, not through limits. By the time the formula
`e = lim(1 + 1/n)^n` arrives, the reader already understands the thing the formula names.

The pattern: start with phenomena, then abstract. Work a concrete example before stating the general theorem. Address
the reader's confusion directly — the places students get stuck aren't bugs in the exposition; that's where the
explanation needs to live.

## Tell the Reader What You're Doing

Say what you're about to do. Do it. Say what you did.

This rhythm feels redundant when you're writing — you already see the structure. The reader doesn't. They need the
signposts: "We prove this in three steps." "The key idea is..." "This is where compactness is used." Without these, the
reader follows the symbols but loses the architecture.

Treat the trivial cases. If n=0 is weird, say so. If the empty case is excluded by convention, say why. Halmos called
the alternative "legalistically correct but insufficiently explicit" — technically right and pedagogically useless.

## Voice Is Allowed

Mathematical writing has a default funeral-parlor tone that nothing in the subject requires. Knuth, Graham, and
Patashnik's *Concrete Mathematics* rejects it openly. The margins carry "mathematical graffiti" from their students. The
voice is one of working mathematicians thinking aloud — including the false starts. The reader is invited into the
process rather than handed finished results.

Showing the false starts honestly is more useful than hiding them. The reader is going to have false starts of their
own. They need to know that's normal.

## Find the Picture

If you can't draw the idea, you may not understand it yet.

Sanderson's "Essence of Calculus" introduces derivatives by asking how fast the area of a circle changes as you increase
its radius. Watching a thin ring appear at the boundary — watching dA/dr become 2πr because the ring has circumference
2πr and infinitesimal width dr — makes the formula obvious in a way no symbolic derivation does.

Even pure prose exposition benefits from asking: what's the picture I'm trying to put in the reader's head? If there
isn't one, the writing will float free of meaning, and the reader will float with it.

## The Test

Tim Gowers borrows a phrase from Timothy Chow: the *open exposition problem*. To solve one is to explain a subject so
completely that every step feels motivated, and the reader senses they could have arrived at the result themselves.

That's the test. Hand the finished piece to your imagined reader. Do they follow it? Do they feel, at the end, that the
result was inevitable?

If yes, you've written mathematics well. If no, the work isn't done. Halmos again: write it, rewrite it, and re-rewrite
it several times.

Then stop. The hardest discipline in mathematical writing is leaving out the digressions you find interesting. When
you've said what you came to say, the page is finished.
