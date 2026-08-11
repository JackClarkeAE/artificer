#!/usr/bin/env bash
set -euo pipefail

workspace_tree="$(cargo tree --workspace --edges normal,build)"
kernel_tree="$(cargo tree --package artificer-kernel --edges normal,build)"
sketch_tree="$(cargo tree --package artificer-sketch --edges normal,build)"

if printf '%s\n' "$workspace_tree" | rg --ignore-case '(^|[^[:alnum:]_-])(occt|opencascade)([^[:alnum:]_-]|$)'; then
    printf 'error: an OCCT/OpenCascade dependency entered the product workspace\n' >&2
    exit 1
fi

if printf '%s\n' "$kernel_tree" | rg --ignore-case '(^|[^[:alnum:]_-])(egui|eframe|wgpu)([^[:alnum:]_-]|$)'; then
    printf 'error: a UI or rendering dependency entered the native kernel crate\n' >&2
    exit 1
fi

if printf '%s\n' "$sketch_tree" | rg --ignore-case '(^|[^[:alnum:]_-])(egui|eframe|wgpu|artificer-model|artificer-kernel v)([^[:alnum:]_-]|$)'; then
    printf 'error: UI, document, or B-rep dependencies entered the sketch-authoring crate\n' >&2
    exit 1
fi

if rg --files crates apps | rg '\.(c|cc|cpp|cxx|h|hh|hpp|hxx)$'; then
    printf 'error: C/C++ source entered a native product crate\n' >&2
    exit 1
fi

# Interactive model mutations are staged behind one coordinator. These exact
# counts are a narrow source-level tripwire: adding a kernel execution site or
# calling either private transaction implementation from a widget requires an
# explicit architecture-audit change and review. M5a adds one document-rebuild
# execution site; it is fed only by validated replay plans and publishes only
# after the rebuild transaction commits atomically. F2 adds one confirmed Part
# Library component-insertion site; it resolves an immutable package first and
# publishes the kernel result, component occurrence, body, and history node as
# one application transaction. F3 placement, grounding, and joint creation do
# not execute the geometry kernel, but their only model mutations must still
# remain behind the same confirmation dispatcher.
# Whole-face push/pull has its own private transactional executor because it
# consumes a face rather than a sketch, but it remains reachable only from the
# same confirmation dispatcher. The native solid-feature presets share one
# additional private executor for Revolve, Hole, Rib, Mirror, Pattern, Chamfer,
# and Fillet; persistent face/edge targets are bound before that result and its
# history node publish atomically through the same dispatcher. Three additional
# execution sites are the bounded async extrusion commit and read-only preview
# helpers; both consume immutable snapshots and cannot publish through widgets.
# Extracted presentation modules may stage intents but must never execute the
# kernel or mutate the parametric document directly.
for module in material navigation ribbon theme; do
    if rg 'NativeKernel::execute\(|execute_case\(|execute_sketch_extrusion\(|execute_face_push_pull\(|execute_library_insertion\(|apply_transform_preview\(|apply_component_placement_preview\(|apply_component_grounding\(|apply_revolute_joint\(|set_component_pose\(|set_component_grounded\(|add_joint\(' "apps/workbench/src/${module}.rs"; then
        printf 'error: a kernel-execution or document-mutation site entered the %s presentation module\n' "$module" >&2
        exit 1
    fi
done

workbench_source="apps/workbench/src/lib.rs"
workbench_gate_source="$(sed '/^#\[cfg(test)\]/,$d' "$workbench_source")"
native_execute_sites="$(printf '%s\n' "$workbench_gate_source" | rg -c 'NativeKernel::execute\(')"
case_transaction_sites="$(printf '%s\n' "$workbench_gate_source" | rg -c 'execute_case\(')"
transform_transaction_sites="$(printf '%s\n' "$workbench_gate_source" | rg -c 'apply_transform_preview\(')"
extrusion_transaction_sites="$(printf '%s\n' "$workbench_gate_source" | rg -c 'execute_sketch_extrusion\(')"
push_pull_transaction_sites="$(printf '%s\n' "$workbench_gate_source" | rg -c 'execute_face_push_pull\(')"
library_transaction_sites="$(printf '%s\n' "$workbench_gate_source" | rg -c 'execute_library_insertion\(')"
component_placement_sites="$(printf '%s\n' "$workbench_gate_source" | rg -c 'apply_component_placement_preview\(')"
component_grounding_sites="$(printf '%s\n' "$workbench_gate_source" | rg -c 'apply_component_grounding\(')"
revolute_joint_sites="$(printf '%s\n' "$workbench_gate_source" | rg -c 'apply_revolute_joint\(')"
component_pose_mutations="$(printf '%s\n' "$workbench_gate_source" | rg -c 'set_component_pose\(')"
component_grounding_mutations="$(printf '%s\n' "$workbench_gate_source" | rg -c 'set_component_grounded\(')"
joint_add_mutations="$(printf '%s\n' "$workbench_gate_source" | rg -c 'add_joint\(')"
confirm_dispatch_sites="$(printf '%s\n' "$workbench_gate_source" | rg -c 'confirm_pending_operation\(')"
cancel_dispatch_sites="$(printf '%s\n' "$workbench_gate_source" | rg -c 'cancel_pending_operation\(')"
if [[ "$native_execute_sites" -ne 10 \
    || "$case_transaction_sites" -ne 3 \
    || "$transform_transaction_sites" -ne 2 \
    || "$extrusion_transaction_sites" -ne 2 \
    || "$push_pull_transaction_sites" -ne 2 \
    || "$library_transaction_sites" -ne 2 \
    || "$component_placement_sites" -ne 2 \
    || "$component_grounding_sites" -ne 2 \
    || "$revolute_joint_sites" -ne 2 \
    || "$component_pose_mutations" -ne 1 \
    || "$component_grounding_mutations" -ne 1 \
    || "$joint_add_mutations" -ne 1 \
    || "$confirm_dispatch_sites" -ne 2 \
    || "$cancel_dispatch_sites" -ne 2 ]]; then
    printf 'error: the Kernel Lab interactive operation gate call graph changed\n' >&2
    exit 1
fi

printf 'architecture boundaries are clean\n'
