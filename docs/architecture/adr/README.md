# Architecture decision records

One record per decision that would otherwise have to be re-derived from the
code. Each file opens with a single bare `Status:` line; the summaries below
are the first clause of that line, so this index can never disagree with a
record without the disagreement being visible.

Statuses mean what they say: **accepted** is a decision the tree follows,
**implemented** additionally means the described machinery ships and is gated
by tests, and **proposed** is a plan not yet executed.

| # | Title | Status | Superseded or extended by |
|---|---|---|---|
| [0001](0001-native-rust-kernel-with-reference-backend.md) | Native Rust kernel with an external development oracle | Accepted | — |
| [0002](0002-numerical-correctness-model.md) | Numerical correctness and tolerance model | Accepted and implemented | — |
| [0003](0003-entity-identity-and-persistent-references.md) | Entity identity across snapshots and regeneration | Accepted and implemented | — |
| [0004](0004-first-experimental-cuboid-slice.md) | First experimental cuboid slice | Accepted | — |
| [0005](0005-display-transforms-and-motion.md) | Display transforms and deterministic motion | Accepted | 0006, 0007 |
| [0006](0006-committed-similarity-transforms.md) | Committed whole-snapshot similarity transforms | Accepted | — |
| [0007](0007-universal-model-operation-confirmation.md) | Universal confirmation for interactive model operations | Accepted | 0027 |
| [0008](0008-plane-profile-workbench.md) | Plane and profile workbench boundary | Accepted | 0027 |
| [0009](0009-live-sketch-dimensions.md) | Live sketch dimensions are editable construction intent | Accepted | 0027 |
| [0010](0010-first-convex-profile-extrusion.md) | First native profile extrusion is a declarative convex constructor | Accepted (historical M4a slice) | 0015 |
| [0011](0011-expandable-workbench-shell.md) | Expandable workbench shell preserves a fixed confirmation rail | Accepted | 0014 (history-preview authority), 0028 |
| [0012](0012-first-selected-face-add-cut.md) | First selected-face Add and Cut use an exact rectangular scaffold | Accepted (historical M4c slice) | 0013, 0015 |
| [0013](0013-repeatable-rectangular-face-features.md) | Repeatable rectangular face features use local boundary rewrites | Accepted (historical M4d slice) | 0015 |
| [0014](0014-m5a-parametric-document-foundation.md) | M5a parametric document foundation | Accepted | — |
| [0015](0015-linear-profile-features-and-history-rollback.md) | Linear-profile features use hole-aware faces and exact push/pull | Accepted (M4e/M5a slice) | 0016 |
| [0016](0016-exact-planar-profile-curves-and-regions.md) | Exact planar curves form deterministic material regions | Accepted | — |
| [0017](0017-portable-native-document-v4.md) | Portable native document v4 and atomic fresh-process replay | Accepted | — |
| [0018](0018-content-addressed-part-library-and-components.md) | Content-addressed local Part Library and component occurrences | Accepted | — |
| [0019](0019-rigid-occurrence-placement-and-joint-forest.md) | Rigid occurrence placement and persistent joint forest | Accepted | 0026 F8 (joint kinds, pose solver) |
| [0020](0020-regularized-exact-planar-face-features.md) | One regularized exact-profile path for selected-face features | Accepted and implemented | — |
| [0021](0021-editable-sketch-authoring-and-region-replay.md) | Editable sketch authoring and late-bound region replay | Accepted and implemented | 0027 |
| [0022](0022-development-incidents-and-input-tracing.md) | Development incidents and privacy-conscious input tracing | Accepted | — |
| [0023](0023-carrier-unified-rims-and-exact-rim-blends.md) | Carrier-unified rims and exact rim blends | Implemented | — |
| 0024 | *never written* | — | — |
| [0025](0025-analytic-surface-intersections.md) | Analytic surface intersections and the Boolean domain oracle | Implemented | — |
| [0026](0026-second-expansion-programme.md) | The second expansion programme | Accepted (Phase 1 delivered) | — |
| [0027](0027-sketch-edits-commit-on-acceptance.md) | Sketch strokes and typed dimensions commit on acceptance, on the canvas or in the panel | Accepted and implemented | — |
| [0028](0028-workbench-command-registry-and-contextual-properties.md) | The workbench command registry, ribbon tabs, and contextual properties | Accepted and implemented | — |
| [0029](0029-velopack-installers-and-in-app-updates.md) | Velopack installers and in-app updates | Accepted and implemented | — |

The 0024 gap is deliberate and recorded rather than backfilled: renumbering
published records would break every reference that already points at 0025.
