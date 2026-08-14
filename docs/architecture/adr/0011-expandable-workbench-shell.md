# ADR 0011: Expandable workbench shell preserves a fixed confirmation rail

Status: Accepted for the M4b shell and confirmation rail; the history-preview authority boundary is superseded by [ADR 0014](0014-m5a-parametric-document-foundation.md)
- Date: 2026-07-29
- Decision owners: Artificer project

## Context

> **Supersession note:** this ADR remains authoritative for the expandable shell and fixed confirmation rail. Its session-local, read-only feature-preview boundary described the M4b state and was replaced by the document-backed Browser and History decision in [ADR 0014](0014-m5a-parametric-document-foundation.md).

The first workbench slices used fixed side panels that mixed workspace switching, object navigation, command input, diagnostics, and development cases. That was adequate for one cuboid and one profile, but it does not provide a stable place for a growing catalogue of sketch, feature, inspection, and history tools. Simply adding more controls would progressively reduce the viewport and make the model-operation boundary harder to find.

M4 also needs a more product-like development surface before M5 supplies a real parametric document. The shell must therefore be useful and expandable without presenting a decorative feature timeline as if rollback, regeneration, suppression, or dependency editing already existed.

Autodesk's published Fusion interface is an interaction reference for separating the application bar, contextual toolbar, object browser, canvas, navigation controls, and timeline. Its API documentation also describes workspace-specific toolbar tabs and panels, resizable/dockable palettes, collapsible command-input groups, and command dialogs whose acceptance stays disabled until input is valid. These are product-design precedents only. Artificer reimplements its shell in native Rust/egui and does not reuse Autodesk code, visual assets, branding, product names, or geometry technology.

## Decision

The kernel lab adopts a presentation-owned workbench shell with these stable regions:

- a compact application header containing the document identity, Model/Sketch workspace switch, status, and panel-visibility controls;
- an expandable command ribbon whose groups and enabled commands follow the active workspace;
- a resizable left model browser with nested document and origin content, which collapses to a narrow expansion rail;
- a resizable right contextual inspector, labelled as Properties or Sketch Palette according to the active workspace, with independently collapsible input and diagnostic sections;
- a central viewport that retains the remaining space and owns model or sketch interaction;
- a collapsible bottom committed-feature preview; and
- a fixed confirmation rail at the outer bottom edge.

Shell visibility and panel widths are presentation state. Showing, hiding, expanding, collapsing, or resizing a region is immediate and cannot execute a kernel command, commit a sketch artifact, change a snapshot, or enter the model-operation gate. The current visibility defaults are exposed for semantic UI tests and later preference persistence; M4b does not yet promise saved workspace layouts, detachable windows, toolbar pinning, or automatic breakpoint-driven rearrangement.

The command ribbon groups the small current command set by intent. Model mode presents Create, Modify, Select/View, and Motion groups. Sketch mode presents Select, Create, Complete, and View groups. A stable Solid group keeps Extrude visible in both workspaces and disables it until the current sketch is finished and eligible, avoiding a moving primary command. Operation-specific values and diagnostics remain in the contextual inspector. Ribbon groups must retain their full vertical hit regions at the supported 1040×700 minimum window. This is the first registry-shaped layout boundary, not a claim that the full future command catalogue or user-customizable ribbon is implemented.

### Confirmation-rail invariant

The universal confirmation contract from [ADR 0007](0007-universal-model-operation-confirmation.md) is elevated to a shell invariant:

- the confirmation rail always reserves the same layout space, including when no operation is pending;
- it cannot be collapsed, hidden, resized, or replaced by the command ribbon, browser, inspector, feature preview, or viewport;
- every interactive modeling operation continues to stage one shared pending intent before the rail can confirm it;
- valid pending intent exposes only a compact green tick square and bare `Enter`; cancellation remains a compact red cross square and `Escape`; invalid intent cannot be confirmed;
- collapsing the inspector that contains an operation's editable values does not cancel, execute, or silently alter that operation; and
- no command-group or panel-local button may bypass the central confirmation dispatcher.

The rail is intentionally separate from operation-specific input. The user can reclaim screen space without losing sight of whether an action will change model truth, and showing or hiding chrome cannot move the commit boundary.

> **Model-workspace note:** [ADR 0028](0028-workbench-command-registry-and-contextual-properties.md) reverses this separation for the model workspace, where an operation's inputs and its tick are now one surface, and moves the rail off the outer bottom edge. Everything the invariant protects — one staged intent, no confirmation of invalid intent, no panel-local bypass, no cancellation by collapsing chrome — is unchanged, and the sketch workspace still reads exactly as written here.

### Historical M4b feature-preview boundary (superseded)

At the time of M4b, the bottom strip was a **read-only committed-feature preview until M5**. A presentation-owned, session-local ledger preserved successful Sketch, Extrude, Add, Cut, and Transform entries after Origin and Base body. Staging, rejection, and cancellation did not append entries. Beginning a later sketch did not erase earlier committed entries. Its chips could navigate to the current Model or active Sketch view, but did not mutate model truth.

That M4b ledger was not persisted and was not a feature DAG or authoritative command journal. It had no replay payloads, dependency edges, rollback marker, dirty propagation, recomputation, reorder, suppression, feature editing, undo/redo, or persistent-reference resolution. ADR 0014 records the gate that replaced it: the document layer now owns feature identity, dependency and replay state, while the bottom strip is only a projection. Reorder and general feature editing remain outside the accepted M5a subset.

## Verification

- Semantic shell tests cover independent expansion and collapse of the command ribbon, browser, inspector, and committed-feature preview.
- Supported minimum-window tests keep expansion controls, the complete ribbon hit regions, persistent Extrude, and the confirmation rail reachable in every panel state.
- Pending-operation tests assert that collapsing or expanding shell regions neither executes nor discards the staged operation.
- Accessibility queries use stable labels for expansion controls, Browser/Properties regions, icon-only confirmation actions, and the preview boundary.
- Feature-preview tests prove that only successful commits append, multiple transforms accumulate, and earlier features survive entry into a later sketch.
- Visual snapshots cover representative expanded, collapsed, and supported-minimum layouts without treating the committed-feature preview as M5 regeneration evidence.

## Consequences

- New commands have predictable homes while the viewport remains the centre of the application.
- Dense diagnostics and secondary controls can be hidden without weakening the universal confirmation contract.
- The Browser and Properties panels can grow incrementally around stable entity and operation identities.
- The bottom strip provided useful M4b orientation without a false parametric-history claim; ADR 0014 now governs its document-backed authority boundary.
- A future command registry, saved layouts, compact-window policy, true feature editor, and drag/reorder behaviour remain separately gated work.

## Design references

- [Autodesk Fusion desktop interface](https://help.autodesk.com/view/fusion360/ENU/?contextId=LP-STEPS-P13N-SNP-GS-OTH-CRD-1)
- [Autodesk Fusion UI structure and toolbar panels](https://help.autodesk.com/cloudhelp/ENU/Fusion-360-API/files/UserInterface_UM.htm)
- [Autodesk Fusion command inputs and collapsible groups](https://help.autodesk.com/cloudhelp/ENU/Fusion-360-API/files/CommandInputs_UM.htm)
- [Autodesk Fusion palettes and docking](https://help.autodesk.com/cloudhelp/ENU/Fusion-360-API/files/Palettes_UM.htm)
- [Autodesk Fusion timeline behaviour](https://help.autodesk.com/view/fusion360/ENU/?contextId=LP-STEPS-P13N-SNP-GS-OTH-CRD-2)
