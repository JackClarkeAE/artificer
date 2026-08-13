# ADR 0001: Native Rust kernel with an external development oracle

Status: Accepted
- Date: 2026-07-28
- Decision owners: Artificer project

## Context

Artificer's long-term goal is a custom geometry and B-rep kernel that integrates naturally with a Rust application. Building robust intersections, booleans, blends, healing, and STEP support will take many capability iterations. Blocking all product and UI learning until the native kernel reaches broad coverage would delay feedback and encourage untested interfaces.

Open CASCADE Technology (OCCT) 8.0 is a current open-source C++ B-rep platform with geometry, topology, modeling algorithms, healing, tessellation, exchange, and a command-driven test harness. It is licensed under LGPL-2.1 with an additional exception. The `truck` project is a valuable Apache-2.0 Rust architecture/reference, but its published shape-operation and STEP coverage is not yet equivalent to a production commercial kernel.

## Decision

Artificer will own a narrow, semantic, versioned Rust kernel protocol and one product implementation:

1. `NativeKernel`: the permanent Rust implementation and source of truth for native capability claims.
2. `OcctOracle`: an optional development executable used only by differential tests and offline comparison.

The native implementation and oracle consume equivalent declarative case intent and produce semantically comparable geometric outcomes. The oracle is not a `Kernel` implementation, cannot execute product commands, and cannot create geometry for the UI or saved Artificer documents. Artificer validates native provenance and diagnostics against its own contracts. Oracle entity history, diagnostic detail, and topology decomposition are best-effort evidence only where OCCT exposes compatible meaning; they are never protocol requirements.

### Boundary rules

- OCCT classes, handles, serialization, exceptions, IDs, tolerance conventions, headers, and libraries do not appear in or link into any Artificer product crate, binary, document, renderer, or UI.
- The oracle is a separate development tool under `tools/oracle-occt`; it communicates only through versioned test-case and result files or a child-process test protocol.
- Native Rust builds and product packaging never require OCCT to compile, test the non-differential suite, run, import/export, or reopen a document.
- Product code has no runtime fallback from native operations to the oracle. Unsupported native operations return Artificer's structured `Unsupported` result.
- The OCCT version and build options are pinned in recorded differential evidence.
- Reference differences are adjudicated semantically; the native implementation does not blindly copy reference topology or defects.
- Native capability status is explicit per operation: unavailable, experimental, beta, or stable.
- CI jobs that use OCCT are optional, separately labelled oracle jobs; the required native suite proves it does not depend on them.

## Consequences

### Benefits

- The differential corpus provides independent evidence without creating product lock-in.
- Artificer owns its domain model, operation semantics, topology, algorithms, persistence, and failure behaviour.
- Removing the oracle does not remove any product capability.
- Real-world STEP fixtures may be inspected with OCCT during test adjudication, while native import/export remains an Artificer milestone.

### Costs and risks

- Oracle normalization and semantic comparison require extra test engineering.
- OCCT may accept/reject or regularize cases differently, so comparison logic must not depend on raw face counts.
- The product remains unavailable for operations the native kernel has not implemented.
- Development environments and CI that install/run OCCT must still comply with its license; Artificer product artifacts do not distribute or link it.
- A lowest-common-denominator protocol would inhibit native design. The protocol therefore expresses Artificer semantics, not a wrapper around OCCT methods.

## Alternatives considered

### Native Rust only from day one

This maximizes purity and minimizes dependency policy, but discards a strong independent differential oracle. Rejected: the oracle adds test evidence without entering the product.

### Adopt OCCT as the permanent kernel

This is the shortest open-source path to a capable CAD product, but does not satisfy the long-term custom-kernel goal and couples core behaviour to a C++ implementation and its licensing/evolution.

### Base the permanent kernel directly on `truck`

This would accelerate some Rust geometry/topology work, but currently transfers external public types and coverage constraints into Artificer. `truck` remains a valuable permissive reference and possible source of isolated components after focused evaluation.

### License Parasolid or ACIS/CGM

This is the shortest route to industrial breadth if funding and commercial terms permit. It remains a product option, but it is not a plan for creating a custom kernel.

## Boundary verification

- A clean native build succeeds when OCCT is absent.
- Required unit, integration, replay, UI, and native interchange tests succeed when OCCT is absent.
- Product dependency and binary audits contain no OCCT library, symbol, header path, or package.
- No application command path can address or launch `tools/oracle-occt`.
- Every oracle case can be retained as native case data without retaining an OCCT-generated product model.

## Sources

- [Open CASCADE 8.0 release](https://dev.opencascade.org/release)
- [Open CASCADE licensing](https://dev.opencascade.org/resources/licensing)
- [Open CASCADE architecture overview](https://dev.opencascade.org/doc/overview/html/)
- [`truck` repository and crate map](https://github.com/ricosjp/truck)
- [`truck-stepio` current status](https://docs.rs/truck-stepio/latest/truck_stepio/)
