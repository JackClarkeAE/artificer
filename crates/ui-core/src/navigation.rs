//! Navigation profiles that match the mouse conventions of other CAD packages.
//!
//! Muscle memory for orbit and pan is the hardest habit to relearn when
//! moving between CAD applications, and it is entirely arbitrary — nothing
//! about the geometry cares which button spins the view. So rather than teach
//! one convention, the workbench asks which package the user already knows and
//! adopts that package's bindings by name: someone familiar with Fusion 360,
//! Inventor, SolidWorks, Onshape, Creo, or NX picks their package and keeps
//! their hands.
//!
//! A profile models the bindings that genuinely differ between packages:
//! which button orbits, which pans, which (if any) drags to zoom, which way
//! the wheel zooms, and the hold-to-navigate keys Inventor users expect (F4
//! orbits, F2 pans, F3 zooms). Everything else stays common, and every
//! profile leaves an unmodified left button free for selection so picking
//! never fights navigation.

/// The package whose navigation conventions the workbench should follow.
///
/// The serde aliases keep documents and preference files written before the
/// profiles were named after packages loading onto the profile with the same
/// bindings.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NavigationPreset {
    /// The workbench's own scheme: right-drag orbits, middle-drag pans.
    #[default]
    #[serde(alias = "right-orbit")]
    Artificer,
    /// Fusion 360: middle pans, Shift+middle orbits, wheel forward zooms out.
    #[serde(
        alias = "middle-pan",
        alias = "middle-pan-inverted",
        alias = "fusion360"
    )]
    Fusion,
    /// Inventor: as Fusion, plus hold F4 to orbit, F2 to pan, F3 to zoom.
    Inventor,
    /// SolidWorks: middle orbits, Ctrl+middle pans, Shift+middle drag zooms,
    /// wheel forward zooms out.
    #[serde(alias = "middle-orbit-inverted", alias = "solidworks")]
    SolidWorks,
    /// Onshape: right orbits, middle or Ctrl+right pans.
    Onshape,
    /// Creo: middle orbits, Shift+middle pans, Ctrl+middle drag zooms,
    /// wheel forward zooms out.
    #[serde(alias = "middle-orbit-shift-pan-inverted")]
    Creo,
    /// NX: middle orbits, Shift+middle pans, Ctrl+middle drag zooms.
    #[serde(alias = "middle-orbit-shift-pan")]
    Nx,
}

/// Which mouse button, with which modifier, drives a navigation gesture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gesture {
    Right,
    CtrlRight,
    Middle,
    ShiftMiddle,
    CtrlMiddle,
}

/// A keyboard key that, held down, turns a plain left drag into a navigation
/// gesture. These are the Inventor F2/F3/F4 conventions; other profiles carry
/// none.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HoldKey {
    F2,
    F3,
    F4,
}

impl HoldKey {
    #[must_use]
    pub const fn key(self) -> egui::Key {
        match self {
            Self::F2 => egui::Key::F2,
            Self::F3 => egui::Key::F3,
            Self::F4 => egui::Key::F4,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
        }
    }

    const fn is_down(self, state: GestureState) -> bool {
        match self {
            Self::F2 => state.f2,
            Self::F3 => state.f3,
            Self::F4 => state.f4,
        }
    }
}

/// The navigation gesture a drag currently resolves to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NavigationAction {
    Orbit,
    Pan,
    /// A drag that zooms: up zooms in, down zooms out.
    ZoomDrag,
}

/// The resolved bindings for one profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bindings {
    pub orbit: Gesture,
    pub pan: Gesture,
    /// A second pan gesture, for packages that accept two (Onshape pans with
    /// the middle button or Ctrl+right).
    pub pan_alternate: Option<Gesture>,
    /// A drag gesture that zooms rather than orbits or pans.
    pub zoom_drag: Option<Gesture>,
    /// `true` when scrolling forward should zoom out, which several
    /// mainstream packages do by default.
    pub invert_zoom: bool,
    /// Hold-to-orbit key applied to a plain left drag.
    pub orbit_key: Option<HoldKey>,
    /// Hold-to-pan key applied to a plain left drag.
    pub pan_key: Option<HoldKey>,
    /// Hold-to-zoom key applied to a plain left drag.
    pub zoom_key: Option<HoldKey>,
}

