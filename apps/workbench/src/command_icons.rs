//! Vector icons for the workbench command ribbon.
//!
//! Authored in normalized 0..1 coordinates and painted at whatever size the
//! ribbon asks for, so one definition serves both the large captioned buttons
//! and the small grid buttons. This mirrors `sketch_toolbar::paint_tool_icon`
//! deliberately rather than sharing it: the sketch crate owns drawing-tool
//! iconography and must not grow a dependency on model-command vocabulary.

use egui::{Color32, Painter, Pos2, Rect, Stroke, pos2};
use std::f32::consts::TAU;

/// One command icon, drawn from strokes rather than a font or a bitmap so it
/// stays crisp at every ribbon size and follows the theme colour.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandIcon {
    Sketch,
    Plane,
    Extrude,
    Revolve,
    Hole,
    Rib,
    Mirror,
    Pattern,
    Chamfer,
    Fillet,
    Combine,
    Subtract,
    Intersect,
    Move,
    Rotate,
    Scale,
    Select,
    Measure,
    Orbit,
    Frame,
    Home,
    Edges,
    Shaded,
    Play,
    Stop,
    Library,
    Finish,
    Snap,
    Browser,
    Properties,
    History,
    Save,
    Open,
    Rebuild,
    Material,
    Theme,
}

pub fn paint_command_icon(painter: &Painter, rect: Rect, icon: CommandIcon, color: Color32) {
    IconPainter {
        painter,
        rect,
        color,
    }
    .paint(icon);
}

struct IconPainter<'a> {
    painter: &'a Painter,
    rect: Rect,
    color: Color32,
}

