//! Navigation presets that match the mouse conventions of other CAD packages.
//!
//! Muscle memory for orbit and pan is the hardest habit to relearn when
//! moving between CAD applications, and it is entirely arbitrary — nothing
//! about the geometry cares which button spins the view. So rather than teach
//! one convention, the workbench asks which package the user already knows and
//! adopts that package's bindings.
//!
//! Only the bindings that genuinely differ between packages are modelled:
//! which button orbits, which pans, and which way the wheel zooms. Everything
//! else stays common, and every preset leaves the left button free for
//! selection so picking never fights navigation.

/// The package whose navigation conventions the workbench should follow.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NavigationPreset {
    /// The workbench's own scheme: right-drag orbits, middle-drag pans.
    #[default]
    Artificer,
    /// Middle-drag orbits, and the wheel pushes the model away.
    MiddleOrbitInverted,
    /// Middle pans, shift with middle orbits.
    MiddlePan,
    /// As middle-pan, with the opposite wheel sense.
    MiddlePanInverted,
    /// Right orbits, middle pans.
    RightOrbit,
    /// Middle orbits, shift with middle pans.
    MiddleOrbitShiftPan,
    /// As middle-orbit-shift-pan, with the opposite wheel sense.
    MiddleOrbitShiftPanInverted,
}

/// Which mouse button, with which modifier, drives a navigation gesture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gesture {
    Right,
    Middle,
    ShiftMiddle,
    CtrlMiddle,
}

/// The resolved bindings for one preset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bindings {
    pub orbit: Gesture,
    pub pan: Gesture,
    /// `true` when scrolling forward should zoom out, which several
    /// mainstream packages do by default.
    pub invert_zoom: bool,
}

impl NavigationPreset {
    pub const ALL: [Self; 7] = [
        Self::Artificer,
        Self::MiddleOrbitInverted,
        Self::MiddlePan,
        Self::MiddlePanInverted,
        Self::RightOrbit,
        Self::MiddleOrbitShiftPan,
        Self::MiddleOrbitShiftPanInverted,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Artificer => "Artificer",
            Self::MiddleOrbitInverted => "Middle orbit · inverted zoom",
            Self::MiddlePan => "Middle pan",
            Self::MiddlePanInverted => "Middle pan · inverted zoom",
            Self::RightOrbit => "Right orbit",
            Self::MiddleOrbitShiftPan => "Middle orbit · shift pan",
            Self::MiddleOrbitShiftPanInverted => "Middle orbit · shift pan · inverted zoom",
        }
    }

    #[must_use]
    pub const fn bindings(self) -> Bindings {
        match self {
            Self::Artificer => Bindings {
                orbit: Gesture::Right,
                pan: Gesture::Middle,
                invert_zoom: false,
            },
            Self::MiddleOrbitInverted => Bindings {
                orbit: Gesture::Middle,
                pan: Gesture::CtrlMiddle,
                invert_zoom: true,
            },
            Self::MiddlePan => Bindings {
                orbit: Gesture::ShiftMiddle,
                pan: Gesture::Middle,
                invert_zoom: false,
            },
            Self::MiddlePanInverted => Bindings {
                orbit: Gesture::ShiftMiddle,
                pan: Gesture::Middle,
                invert_zoom: true,
            },
            Self::RightOrbit => Bindings {
                orbit: Gesture::Right,
                pan: Gesture::Middle,
                invert_zoom: false,
            },
            Self::MiddleOrbitShiftPan => Bindings {
                orbit: Gesture::Middle,
                pan: Gesture::ShiftMiddle,
                invert_zoom: false,
            },
            Self::MiddleOrbitShiftPanInverted => Bindings {
                orbit: Gesture::Middle,
                pan: Gesture::ShiftMiddle,
                invert_zoom: true,
            },
        }
    }

    /// A one-line description of the resulting bindings, shown beside the
    /// picker so the change is legible before it is felt.
    #[must_use]
    pub fn summary(self) -> String {
        let bindings = self.bindings();
        format!(
            "{} orbits · {} pans · wheel zooms {}",
            bindings.orbit.label(),
            bindings.pan.label(),
            if bindings.invert_zoom {
                "out when scrolled forward"
            } else {
                "in when scrolled forward"
            }
        )
    }
}

impl Gesture {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Right => "Right-drag",
            Self::Middle => "Middle-drag",
            Self::ShiftMiddle => "Shift+middle",
            Self::CtrlMiddle => "Ctrl+middle",
        }
    }

    /// Whether this gesture is active for the given drag state.
    #[must_use]
    pub const fn matches(self, state: GestureState) -> bool {
        match self {
            Self::Right => state.right,
            Self::Middle => state.middle && !state.shift && !state.ctrl,
            Self::ShiftMiddle => state.middle && state.shift,
            Self::CtrlMiddle => state.middle && state.ctrl,
        }
    }
}

/// The buttons and modifiers currently held.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GestureState {
    pub right: bool,
    pub middle: bool,
    pub shift: bool,
    pub ctrl: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_separates_orbit_from_pan() {
        for preset in NavigationPreset::ALL {
            let bindings = preset.bindings();
            assert_ne!(
                bindings.orbit,
                bindings.pan,
                "{} binds orbit and pan to the same gesture",
                preset.label()
            );
        }
    }

    #[test]
    fn a_plain_middle_drag_never_triggers_a_modified_gesture() {
        let plain = GestureState {
            middle: true,
            ..GestureState::default()
        };
        assert!(Gesture::Middle.matches(plain));
        assert!(!Gesture::ShiftMiddle.matches(plain));
        assert!(!Gesture::CtrlMiddle.matches(plain));
    }

    #[test]
    fn middle_orbit_preset_orbits_on_the_middle_button_and_inverts_the_wheel() {
        let bindings = NavigationPreset::MiddleOrbitInverted.bindings();
        assert_eq!(bindings.orbit, Gesture::Middle);
        assert!(bindings.invert_zoom);
        // The middle-pan scheme keeps the middle button for panning, a habit
        // most often trips people moving between the two.
        assert_eq!(NavigationPreset::MiddlePan.bindings().pan, Gesture::Middle);
        assert!(!NavigationPreset::MiddlePan.bindings().invert_zoom);
    }

    #[test]
    fn a_modified_drag_resolves_to_exactly_one_gesture() {
        let shifted = GestureState {
            middle: true,
            shift: true,
            ..GestureState::default()
        };
        let matching = [
            Gesture::Right,
            Gesture::Middle,
            Gesture::ShiftMiddle,
            Gesture::CtrlMiddle,
        ]
        .into_iter()
        .filter(|gesture| gesture.matches(shifted))
        .count();
        assert_eq!(matching, 1);
    }
}