impl Bindings {
    /// Resolves the drag state to at most one navigation action. Hold keys
    /// take precedence over button gestures so that, say, Inventor's F4 wins
    /// even while a button is also down; the zoom drag is tested before orbit
    /// and pan because its gesture is always the more modified one.
    #[must_use]
    pub fn action(self, state: GestureState) -> Option<NavigationAction> {
        if state.primary {
            for (key, action) in [
                (self.orbit_key, NavigationAction::Orbit),
                (self.pan_key, NavigationAction::Pan),
                (self.zoom_key, NavigationAction::ZoomDrag),
            ] {
                if key.is_some_and(|key| key.is_down(state)) {
                    return Some(action);
                }
            }
        }
        if self.zoom_drag.is_some_and(|gesture| gesture.matches(state)) {
            return Some(NavigationAction::ZoomDrag);
        }
        if self.orbit.matches(state) {
            return Some(NavigationAction::Orbit);
        }
        if self.pan.matches(state)
            || self
                .pan_alternate
                .is_some_and(|gesture| gesture.matches(state))
        {
            return Some(NavigationAction::Pan);
        }
        None
    }

    /// Whether any hold-to-navigate key is currently down, so callers can
    /// tell an intentional navigation drag from a selection drag.
    #[must_use]
    pub fn hold_key_down(self, state: GestureState) -> bool {
        [self.orbit_key, self.pan_key, self.zoom_key]
            .into_iter()
            .flatten()
            .any(|key| key.is_down(state))
    }
}

impl NavigationPreset {
    pub const ALL: [Self; 7] = [
        Self::Artificer,
        Self::Fusion,
        Self::Inventor,
        Self::SolidWorks,
        Self::Onshape,
        Self::Creo,
        Self::Nx,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Artificer => "Artificer",
            Self::Fusion => "Familiar with Fusion 360",
            Self::Inventor => "Familiar with Inventor",
            Self::SolidWorks => "Familiar with SolidWorks",
            Self::Onshape => "Familiar with Onshape",
            Self::Creo => "Familiar with Creo",
            Self::Nx => "Familiar with NX",
        }
    }

    #[must_use]
    pub const fn bindings(self) -> Bindings {
        const COMMON: Bindings = Bindings {
            orbit: Gesture::Right,
            pan: Gesture::Middle,
            pan_alternate: None,
            zoom_drag: None,
            invert_zoom: false,
            orbit_key: None,
            pan_key: None,
            zoom_key: None,
        };
        match self {
            Self::Artificer => COMMON,
            Self::Fusion => Bindings {
                orbit: Gesture::ShiftMiddle,
                pan: Gesture::Middle,
                invert_zoom: true,
                ..COMMON
            },
            Self::Inventor => Bindings {
                orbit: Gesture::ShiftMiddle,
                pan: Gesture::Middle,
                invert_zoom: true,
                orbit_key: Some(HoldKey::F4),
                pan_key: Some(HoldKey::F2),
                zoom_key: Some(HoldKey::F3),
                ..COMMON
            },
            Self::SolidWorks => Bindings {
                orbit: Gesture::Middle,
                pan: Gesture::CtrlMiddle,
                zoom_drag: Some(Gesture::ShiftMiddle),
                invert_zoom: true,
                ..COMMON
            },
            Self::Onshape => Bindings {
                orbit: Gesture::Right,
                pan: Gesture::Middle,
                pan_alternate: Some(Gesture::CtrlRight),
                ..COMMON
            },
            Self::Creo => Bindings {
                orbit: Gesture::Middle,
                pan: Gesture::ShiftMiddle,
                zoom_drag: Some(Gesture::CtrlMiddle),
                invert_zoom: true,
                ..COMMON
            },
            Self::Nx => Bindings {
                orbit: Gesture::Middle,
                pan: Gesture::ShiftMiddle,
                zoom_drag: Some(Gesture::CtrlMiddle),
                ..COMMON
            },
        }
    }

    /// A one-line description of the resulting bindings, shown beside the
    /// picker so the change is legible before it is felt.
    #[must_use]
    pub fn summary(self) -> String {
        let bindings = self.bindings();
        let mut parts = vec![
            format!("{} orbits", bindings.orbit.label()),
            match bindings.pan_alternate {
                Some(alternate) => {
                    format!("{} or {} pans", bindings.pan.label(), alternate.label())
                }
                None => format!("{} pans", bindings.pan.label()),
            },
        ];
        if let Some(zoom) = bindings.zoom_drag {
            parts.push(format!("{} zooms", zoom.label()));
        }
        if let (Some(orbit), Some(pan), Some(zoom)) =
            (bindings.orbit_key, bindings.pan_key, bindings.zoom_key)
        {
            parts.push(format!(
                "hold {} to orbit, {} to pan, {} to zoom",
                orbit.label(),
                pan.label(),
                zoom.label()
            ));
        }
        parts.push(
            if bindings.invert_zoom {
                "wheel zooms out when scrolled forward"
            } else {
                "wheel zooms in when scrolled forward"
            }
            .to_owned(),
        );
        parts.join(" · ")
    }
}

