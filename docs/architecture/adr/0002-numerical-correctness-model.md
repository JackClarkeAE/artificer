# ADR 0002: Numerical correctness and tolerance model

Status: Accepted and implemented — the tolerance model ships as
`PrecisionPolicy` (`crates/protocol/src/lib.rs`, `linear_agreement`,
`angular_agreement_radians`, `min_feature_size`) and is enforced by the solid
validator (`crates/kernel/src/validator.rs`) on every commit
- Date: 2026-07-28
- Decision owners: Artificer project

## Context

B-rep failures commonly arise when geometric decisions made with finite floating-point arithmetic contradict each other. A single global epsilon does not solve this: it conflates mathematical predicate signs, uncertainty in constructed geometry, the product's minimum meaningful feature, parameter-space error, and display tessellation.

Full exact construction for general NURBS geometry is impractical as a universal representation, while ordinary `f64` comparisons are insufficient for topology-changing decisions near degeneracy.

## Decision

Artificer will use a hybrid certified model:

- Ordinary geometry evaluation and approximate constructions use `f64` in documented local/model frames.
- Critical predicates use a filtered ladder: bounded `f64` first, then adaptive expansion or exact arithmetic when the fast result is uncertain.
- Root/intersection construction uses intervals, subdivision, higher precision where needed, and a certified residual/error enclosure.
- Every constructed entity carries or can produce an uncertainty/error bound appropriate to its representation.
- Every document/operation receives an explicit `PrecisionPolicy`; tolerances are dimensioned and named.
- Modeling resolution, entity uncertainty, predicate correctness, and display tolerance are separate APIs.
- Algorithms may return `NumericallyIndeterminate` rather than inventing topology.
- Operations keep an inspectable tolerance ledger and cannot silently inflate entity tolerances.
- The supported coordinate/feature-size envelope is empirically established, versioned, and tested across all included magnitude decades.

### Required predicate result

Topology-changing predicates return:

```text
Negative | Zero | Positive | Indeterminate
```

They do not return `abs(value) < epsilon` as mathematical zero. Product-level merge/sew decisions may consider modeling resolution only after predicate and uncertainty information are preserved.

For exact represented inputs, `Zero` means exact degeneracy of those represented mathematical values. When an operand represents an uncertainty enclosure, a signed result is certified only if it is invariant for every admissible value. If that enclosure spans multiple signs, or includes both zero and a nonzero sign, the result is `Indeterminate`; modeling-resolution equivalence is not `Zero`.

### Initial `PrecisionPolicy` concerns

The exact Rust schema is deferred to M0, but it must distinguish at least:

- Document units and model-space normalization.
- Modeling resolution/minimum retained feature.
- Linear and angular agreement bounds.
- Parameter-space/root-isolation bounds.
- Curve/surface approximation budget.
- Maximum entity uncertainty and cumulative-operation budget.
- Iteration/subdivision/precision ceilings that lead to `Indeterminate`.
- Independent tessellation chord, angle, and normal criteria.

## Consequences

### Benefits

- Topological control flow remains correct for many near-degenerate cases without paying arbitrary-precision cost everywhere.
- Failures are explicit and reproducible rather than depending on scattered constants.
- The kernel can honestly state its supported numerical range.
- Diagnostic traces can explain whether a problem came from input uncertainty, product resolution, root isolation, or an exact-sign failure.

### Costs and risks

- Predicates, constructions, and every modeling algorithm must propagate richer results and error information.
- Exact/adaptive fallback and interval methods increase implementation complexity.
- Some inputs return `Indeterminate` until the supported domain is widened.
- Cross-platform deterministic floating-point behaviour requires controlled evaluation order and careful compiler settings.
- Incorrect error-bound analysis is itself a correctness defect and needs independent oracle tests.

## Rejected policies

### One global epsilon

Rejected because units, scale, derivative conditioning, UV domains, angle, and accumulated uncertainty differ. It also permits mutually contradictory decisions.

### Always use arbitrary precision

Rejected as a universal storage/construction strategy because general spline roots and curved intersections still require algebraic/approximate representation and the cost would be paid in routine evaluations. Exact arithmetic remains available for bounded predicates and reference tests.

### Pure `f64` plus retries/healing

Rejected because rerunning or increasing tolerances can hide wrong topology, make results nondeterministic, and destroy small features without an explicit product decision.

## Verification

- Exhaustive small-integer predicate grids against an exact oracle.
- At least one million random and near-degenerate cases per critical predicate before T1.
- `nextafter` tests around every modeling-resolution boundary.
- Transform, translation, and scale metamorphic tests.
- Analytic construction identities and interval containment checks.
- Full tolerance-ledger capture in every relevant failure bundle.

## Sources

- [Shewchuk, Adaptive Precision Floating-Point Arithmetic and Fast Robust Geometric Predicates](https://people.eecs.berkeley.edu/~jrs/papers/robustr.pdf)
- [CGAL linear geometry kernel and exact-predicate choices](https://doc.cgal.org/latest/Kernel_23/index.html)
- [CGAL filtered kernel](https://doc.cgal.org/latest/Kernel_23/structCGAL_1_1Filtered__kernel.html)
