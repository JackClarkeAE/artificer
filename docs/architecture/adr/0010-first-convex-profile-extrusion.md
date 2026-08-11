# ADR 0010: First native profile extrusion is a declarative convex constructor

## Status

Accepted for the historical M4a experimental slice; the profile domain is widened by ADR 0015.

## Context

The workbench can certify one simple polyline profile, but its sketch entities are application-owned and are not B-rep topology. The first extrusion must cross the public Rust protocol, create topology bottom-up, pass the complete solid validator, emit provenance, and replay deterministically. It must not polygonize analytic circles or imply additive, cut, or selected-face editing that the topology layer cannot yet perform.

The current debug triangulator is guaranteed crack-free for a strictly convex planar cap. General concave profiles, holes, and curved boundaries require additional profile and tessellation machinery.

## Decision

Protocol version 2 adds a finite planar point, an explicit world-space planar frame, and `ExtrudePolygon`. The command accepts one three-to-256-vertex simple, strictly convex polygon with no repeated closing vertex and a finite positive distance. The kernel independently:

- bounds the serialized vertex sequence before allocation and repeats the limit for typed requests;
- validates the frame and orthonormalizes its ordered axes;
- certifies closure, winding, non-intersection, feature size, and strict convexity;
- normalizes clockwise/cyclically shifted input to one deterministic representation;
- rejects placements whose derived planes, p-curves, chords, or edge lengths cannot be represented within the active agreement policy;
- constructs bottom and top caps plus one side face per profile edge;
- validates the complete closed solid before publication;
- computes bounds, area, volume, and centroid from the B-rep;
- emits generated history for every output, including cap and indexed side-face meaning.

This command is a constructor and therefore requires an empty input snapshot. The workbench may replace its displayed diagnostic body only after successful execution. A finished workbench polyline is converted to the declarative request on confirmation, but the kernel never trusts the workbench's finished marker as evidence. The previous displayed body and pending intent remain intact on rejection.

The UI stages extrusion behind the same green tick/bare-`Enter` gate and keeps its distance visible. Its conservative preflight disables the staging action and explains concave, collinear, indeterminate, oversized, and unsupported profiles; the kernel remains the authority and independently repeats validation on confirmation. Analytic circles, arcs, holes/islands, signed/two-sided extrusion, add/cut semantics, and extrusion of a selected solid face are visibly unsupported rather than approximated.

OCCT remains an external oracle for differential fixtures only and is not linked, called, or represented inside the product implementation.

## Consequences

- This is an honest first M4 vertical slice, not completion of the M4 milestone.
- Convex cap fan triangulation is valid and shared B-rep edges are reused exactly in display geometry.
- Protocol fixtures from earlier versions move to version 2 and continue to replay deterministically.
- Extending extrusion to concave loops or holes requires a certified region representation and triangulator; selected-face extrusion additionally requires face-to-profile extraction and topology-editing semantics.
