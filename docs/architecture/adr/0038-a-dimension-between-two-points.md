# ADR 0038: A dimension between two points

Status: implemented — the dimension tool measures between two points, the
relation it makes is drawn as a dimension, and typing a new value into it moves
the geometry.

- Date: 2026-09-17
- Decision owners: Artificer project

## Context

The user asked to dimension between two sketch objects and got nothing back.

Everything the feature needed was already in the tree, and none of it was
joined up.

**The solver has held a distance between two arbitrary points since the sketch
crate was written.** `SketchConstraintKind::Distance { first, second, distance }`
names two point ids — they need not belong to the same object — and its
projection already does the right thing when one end is held: the held end
stays and the other travels.

**The relation tool already made one.** Picking two endpoints with the Distance
relation creates that constraint, seeded with the separation the points already
have. Its own tooltip said, in as many words, "their present separation becomes
the held value; edit it afterwards with the dimension tool".

**Nothing drew it and nothing could edit it.** Not one line of the UI read
`constraints()` outside its own tests. The dimension tool resolved a pick to a
recipe's stable key — `width`, `diameter`, `length` — and a distance between
two points is not a number any recipe carries, so the tool could not see it and
bailed. The promise in the tooltip was never kept by any code.

So a user who reached for the tool named "dimension" and clicked two objects
got silence, and a user who found the relation tool instead got an invisible,
unchangeable constraint. That is the same fault as ADR 0037's: a capability
present in the model with no route to it, which to the person using it is
indistinguishable from a capability that does not exist.

## Decision

### A dimension between two points is a relation, not a new kind of record

No new persisted type. The dimension *is* the `Distance` relation: already
serialised with the document, already transactional, already undoable, already
solved. What was missing was a way to see it and a way to change it, and those
are presentation and editing concerns rather than model ones.

This is why a dimension survives a save and a reload with nothing added to the
document format.

### Discovery is from the relation, not from an operation

Every other dimension on the canvas is found by asking an operation which
numbers its recipe carries. A dimension between two points belongs to no single
operation, so it cannot be found that way and is read from the relation itself.
`point_to_point_dimensions` reports each one with both ends at their *solved*
positions, so the annotation is drawn against the geometry actually on screen
rather than against authored coordinates the solver has since moved.

It reads whichever definition the canvas is presenting — the staged candidate
while an edit waits, the committed sketch otherwise. A dimension placed a
moment ago is part of what the user is looking at, and reading the committed
sketch instead would leave it invisible until confirmed, which is the original
complaint wearing different clothes.

### Retyping a dimension holds the end it is measured from

A dimension the user types into is an edit, and ADR 0035 already settled what
an edit outranks: the solver's freedom to share the movement out. The relation's
first point is the end the dimension is measured *from*, so it stays, and the
second end is what travels. It is the same rule that makes a line's length move
its end and leave its start.

`stage_relation_measurement` anchors that point, then writes every point the
solver had to move back into its own operation's recipe — the same machinery
`stage_replace_pulling_followers` uses, now shared between them. Without the
write-back the anchor would last exactly one solve and the next ordinary read
would split the difference and drag the far object half way back.

Holding neither end stays legitimate and means what it says: the solver shares
the movement, which is the fair answer when nothing distinguishes the two
points.

### A value the sketch cannot hold is refused, in the solver's words

A dimension that contradicts the rest of the system leaves the sketch exactly
as it was, and the text stays on the canvas to be corrected. The sketch never
stores a relation saying one thing while its geometry measures another — the
guarantee ADR 0026's F1 asked for and the P1 gate pinned.

This is the same bargain every numeric field here makes, and it is worth being
explicit that the alternative was available and rejected: a dimension quietly
rounded to whatever the solver could reach would be a number the user did not
type, presented as though they had.

### Placing and retyping both apply the moment they are accepted

The dimension tool's contract is `CommitsOnAcceptance`, and the workbench
already states the rule: sketching flows one stroke into the next with undo as
the safety net, and a typed dimension applies the same way. Both routes hand
their edit to the same immediate-commit path a freshly drawn stroke uses, so
neither leaves anything stranded at a confirmation gate.

The first version of this work staged the edit and waited for the gate instead.
The end-to-end test caught it: the edit was staged in the sketch and invisible
to the confirmation UI, so it could never be confirmed at all. A tool's declared
commit contract is not decoration.

### An endpoint pick beats a curve pick

With the dimension tool armed, a click that lands on a point is a question
about that point; a click anywhere else is the question it always was, about
the object under it. That precedence is what snapping and the relation tools
already use, so the tool gains a gesture without changing the one it had.

The relation it stages is built by the same code the Distance relation tool
uses, because "the distance between these two points" means one thing however
the user asked for it.

## Consequences and limits

**Only a distance can be typed into.** `measurement` reports the number a
relation holds, and a relation that states a pure relationship — perpendicular,
coincident — holds none. Typing into one is refused by name rather than
quietly turning it into a different relation.

**A follower its recipe cannot state is still left behind.** The write-back
inherits ADR 0035's limit exactly: a rectangle's derived corners, a pattern's
copies and a trim's fragment ends have no literal to write into, so the
ordinary solve goes on sharing the movement for that one relation.

**Dimensions are always drawn, and there is no way to hide one.** A relation
the user placed deliberately is a dimension and reads as one whenever the
sketch is open. A sketch carrying many of them will get busy, and a visibility
control belongs with the constraint glyphs ADR 0026 P5 specifies rather than
here.

**Angle and radius dimensions between objects remain unbuilt.** They are
separate relation kinds with separate residuals — ADR 0026 F1 stage 2 lists
them — not this one wearing a different label.