impl IconPainter<'_> {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        pos2(
            self.rect.left() + self.rect.width() * x,
            self.rect.top() + self.rect.height() * y,
        )
    }

    fn stroke(&self) -> Stroke {
        Stroke::new((self.rect.width() / 15.0).clamp(1.1, 1.7), self.color)
    }

    fn line(&self, a: (f32, f32), b: (f32, f32)) {
        self.painter
            .line_segment([self.p(a.0, a.1), self.p(b.0, b.1)], self.stroke());
    }

    fn path(&self, points: &[(f32, f32)]) {
        for pair in points.windows(2) {
            self.line(pair[0], pair[1]);
        }
    }

    fn closed_path(&self, points: &[(f32, f32)]) {
        self.path(points);
        if let (Some(first), Some(last)) = (points.first(), points.last()) {
            self.line(*last, *first);
        }
    }

    fn rectangle(&self, min: (f32, f32), max: (f32, f32)) {
        self.closed_path(&[
            (min.0, min.1),
            (max.0, min.1),
            (max.0, max.1),
            (min.0, max.1),
        ]);
    }

    fn filled_rectangle(&self, min: (f32, f32), max: (f32, f32)) {
        self.painter.rect_filled(
            Rect::from_two_pos(self.p(min.0, min.1), self.p(max.0, max.1)),
            0.0,
            self.color,
        );
    }

    fn dot(&self, point: (f32, f32), radius: f32) {
        self.painter.circle_filled(
            self.p(point.0, point.1),
            self.rect.width() * radius,
            self.color,
        );
    }

    fn circle(&self, centre: (f32, f32), radius: f32) {
        self.painter.circle_stroke(
            self.p(centre.0, centre.1),
            self.rect.width() * radius,
            self.stroke(),
        );
    }

    fn arc(&self, centre: (f32, f32), radius: f32, start: f32, sweep: f32) {
        let segments = 14;
        for index in 0..segments {
            let a = start + sweep * index as f32 / segments as f32;
            let b = start + sweep * (index + 1) as f32 / segments as f32;
            self.line(
                (centre.0 + radius * a.cos(), centre.1 + radius * a.sin()),
                (centre.0 + radius * b.cos(), centre.1 + radius * b.sin()),
            );
        }
    }

    fn dashed(&self, a: (f32, f32), b: (f32, f32)) {
        let segments = 5;
        for index in 0..segments {
            if index % 2 == 1 {
                continue;
            }
            let t0 = index as f32 / segments as f32;
            let t1 = (index + 1) as f32 / segments as f32;
            self.line(
                (a.0 + (b.0 - a.0) * t0, a.1 + (b.1 - a.1) * t0),
                (a.0 + (b.0 - a.0) * t1, a.1 + (b.1 - a.1) * t1),
            );
        }
    }

    /// A solid arrowhead at `tip`, pointing along `(dx, dy)`.
    fn arrowhead(&self, tip: (f32, f32), direction: (f32, f32), size: f32) {
        let length = direction.0.hypot(direction.1).max(f32::EPSILON);
        let (dx, dy) = (direction.0 / length, direction.1 / length);
        let (nx, ny) = (-dy, dx);
        self.painter.add(egui::Shape::convex_polygon(
            vec![
                self.p(tip.0, tip.1),
                self.p(
                    tip.0 - dx * size + nx * size * 0.55,
                    tip.1 - dy * size + ny * size * 0.55,
                ),
                self.p(
                    tip.0 - dx * size - nx * size * 0.55,
                    tip.1 - dy * size - ny * size * 0.55,
                ),
            ],
            self.color,
            Stroke::NONE,
        ));
    }

    fn arrow(&self, from: (f32, f32), to: (f32, f32), head: f32) {
        self.line(from, to);
        self.arrowhead(to, (to.0 - from.0, to.1 - from.1), head);
    }

    #[allow(clippy::too_many_lines)]
    fn paint(&self, icon: CommandIcon) {
        match icon {
            CommandIcon::Sketch => {
                // A drawing plane with a pencil laid across it.
                self.closed_path(&[(0.10, 0.66), (0.44, 0.82), (0.90, 0.62), (0.56, 0.46)]);
                self.path(&[(0.34, 0.44), (0.62, 0.12), (0.76, 0.24), (0.48, 0.56)]);
                self.line((0.48, 0.56), (0.34, 0.44));
                self.dot((0.44, 0.52), 0.045);
            }
            CommandIcon::Plane => {
                self.closed_path(&[(0.10, 0.62), (0.44, 0.80), (0.90, 0.56), (0.56, 0.38)]);
                self.dashed((0.50, 0.09), (0.50, 0.44));
            }
            CommandIcon::Extrude => {
                self.rectangle((0.22, 0.60), (0.78, 0.86));
                self.arrow((0.50, 0.60), (0.50, 0.14), 0.10);
            }
            CommandIcon::Revolve => {
                self.dashed((0.18, 0.08), (0.18, 0.92));
                self.closed_path(&[(0.42, 0.30), (0.72, 0.30), (0.72, 0.70), (0.42, 0.70)]);
                self.arc((0.18, 0.50), 0.40, -1.05, 2.10);
                self.arrowhead((0.38, 0.85), (0.35, 0.35), 0.09);
            }
            CommandIcon::Hole => {
                self.rectangle((0.12, 0.20), (0.88, 0.80));
                self.circle((0.50, 0.50), 0.19);
                self.dashed((0.50, 0.14), (0.50, 0.86));
            }
            CommandIcon::Rib => {
                self.line((0.16, 0.86), (0.86, 0.86));
                self.line((0.16, 0.86), (0.16, 0.16));
                self.closed_path(&[(0.22, 0.80), (0.66, 0.80), (0.22, 0.34)]);
            }
            CommandIcon::Mirror => {
                self.dashed((0.50, 0.08), (0.50, 0.92));
                self.closed_path(&[(0.12, 0.28), (0.40, 0.50), (0.12, 0.72)]);
                self.closed_path(&[(0.88, 0.28), (0.60, 0.50), (0.88, 0.72)]);
            }
            CommandIcon::Pattern => {
                for row in 0..3 {
                    for column in 0..3 {
                        let x = 0.14 + column as f32 * 0.29;
                        let y = 0.14 + row as f32 * 0.29;
                        self.rectangle((x, y), (x + 0.16, y + 0.16));
                    }
                }
            }
            CommandIcon::Chamfer => {
                self.path(&[(0.14, 0.14), (0.14, 0.62), (0.44, 0.86), (0.86, 0.86)]);
                self.dashed((0.14, 0.62), (0.14, 0.86));
                self.dashed((0.14, 0.86), (0.44, 0.86));
            }
            CommandIcon::Fillet => {
                self.line((0.14, 0.14), (0.14, 0.52));
                self.arc((0.48, 0.52), 0.34, TAU / 2.0, -TAU / 4.0);
                self.line((0.48, 0.86), (0.86, 0.86));
                self.dashed((0.14, 0.52), (0.14, 0.86));
                self.dashed((0.14, 0.86), (0.48, 0.86));
            }
            CommandIcon::Combine => {
                self.circle((0.38, 0.50), 0.27);
                self.circle((0.62, 0.50), 0.27);
                self.dot((0.50, 0.50), 0.055);
            }
            CommandIcon::Subtract => {
                self.circle((0.38, 0.50), 0.27);
                self.arc((0.62, 0.50), 0.27, -TAU / 4.0, TAU / 2.0);
                self.dashed((0.62, 0.23), (0.62, 0.77));
            }
            CommandIcon::Intersect => {
                self.arc((0.38, 0.50), 0.27, -TAU / 4.0, TAU / 2.0);
                self.arc((0.62, 0.50), 0.27, TAU / 4.0, TAU / 2.0);
            }
            CommandIcon::Move => {
                self.line((0.50, 0.14), (0.50, 0.86));
                self.line((0.14, 0.50), (0.86, 0.50));
                self.arrowhead((0.50, 0.10), (0.0, -1.0), 0.10);
                self.arrowhead((0.50, 0.90), (0.0, 1.0), 0.10);
                self.arrowhead((0.10, 0.50), (-1.0, 0.0), 0.10);
                self.arrowhead((0.90, 0.50), (1.0, 0.0), 0.10);
            }
            CommandIcon::Rotate => {
                self.arc((0.50, 0.52), 0.32, -TAU * 0.42, TAU * 0.76);
                self.arrowhead((0.72, 0.24), (0.7, -0.7), 0.11);
                self.dot((0.50, 0.52), 0.055);
            }
            CommandIcon::Scale => {
                self.rectangle((0.12, 0.56), (0.44, 0.88));
                self.rectangle((0.44, 0.16), (0.88, 0.60));
                self.arrow((0.30, 0.72), (0.72, 0.34), 0.09);
            }
            CommandIcon::Select => {
                self.closed_path(&[
                    (0.24, 0.12),
                    (0.30, 0.78),
                    (0.46, 0.61),
                    (0.58, 0.88),
                    (0.70, 0.82),
                    (0.58, 0.56),
                    (0.80, 0.52),
                ]);
            }
            CommandIcon::Measure => {
                self.rectangle((0.10, 0.36), (0.90, 0.64));
                for index in 0..4 {
                    let x = 0.26 + index as f32 * 0.16;
                    self.line((x, 0.36), (x, if index % 2 == 0 { 0.52 } else { 0.46 }));
                }
            }
            CommandIcon::Orbit => {
                self.circle((0.50, 0.50), 0.26);
                self.arc((0.50, 0.50), 0.42, 0.0, TAU);
                self.dot((0.92, 0.50), 0.055);
            }
            CommandIcon::Frame => {
                for (corner, dx, dy) in [
                    ((0.10, 0.10), 1.0, 1.0),
                    ((0.90, 0.10), -1.0, 1.0),
                    ((0.10, 0.90), 1.0, -1.0),
                    ((0.90, 0.90), -1.0, -1.0),
                ] {
                    self.line(corner, (corner.0 + 0.22 * dx, corner.1));
                    self.line(corner, (corner.0, corner.1 + 0.22 * dy));
                }
                self.rectangle((0.38, 0.38), (0.62, 0.62));
            }
            CommandIcon::Home => {
                self.path(&[(0.10, 0.50), (0.50, 0.14), (0.90, 0.50)]);
                self.path(&[(0.20, 0.44), (0.20, 0.88), (0.80, 0.88), (0.80, 0.44)]);
                self.rectangle((0.42, 0.60), (0.58, 0.88));
            }
            CommandIcon::Edges => {
                self.rectangle((0.12, 0.28), (0.66, 0.82));
                self.path(&[(0.12, 0.28), (0.36, 0.12), (0.90, 0.12), (0.66, 0.28)]);
                self.path(&[(0.90, 0.12), (0.90, 0.66), (0.66, 0.82)]);
                self.dashed((0.12, 0.82), (0.36, 0.66));
                self.dashed((0.36, 0.66), (0.90, 0.66));
            }
            CommandIcon::Shaded => {
                self.painter.add(egui::Shape::convex_polygon(
                    vec![
                        self.p(0.12, 0.28),
                        self.p(0.36, 0.12),
                        self.p(0.90, 0.12),
                        self.p(0.66, 0.28),
                    ],
                    self.color.gamma_multiply(0.55),
                    Stroke::NONE,
                ));
                self.filled_rectangle((0.12, 0.28), (0.66, 0.82));
                self.painter.add(egui::Shape::convex_polygon(
                    vec![
                        self.p(0.66, 0.28),
                        self.p(0.90, 0.12),
                        self.p(0.90, 0.66),
                        self.p(0.66, 0.82),
                    ],
                    self.color.gamma_multiply(0.35),
                    Stroke::NONE,
                ));
            }
            CommandIcon::Play => {
                self.painter.add(egui::Shape::convex_polygon(
                    vec![self.p(0.26, 0.14), self.p(0.84, 0.50), self.p(0.26, 0.86)],
                    self.color,
                    Stroke::NONE,
                ));
            }
            CommandIcon::Stop => {
                self.filled_rectangle((0.22, 0.22), (0.78, 0.78));
            }
            CommandIcon::Library => {
                self.rectangle((0.10, 0.20), (0.32, 0.86));
                self.rectangle((0.36, 0.14), (0.58, 0.86));
                self.path(&[(0.64, 0.30), (0.88, 0.22), (0.92, 0.80), (0.68, 0.88)]);
                self.line((0.64, 0.30), (0.68, 0.88));
            }
            CommandIcon::Finish => {
                self.path(&[(0.16, 0.52), (0.40, 0.78), (0.86, 0.22)]);
            }
            CommandIcon::Snap => {
                for index in 0..3 {
                    let offset = 0.22 + index as f32 * 0.28;
                    self.dashed((0.08, offset), (0.92, offset));
                    self.dashed((offset, 0.08), (offset, 0.92));
                }
                self.dot((0.50, 0.50), 0.09);
            }
            CommandIcon::Browser => {
                self.line((0.14, 0.18), (0.14, 0.80));
                for index in 0..3 {
                    let y = 0.30 + index as f32 * 0.25;
                    self.line((0.14, y), (0.34, y));
                    self.line((0.38, y), (0.88, y));
                }
                self.line((0.14, 0.18), (0.88, 0.18));
            }
            CommandIcon::Properties => {
                for index in 0..3 {
                    let y = 0.26 + index as f32 * 0.24;
                    self.line((0.12, y), (0.88, y));
                    self.dot((0.28 + index as f32 * 0.24, y), 0.075);
                }
            }
            CommandIcon::History => {
                self.arc((0.50, 0.50), 0.34, -TAU * 0.40, TAU * 0.80);
                self.arrowhead((0.28, 0.24), (-0.6, -0.8), 0.10);
                self.path(&[(0.50, 0.30), (0.50, 0.52), (0.68, 0.62)]);
            }
            CommandIcon::Save => {
                self.rectangle((0.14, 0.14), (0.86, 0.86));
                self.rectangle((0.32, 0.14), (0.68, 0.40));
                self.rectangle((0.26, 0.56), (0.74, 0.86));
            }
            CommandIcon::Open => {
                self.path(&[(0.10, 0.80), (0.10, 0.24), (0.40, 0.24), (0.48, 0.36)]);
                self.path(&[(0.48, 0.36), (0.78, 0.36), (0.78, 0.46)]);
                self.closed_path(&[(0.10, 0.80), (0.26, 0.46), (0.92, 0.46), (0.76, 0.80)]);
            }
            CommandIcon::Rebuild => {
                self.arc((0.50, 0.50), 0.32, -TAU * 0.20, TAU * 0.62);
                self.arrowhead((0.78, 0.36), (0.6, -0.8), 0.10);
                self.arc((0.50, 0.50), 0.32, TAU * 0.30, TAU * 0.62);
                self.arrowhead((0.22, 0.64), (-0.6, 0.8), 0.10);
            }
            CommandIcon::Theme => {
                // The contrast mark: one circle, half of it filled.
                self.circle((0.50, 0.50), 0.36);
                let radius = 0.36;
                let mut points = vec![self.p(0.50, 0.50 - radius)];
                let steps = 18;
                for index in 0..=steps {
                    let angle = -TAU / 4.0 + TAU / 2.0 * index as f32 / steps as f32;
                    points.push(self.p(0.50 + radius * angle.cos(), 0.50 + radius * angle.sin()));
                }
                self.painter.add(egui::Shape::convex_polygon(
                    points,
                    self.color,
                    Stroke::NONE,
                ));
            }
            CommandIcon::Material => {
                self.circle((0.50, 0.50), 0.36);
                self.arc((0.50, 0.50), 0.36, TAU * 0.06, TAU * 0.20);
                self.arc((0.36, 0.36), 0.10, 0.0, TAU);
            }
        }
    }
}
