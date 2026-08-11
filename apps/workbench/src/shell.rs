//! Expandable native CAD-workbench shell state.
//!
//! Geometry and transaction state deliberately stay outside this module. The
//! shell only owns presentation choices that may change immediately without
//! entering the model-operation confirmation gate.

/// Public, copyable visibility snapshot used by semantic UI tests and future
/// persisted workspace preferences.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkbenchShellVisibility {
    pub command_ribbon: bool,
    pub model_browser: bool,
    pub feature_timeline: bool,
}

impl Default for WorkbenchShellVisibility {
    fn default() -> Self {
        Self {
            command_ribbon: true,
            model_browser: true,
            feature_timeline: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct WorkbenchShellState {
    visibility: WorkbenchShellVisibility,
}

impl WorkbenchShellState {
    pub(crate) const fn visibility(self) -> WorkbenchShellVisibility {
        self.visibility
    }

    #[cfg(test)]
    pub(crate) fn model_browser_mut(&mut self) -> &mut bool {
        &mut self.visibility.model_browser
    }

    pub(crate) fn feature_timeline_mut(&mut self) -> &mut bool {
        &mut self.visibility.feature_timeline
    }

    pub(crate) fn set_command_ribbon(&mut self, expanded: bool) {
        self.visibility.command_ribbon = expanded;
    }

    pub(crate) fn set_model_browser(&mut self, expanded: bool) {
        self.visibility.model_browser = expanded;
    }

    pub(crate) fn set_feature_timeline(&mut self, expanded: bool) {
        self.visibility.feature_timeline = expanded;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_primary_cad_region_starts_expanded_and_toggles_independently() {
        let mut shell = WorkbenchShellState::default();
        assert_eq!(shell.visibility(), WorkbenchShellVisibility::default());

        shell.set_command_ribbon(false);
        shell.set_model_browser(false);
        shell.set_feature_timeline(false);
        assert_eq!(
            shell.visibility(),
            WorkbenchShellVisibility {
                command_ribbon: false,
                model_browser: false,
                feature_timeline: false,
            }
        );

        shell.set_command_ribbon(true);
        *shell.model_browser_mut() = true;
        *shell.feature_timeline_mut() = true;
        assert_eq!(shell.visibility(), WorkbenchShellVisibility::default());
    }
}
