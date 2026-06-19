//! Generic "side decision" glue for the decision engines and CLI.
//!
//! A turn's decision for one side is a single [`MoveChoice`] in singles, but a
//! combined [`SideAction`](crate::engine::state::SideAction) (one sub-action per
//! active slot) in doubles. The search engines (`mcts`, `mcts_threaded`,
//! `search`) and the CLI (`io`) are written once against the [`SideChoice`] type
//! alias and the thin dispatch functions below, so the exact same algorithm code
//! drives both formats.
//!
//! In a singles build every alias/function here collapses to its singles
//! counterpart (zero-cost), so singles behaviour is bit-for-bit unchanged. The
//! doubles arms only ever compile together with `genx` (gen4-9), which is the
//! only place doubles support exists.

use crate::engine::state::MoveChoice;
use crate::instruction::StateInstructions;
use crate::state::{Side, SideReference, State};

/// The unit a side picks for a turn: one move in singles, a per-slot combined
/// action in doubles.
#[cfg(not(feature = "doubles"))]
pub type SideChoice = MoveChoice;
#[cfg(feature = "doubles")]
pub type SideChoice = crate::engine::state::SideAction;

/// Legal combined options for each side at the current (non-root) decision point.
#[cfg(not(feature = "doubles"))]
pub fn get_all_options(state: &State) -> (Vec<SideChoice>, Vec<SideChoice>) {
    state.get_all_options()
}
#[cfg(feature = "doubles")]
pub fn get_all_options(state: &State) -> (Vec<SideChoice>, Vec<SideChoice>) {
    state.get_all_options_doubles()
}

/// Legal combined options at the root (handles team preview / replacements).
#[cfg(not(feature = "doubles"))]
pub fn root_get_all_options(state: &State) -> (Vec<SideChoice>, Vec<SideChoice>) {
    state.root_get_all_options()
}
#[cfg(feature = "doubles")]
pub fn root_get_all_options(state: &State) -> (Vec<SideChoice>, Vec<SideChoice>) {
    state.root_get_all_options_doubles()
}

/// Resolve a pair of side decisions into the branched instruction set.
#[cfg(not(feature = "doubles"))]
pub fn generate_instructions(
    state: &mut State,
    s1: &SideChoice,
    s2: &SideChoice,
    branch_on_damage: bool,
) -> Vec<StateInstructions> {
    crate::engine::generate_instructions::generate_instructions_from_move_pair(
        state,
        s1,
        s2,
        branch_on_damage,
    )
}
#[cfg(feature = "doubles")]
pub fn generate_instructions(
    state: &mut State,
    s1: &SideChoice,
    s2: &SideChoice,
    branch_on_damage: bool,
) -> Vec<StateInstructions> {
    crate::engine::generate_instructions::generate_instructions_from_actions(
        state,
        s1,
        s2,
        branch_on_damage,
    )
}

/// Whether a side's decision is "do nothing this turn" (every slot is `None`).
/// Used by MCTS to detect the both-sides-pass terminal where there is nothing to
/// expand.
#[cfg(not(feature = "doubles"))]
pub fn is_none_action(choice: &SideChoice) -> bool {
    *choice == MoveChoice::None
}
#[cfg(feature = "doubles")]
pub fn is_none_action(choice: &SideChoice) -> bool {
    choice.actions.iter().all(|a| *a == MoveChoice::None)
}

/// Human-readable rendering of a side decision. In doubles the per-slot
/// sub-actions are joined with `;` (so a combined action stays a single token,
/// safe to embed in the `,`/`|`-delimited MCTS output lines).
#[cfg(not(feature = "doubles"))]
pub fn choice_to_string(choice: &SideChoice, side: &Side) -> String {
    choice.to_string(side)
}
#[cfg(feature = "doubles")]
pub fn choice_to_string(choice: &SideChoice, side: &Side) -> String {
    choice
        .actions
        .iter()
        .map(|a| a.to_string(side))
        .collect::<Vec<_>>()
        .join(";")
}

/// Parse a side decision from the CLI. In doubles the string is the `;`-separated
/// per-slot sub-actions (each accepting an optional `,<slot>` target suffix), e.g.
/// `"closecombat,1;protect"`. A missing/`none` sub-action is `MoveChoice::None`.
#[cfg(not(feature = "doubles"))]
pub fn parse_side_choice(s: &str, side: &Side, side_ref: SideReference) -> Option<SideChoice> {
    MoveChoice::from_string(s, side, side_ref)
}
#[cfg(feature = "doubles")]
pub fn parse_side_choice(s: &str, side: &Side, side_ref: SideReference) -> Option<SideChoice> {
    crate::engine::state::SideAction::from_string(s, side, side_ref)
}
