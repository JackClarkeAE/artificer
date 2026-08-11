//! Shared pointer capture for every direct-manipulation handle.
//!
//! Modeling handles must retain ownership after the pointer leaves their
//! visible marker and must not depend on whichever overlapping face widget
//! egui happened to resolve first.  This small state machine is intentionally
//! geometry-agnostic: callers provide the initial hit result and consume
//! screen-space deltas in their own coordinate system.

use egui::{PointerButton, Pos2, Rect, Ui, Vec2};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DragHandlePhase {
    Started,
    #[default]
    Dragging,
    Finished,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointerSample {
    pub position: Option<Pos2>,
    pub pressed: bool,
    pub down: bool,
    pub released: bool,
    pub in_bounds: bool,
}

impl PointerSample {
    pub fn primary(ui: &Ui, bounds: Rect) -> Self {
        let (position, pressed, down, released) = ui.input(|input| {
            (
                input.pointer.interact_pos(),
                input.pointer.button_pressed(PointerButton::Primary),
                input.pointer.button_down(PointerButton::Primary),
                input.pointer.button_released(PointerButton::Primary),
            )
        });
        Self {
            position,
            pressed,
            down,
            released,
            in_bounds: position.is_some_and(|position| bounds.contains(position)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DragHandleEvent {
    pub phase: DragHandlePhase,
    pub position: Pos2,
    pub frame_delta: Vec2,
    pub total_delta: Vec2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DragHandleInteraction {
    pub event: Option<DragHandleEvent>,
    pub hovered: bool,
    pub consumes_primary: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DragHandleState {
    origin: Option<Pos2>,
    previous: Option<Pos2>,
}

impl DragHandleState {
    pub const fn is_active(self) -> bool {
        self.origin.is_some()
    }

    pub fn cancel(&mut self) {
        self.origin = None;
        self.previous = None;
    }

    pub fn update(&mut self, sample: PointerSample, initial_hit: bool) -> DragHandleInteraction {
        let hovered = sample.in_bounds && initial_hit;
        let mut event = None;

        if !self.is_active() && sample.pressed && hovered {
            let position = sample
                .position
                .expect("a hit pointer sample must carry a position");
            self.origin = Some(position);
            self.previous = Some(position);
            event = Some(DragHandleEvent {
                phase: DragHandlePhase::Started,
                position,
                frame_delta: Vec2::ZERO,
                total_delta: Vec2::ZERO,
            });
        }

        if let Some(origin) = self.origin {
            if (sample.down || sample.released)
                && let Some(position) = sample.position
            {
                let previous = self.previous.unwrap_or(origin);
                self.previous = Some(position);
                if event.is_none() {
                    event = Some(DragHandleEvent {
                        phase: if sample.released {
                            DragHandlePhase::Finished
                        } else {
                            DragHandlePhase::Dragging
                        },
                        position,
                        frame_delta: position - previous,
                        total_delta: position - origin,
                    });
                } else if sample.released
                    && let Some(started) = event.as_mut()
                {
                    started.phase = DragHandlePhase::Finished;
                }
            }

            // Losing a platform release event must not leave the model locked
            // in a permanent drag. Preserve the last sampled point and emit a
            // deterministic finish on the first idle frame.
            if sample.released || (!sample.down && !sample.pressed) {
                let position = sample.position.or(self.previous).unwrap_or(origin);
                let previous = self.previous.unwrap_or(origin);
                event = Some(DragHandleEvent {
                    phase: DragHandlePhase::Finished,
                    position,
                    frame_delta: position - previous,
                    total_delta: position - origin,
                });
                self.cancel();
            }
        }

        let finished = event.is_some_and(|event| event.phase == DragHandlePhase::Finished);
        DragHandleInteraction {
            event,
            hovered: hovered || self.is_active(),
            consumes_primary: self.is_active() || finished,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_survives_leaving_bounds_and_finishes_once() {
        let mut state = DragHandleState::default();
        let started = state.update(
            PointerSample {
                position: Some(Pos2::new(10.0, 10.0)),
                pressed: true,
                down: true,
                in_bounds: true,
                ..PointerSample::default()
            },
            true,
        );
        assert_eq!(started.event.unwrap().phase, DragHandlePhase::Started);
        assert!(started.consumes_primary);

        let dragged = state.update(
            PointerSample {
                position: Some(Pos2::new(35.0, -5.0)),
                down: true,
                in_bounds: false,
                ..PointerSample::default()
            },
            false,
        );
        assert_eq!(dragged.event.unwrap().total_delta, Vec2::new(25.0, -15.0));
        assert!(dragged.consumes_primary);

        let finished = state.update(
            PointerSample {
                position: Some(Pos2::new(40.0, -5.0)),
                released: true,
                in_bounds: false,
                ..PointerSample::default()
            },
            false,
        );
        assert_eq!(finished.event.unwrap().phase, DragHandlePhase::Finished);
        assert!(finished.consumes_primary);
        assert!(!state.is_active());
    }
}
