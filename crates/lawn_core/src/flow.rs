//! Explicit top-level game state transitions.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameState {
    #[default]
    Boot,
    Title,
    ModeSelect,
    Loading,
    Playing,
    Paused,
    Results,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowAction {
    BootComplete,
    OpenModeSelect,
    StartLoading,
    LoadComplete,
    Pause,
    Resume,
    Finish,
    ReturnToModeSelect,
    ReturnToTitle,
}

#[derive(Clone, Debug, Default)]
pub struct GameFlow {
    state: GameState,
}

impl GameFlow {
    #[must_use]
    pub const fn state(&self) -> GameState {
        self.state
    }

    /// Applies a legal top-level state transition.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidTransition`] when `action` is not legal from the
    /// current state; the current state is left unchanged.
    pub fn transition(&mut self, action: FlowAction) -> Result<GameState, InvalidTransition> {
        use FlowAction::{
            BootComplete, Finish, LoadComplete, OpenModeSelect, Pause, Resume, ReturnToModeSelect,
            ReturnToTitle, StartLoading,
        };
        use GameState::{Boot, Loading, ModeSelect, Paused, Playing, Results, Title};
        let next = match (self.state, action) {
            (Boot, BootComplete) | (ModeSelect | Results, ReturnToTitle) => Title,
            (Title | Results, OpenModeSelect)
            | (Paused | Playing | Results, ReturnToModeSelect) => ModeSelect,
            (ModeSelect | Results, StartLoading) => Loading,
            (Loading, LoadComplete) | (Paused, Resume) => Playing,
            (Playing, Pause) => Paused,
            (Playing, Finish) => Results,
            _ => {
                return Err(InvalidTransition {
                    from: self.state,
                    action,
                });
            }
        };
        self.state = next;
        Ok(next)
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("invalid game-flow transition from {from:?} via {action:?}")]
pub struct InvalidTransition {
    pub from: GameState,
    pub action: FlowAction,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_happy_path_and_pause_are_represented() {
        let mut flow = GameFlow::default();
        for action in [
            FlowAction::BootComplete,
            FlowAction::OpenModeSelect,
            FlowAction::StartLoading,
            FlowAction::LoadComplete,
            FlowAction::Pause,
            FlowAction::Resume,
            FlowAction::Finish,
        ] {
            flow.transition(action).unwrap();
        }
        assert_eq!(flow.state(), GameState::Results);
    }

    #[test]
    fn impossible_edges_are_rejected() {
        assert!(GameFlow::default().transition(FlowAction::Finish).is_err());
    }
}
