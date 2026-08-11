# M0 coordinate and feature envelope

The native kernel's candidate working envelope is intentionally narrower than
binary64's representable range. Model coordinates are millimetres.

| Quantity | Candidate contract |
| --- | ---: |
| Absolute coordinate | at most 1e9 mm |
| Modeling resolution | at least 1e-9 mm |
| Minimum feature | at least 1e-7 mm |
| Approximation budget | at least the modeling resolution |
| Scale ratio within one operation | at most 1e16 |

Every public operation must preflight finite inputs and representability before
topology allocation. Certified predicates reason about the exact real values
represented by binary64 and never substitute an epsilon sign. Modeling
resolution is used for product intent and validation, not to turn an uncertain
predicate into a guessed answer.

The executable evidence is split deliberately:

- `artificer-geometry` runs the one-million-case exact-integer orientation corpus,
  power-of-two scale, translation, overflow, underflow, and cancellation cases.
- `artificer-cli repeat` proves 100 byte-identical versioned journals.
- CI runs the repeat contract on pinned Linux and macOS workers.
- kernel operation tests cover coordinate-limit rejection and minimum-feature
  policy without publishing a partial snapshot.

Changing this envelope requires new evidence and an architecture decision; it
is not a tolerance knob that algorithms may widen locally.
