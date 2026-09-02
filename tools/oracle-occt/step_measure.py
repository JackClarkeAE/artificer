#!/usr/bin/env python3
"""Measure a STEP file with OpenCascade: the development oracle for exact
STEP export (ADR 0001, ADR 0026 F9).

Reads one STEP file, imports it with OCCT's STEP reader, checks the shape,
and prints one JSON object:

    {"solids": 1, "valid": true, "volume": 86582.29, "area": 20288.40}

Needs the `OCP` (cadquery-ocp) or `OCC` (pythonocc-core) bindings, which are
a development-machine dependency and never a product one:

    pip install cadquery-ocp

The kernel's `step_export_tests` run this when `ARTIFICER_STEP_ORACLE` names
a command (this script, or anything else that prints the same JSON) and
compare volume and area to the kernel's exact measures at 1e-9 relative.
"""

import json
import sys


def load_occt():
    try:
        from OCP.STEPControl import STEPControl_Reader
        from OCP.IFSelect import IFSelect_RetDone
        from OCP.BRepCheck import BRepCheck_Analyzer
        from OCP.GProp import GProp_GProps
        from OCP.BRepGProp import BRepGProp
        from OCP.TopAbs import TopAbs_SOLID
        from OCP.TopExp import TopExp_Explorer

        def volume_props(shape, props):
            BRepGProp.VolumeProperties_s(shape, props)

        def surface_props(shape, props):
            BRepGProp.SurfaceProperties_s(shape, props)

        return (
            STEPControl_Reader,
            IFSelect_RetDone,
            BRepCheck_Analyzer,
            GProp_GProps,
            volume_props,
            surface_props,
            TopAbs_SOLID,
            TopExp_Explorer,
        )
    except ImportError:
        from OCC.Core.STEPControl import STEPControl_Reader
        from OCC.Core.IFSelect import IFSelect_RetDone
        from OCC.Core.BRepCheck import BRepCheck_Analyzer
        from OCC.Core.GProp import GProp_GProps
        from OCC.Core.BRepGProp import brepgprop
        from OCC.Core.TopAbs import TopAbs_SOLID
        from OCC.Core.TopExp import TopExp_Explorer

        def volume_props(shape, props):
            brepgprop.VolumeProperties(shape, props)

        def surface_props(shape, props):
            brepgprop.SurfaceProperties(shape, props)

        return (
            STEPControl_Reader,
            IFSelect_RetDone,
            BRepCheck_Analyzer,
            GProp_GProps,
            volume_props,
            surface_props,
            TopAbs_SOLID,
            TopExp_Explorer,
        )


def main(path):
    (
        STEPControl_Reader,
        IFSelect_RetDone,
        BRepCheck_Analyzer,
        GProp_GProps,
        volume_props,
        surface_props,
        TopAbs_SOLID,
        TopExp_Explorer,
    ) = load_occt()
    reader = STEPControl_Reader()
    status = reader.ReadFile(path)
    if status != IFSelect_RetDone:
        print(json.dumps({"error": f"STEP reader status {status}"}))
        return 1
    reader.TransferRoots()
    shape = reader.OneShape()
    solids = 0
    explorer = TopExp_Explorer(shape, TopAbs_SOLID)
    while explorer.More():
        solids += 1
        explorer.Next()
    valid = BRepCheck_Analyzer(shape).IsValid()
    volume = GProp_GProps()
    volume_props(shape, volume)
    area = GProp_GProps()
    surface_props(shape, area)
    print(
        json.dumps(
            {
                "solids": solids,
                "valid": bool(valid),
                "volume": volume.Mass(),
                "area": area.Mass(),
            }
        )
    )
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: step_measure.py <file.step>", file=sys.stderr)
        sys.exit(2)
    sys.exit(main(sys.argv[1]))