impl Gesture {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Right => "Right-drag",
            Self::CtrlRight => "Ctrl+right",
            Self::Middle => "Middle-drag",
            Self::ShiftMiddle => "Shift+middle",
            Self::CtrlMiddle => "Ctrl+middle",
        }
    }

    /// Whether this gesture is active for the given drag state.
    #[must_use]
    pub const fn matches(self, state: GestureState) -> bool {
        match self {
            Self::Right => state.right && !state.ctrl,
            Self::CtrlRight => state.right && state.ctrl,
            Self::Middle => state.middle && !state.shift && !state.ctrl,
            Self::ShiftMiddle => state.middle && state.shift && !state.ctrl,
            Self::CtrlMiddle => state.middle && state.ctrl && !state.shift,
        }
    }
}

/// The buttons, modifiers, and hold keys currently active.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GestureState {
    pub primary: bool,
    pub right: bool,
    pub middle: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub f2: bool,
    pub f3: bool,
    pub f4: bool,
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
            if let Some(zoom) = bindings.zoom_drag {
                assert_ne!(zoom, bindings.orbit);
                assert_ne!(zoom, bindings.pan);
            }
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
    fn fusion_profile_orbits_on_shift_middle_and_inverts_the_wheel() {
        let bindings = NavigationPreset::Fusion.bindings();
        assert_eq!(bindings.orbit, Gesture::ShiftMiddle);
        assert_eq!(bindings.pan, Gesture::Middle);
        assert!(bindings.invert_zoom);
        let shifted = GestureState {
            middle: true,
            shift: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(shifted), Some(NavigationAction::Orbit));
        let plain = GestureState {
            middle: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(plain), Some(NavigationAction::Pan));
    }

    #[test]
    fn solidworks_profile_zoom_drags_on_shift_middle() {
        let bindings = NavigationPreset::SolidWorks.bindings();
        let shifted = GestureState {
            middle: true,
            shift: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(shifted), Some(NavigationAction::ZoomDrag));
        let ctrl = GestureState {
            middle: true,
            ctrl: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(ctrl), Some(NavigationAction::Pan));
        let plain = GestureState {
            middle: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(plain), Some(NavigationAction::Orbit));
    }

    #[test]
    fn inventor_hold_keys_navigate_a_left_drag() {
        let bindings = NavigationPreset::Inventor.bindings();
        let orbit = GestureState {
            primary: true,
            f4: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(orbit), Some(NavigationAction::Orbit));
        let pan = GestureState {
            primary: true,
            f2: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(pan), Some(NavigationAction::Pan));
        let zoom = GestureState {
            primary: true,
            f3: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(zoom), Some(NavigationAction::ZoomDrag));
        // The keys are inert without a drag, and a plain left drag stays a
        // selection gesture.
        let idle = GestureState {
            f4: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(idle), None);
        let plain = GestureState {
            primary: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(plain), None);
    }

    #[test]
    fn onshape_pans_on_middle_or_ctrl_right_and_orbits_on_plain_right() {
        let bindings = NavigationPreset::Onshape.bindings();
        let ctrl_right = GestureState {
            right: true,
            ctrl: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(ctrl_right), Some(NavigationAction::Pan));
        let right = GestureState {
            right: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(right), Some(NavigationAction::Orbit));
        let middle = GestureState {
            middle: true,
            ..GestureState::default()
        };
        assert_eq!(bindings.action(middle), Some(NavigationAction::Pan));
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
            Gesture::CtrlRight,
            Gesture::Middle,
            Gesture::ShiftMiddle,
            Gesture::CtrlMiddle,
        ]
        .into_iter()
        .filter(|gesture| gesture.matches(shifted))
        .count();
        assert_eq!(matching, 1);
    }

    #[test]
    fn legacy_preset_names_load_onto_the_matching_package_profile() {
        for (stored, expected) in [
            ("\"artificer\"", NavigationPreset::Artificer),
            ("\"right-orbit\"", NavigationPreset::Artificer),
            ("\"middle-pan\"", NavigationPreset::Fusion),
            ("\"middle-pan-inverted\"", NavigationPreset::Fusion),
            ("\"middle-orbit-inverted\"", NavigationPreset::SolidWorks),
            ("\"middle-orbit-shift-pan\"", NavigationPreset::Nx),
            (
                "\"middle-orbit-shift-pan-inverted\"",
                NavigationPreset::Creo,
            ),
        ] {
            let loaded: NavigationPreset = serde_json::from_str(stored).unwrap();
            assert_eq!(loaded, expected, "{stored}");
        }
    }

    #[test]
    fn every_profile_round_trips_through_serde() {
        for preset in NavigationPreset::ALL {
            let stored = serde_json::to_string(&preset).unwrap();
            let loaded: NavigationPreset = serde_json::from_str(&stored).unwrap();
            assert_eq!(loaded, preset);
        }
    }
}
