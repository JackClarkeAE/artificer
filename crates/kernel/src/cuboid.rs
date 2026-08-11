use crate::topology::{
    Coedge, CoedgeKey, Edge, EdgeKey, EntityId, Face, FaceKey, FaceRole, Loop, LoopKey,
    Orientation, Plane, Point3, Record, Shell, ShellKey, Solid, Surface, Topology, Vector3, Vertex,
    VertexKey,
};

#[derive(Clone, Copy)]
struct FaceDefinition {
    role: FaceRole,
    plane: Plane,
    uses: [(EdgeKey, Orientation); 4],
}

pub(crate) fn build_cuboid(origin: Point3, size: Vector3) -> Topology {
    let x0 = origin.x;
    let y0 = origin.y;
    let z0 = origin.z;
    let x1 = x0 + size.x;
    let y1 = y0 + size.y;
    let z1 = z0 + size.z;

    let points = [
        Point3::new(x0, y0, z0),
        Point3::new(x1, y0, z0),
        Point3::new(x1, y1, z0),
        Point3::new(x0, y1, z0),
        Point3::new(x0, y0, z1),
        Point3::new(x1, y0, z1),
        Point3::new(x1, y1, z1),
        Point3::new(x0, y1, z1),
    ];

    let mut next_entity_id = 1_u64;
    let mut allocate_id = || {
        let id = EntityId::from_raw(next_entity_id);
        next_entity_id += 1;
        id
    };

    let vertices = points
        .iter()
        .copied()
        .map(|point| Record {
            id: allocate_id(),
            value: Vertex { point },
        })
        .collect::<Vec<_>>();

    let edge_vertex_indices = [
        [0, 1],
        [1, 2],
        [2, 3],
        [3, 0],
        [4, 5],
        [5, 6],
        [6, 7],
        [7, 4],
        [0, 4],
        [1, 5],
        [2, 6],
        [3, 7],
    ];

    let edges = edge_vertex_indices
        .iter()
        .map(|indices| Record {
            id: allocate_id(),
            value: Edge::line(
                [VertexKey(indices[0]), VertexKey(indices[1])],
                [points[indices[0]], points[indices[1]]],
            ),
        })
        .collect::<Vec<_>>();

    let x_axis = Vector3::new(1.0, 0.0, 0.0);
    let y_axis = Vector3::new(0.0, 1.0, 0.0);
    let z_axis = Vector3::new(0.0, 0.0, 1.0);

    // Every loop is counter-clockwise when viewed from outside the solid.
    let definitions = [
        FaceDefinition {
            role: FaceRole::NegativeZ,
            plane: Plane::new(points[0], y_axis, x_axis),
            uses: [
                (EdgeKey(3), Orientation::Reverse),
                (EdgeKey(2), Orientation::Reverse),
                (EdgeKey(1), Orientation::Reverse),
                (EdgeKey(0), Orientation::Reverse),
            ],
        },
        FaceDefinition {
            role: FaceRole::PositiveZ,
            plane: Plane::new(points[4], x_axis, y_axis),
            uses: [
                (EdgeKey(4), Orientation::Forward),
                (EdgeKey(5), Orientation::Forward),
                (EdgeKey(6), Orientation::Forward),
                (EdgeKey(7), Orientation::Forward),
            ],
        },
        FaceDefinition {
            role: FaceRole::NegativeY,
            plane: Plane::new(points[0], x_axis, z_axis),
            uses: [
                (EdgeKey(0), Orientation::Forward),
                (EdgeKey(9), Orientation::Forward),
                (EdgeKey(4), Orientation::Reverse),
                (EdgeKey(8), Orientation::Reverse),
            ],
        },
        FaceDefinition {
            role: FaceRole::PositiveY,
            plane: Plane::new(points[3], z_axis, x_axis),
            uses: [
                (EdgeKey(11), Orientation::Forward),
                (EdgeKey(6), Orientation::Reverse),
                (EdgeKey(10), Orientation::Reverse),
                (EdgeKey(2), Orientation::Forward),
            ],
        },
        FaceDefinition {
            role: FaceRole::NegativeX,
            plane: Plane::new(points[0], z_axis, y_axis),
            uses: [
                (EdgeKey(8), Orientation::Forward),
                (EdgeKey(7), Orientation::Reverse),
                (EdgeKey(11), Orientation::Reverse),
                (EdgeKey(3), Orientation::Forward),
            ],
        },
        FaceDefinition {
            role: FaceRole::PositiveX,
            plane: Plane::new(points[1], y_axis, z_axis),
            uses: [
                (EdgeKey(1), Orientation::Forward),
                (EdgeKey(10), Orientation::Forward),
                (EdgeKey(5), Orientation::Reverse),
                (EdgeKey(9), Orientation::Reverse),
            ],
        },
    ];

    let mut coedges = Vec::with_capacity(24);
    let mut loops = Vec::with_capacity(6);
    let mut faces = Vec::with_capacity(6);

    for definition in definitions {
        let mut loop_coedges = Vec::with_capacity(4);
        for (edge_key, orientation) in definition.uses {
            let edge = &edges[edge_key.0].value;
            let endpoints = edge.endpoints();
            let curve_endpoints = match orientation {
                Orientation::Forward => endpoints,
                Orientation::Reverse => [endpoints[1], endpoints[0]],
            };
            let coedge_key = CoedgeKey(coedges.len());
            coedges.push(Record {
                id: allocate_id(),
                value: Coedge::line(
                    edge_key,
                    orientation,
                    [
                        definition.plane.project(curve_endpoints[0]),
                        definition.plane.project(curve_endpoints[1]),
                    ],
                ),
            });
            loop_coedges.push(coedge_key);
        }

        let loop_key = LoopKey(loops.len());
        loops.push(Record {
            id: allocate_id(),
            value: Loop {
                coedges: loop_coedges,
            },
        });
        faces.push(Record {
            id: allocate_id(),
            value: Face {
                surface: Surface::Plane(definition.plane),
                outer_loop: loop_key,
                inner_loops: Vec::new(),
                role: definition.role,
            },
        });
    }

    let shell_key = ShellKey(0);
    let shells = vec![Record {
        id: allocate_id(),
        value: Shell {
            faces: (0..faces.len()).map(FaceKey).collect(),
        },
    }];
    let solids = vec![Record {
        id: allocate_id(),
        value: Solid {
            outer_shell: shell_key,
            inner_shells: Vec::new(),
        },
    }];

    Topology {
        vertices,
        edges,
        coedges,
        loops,
        faces,
        shells,
        solids,
    }
}
