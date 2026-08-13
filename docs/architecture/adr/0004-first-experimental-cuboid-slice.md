# ADR 0004: First experimental cuboid slice

Status: Accepted
- Date: 2026-07-28
- Decision owners: Artificer project

## Context

The first implementation must prove the development loop, architectural boundaries, and visual testing path without pretending to solve general B-rep modeling. A 2D mock would exercise fewer of the topology and rendering contracts, while starting with arbitrary curves or booleans would introduce too many numerical problems before replay and validation exist.

## Decision

The first native capability is an explicitly experimental `MakeCuboid` command. It accepts a finite origin and three finite positive extents above both the active modeling resolution and minimum supported feature size.

On success it constructs a genuine planar B-rep containing:

- 8 vertices.
- 12 shared edges.
- 24 oppositely oriented coedges with planar UV p-curves.
- 6 four-coedge loops.
- 6 outward-oriented planar faces.
- 1 closed shell and 1 solid.
- 12 source-face-mapped display triangles and 12 source-edge-mapped display segments.

The operation executes against an immutable snapshot. The candidate is fully validated before publication; stale snapshots, cancellation, invalid input, or postcondition failures return no new snapshot.

The same native command path is used by the CLI, declarative cases, replay, and the Rust workbench UI. The UI renders deterministic diagnostic geometry, supports source-mapped face selection, and retains the last valid body after a rejected edit.

OCCT is absent from this slice and from every product dependency. A future external oracle may consume the same declarative test intent, but cannot execute application commands or generate an Artificer model.

## Required validation

- All references resolve to live entities of the expected kind.
- Edges connect distinct live vertices and match their endpoint geometry.
- Each loop is connected and closed in topology and UV space.
- Each p-curve endpoint maps through its face plane to the corresponding oriented 3D edge endpoint.
- Each edge has exactly two coedge uses with opposite orientations.
- The shell is closed, connected, and outward oriented.
- Euler characteristic is correct for the cuboid shell.
- Analytic and independently triangulated area, volume, centroid, and bounds agree.
- Every display primitive maps to a live source face or edge.

## Test evidence

- Unit and malformed-topology validator tests.
- Transaction, stale-snapshot, cancellation, and invalid-input tests.
- JSON case and journal round trips plus deterministic replay.
- One hundred repeated constructions yield one semantic digest.
- A canonical deterministic SVG/debug-scene artifact.
- Headless UI interaction tests and focused pixel snapshots.
- Dependency audit proves no OCCT, C++, or UI dependency enters the protocol/kernel crates.

## Explicit non-claims

This does not complete M3 and does not imply support for arbitrary topology, topology editing, NURBS, intersections, booleans, sketches, feature history, persistent naming, STEP, or production tessellation. It establishes the loop through which those capabilities will later earn support.
