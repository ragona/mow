//! Explicit top-level game state transitions.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameState {
    #[default]
    Boot,
    Title,
    WorldEditor,
    Loading,
    Playing,
    Paused,
    Results,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowAction {
    BootComplete,
    OpenWorldEditor,
    StartLoading,
    LoadComplete,
    Pause,
    Resume,
    Finish,
    ReturnToWorldEditor,
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
            BootComplete, Finish, LoadComplete, OpenWorldEditor, Pause, Resume, ReturnToTitle,
            ReturnToWorldEditor, StartLoading,
        };
        use GameState::{Boot, Loading, Paused, Playing, Results, Title, WorldEditor};
        let next = match (self.state, action) {
            (Boot, BootComplete) | (WorldEditor | Results, ReturnToTitle) => Title,
            (Title | Results, OpenWorldEditor)
            | (Paused | Playing | Results, ReturnToWorldEditor) => WorldEditor,
            (WorldEditor | Results, StartLoading) => Loading,
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
            FlowAction::OpenWorldEditor,
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
