//! Doubles-only option enumeration tests (Phase 4).
//!
//! These only build under the `doubles` feature, and only for genx (gen4-9),
//! which is where doubles support lives. They exercise `get_all_options_doubles`
//! / `root_get_all_options_doubles`: the per-side cartesian product of each active
//! slot's legal sub-actions (moves fanned out over their legal targets + switches),
//! with cross-slot legality applied. Turn resolution is NOT covered here.
#![cfg(all(
    feature = "doubles",
    not(any(feature = "gen1", feature = "gen2", feature = "gen3"))
))]

use poke_engine::choices::{Choices, MOVES};
use poke_engine::engine::abilities::Abilities;
use poke_engine::engine::generate_instructions::{
    generate_instructions_from_actions, generate_instructions_from_move_pair,
};
use poke_engine::engine::state::{MoveChoice, PokemonVolatileStatus, SideAction};
use poke_engine::decision;
use poke_engine::instruction::{
    Instruction, StateInstructions, SwapActiveSlotsInstruction, ToggleForceSwitchSlotInstruction,
};
use poke_engine::mcts::perform_mcts;
use poke_engine::state::{
    BattlePosition, Move, PokemonBoostableStat, PokemonIndex, PokemonMoveIndex, PokemonType,
    SideReference, State,
};

/// Tera fans every move option into a Move + MoveTera pair. It is only available
/// under the `terastallization` feature (and only while no side Pokemon has
/// already terastallized, which is the case for a fresh `State::default`).
#[cfg(feature = "terastallization")]
const TERA_FACTOR: usize = 2;
#[cfg(not(feature = "terastallization"))]
const TERA_FACTOR: usize = 1;

/// Replace move `m` on `state.side_one.pokemon[idx]` with `choice`, and silence
/// the other three move slots so option counts are fully controlled by the test.
fn set_single_move(state: &mut State, idx: PokemonIndex, choice: Choices) {
    let pkmn = &mut state.side_one.pokemon[idx];
    pkmn.moves[&PokemonMoveIndex::M0] = Move {
        id: choice,
        disabled: false,
        pp: 32,
        choice: MOVES.get(&choice).unwrap().clone(),
    };
    pkmn.moves[&PokemonMoveIndex::M1].pp = 0;
    pkmn.moves[&PokemonMoveIndex::M2].pp = 0;
    pkmn.moves[&PokemonMoveIndex::M3].pp = 0;
}

/// Two active slots per side, each with a single single-target move (Tackle) and
/// two living benched Pokemon to switch to. Slot moves fan out over the two living
/// foes; the cartesian product drops the two double-switch-to-same-mon combos.
#[test]
fn test_doubles_enumerates_single_target_moves_and_switches() {
    let mut state = State::default();

    // Distinct leads in slot 0 and slot 1 (default points both slots at P0).
    state.side_one.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    state.side_two.active_indices = [PokemonIndex::P0, PokemonIndex::P1];

    // Side one benches: P2/P3 alive (legal switches), P4/P5 fainted.
    state.side_one.pokemon[PokemonIndex::P4].hp = 0;
    state.side_one.pokemon[PokemonIndex::P5].hp = 0;

    // Both active slots get exactly one single-target move.
    set_single_move(&mut state, PokemonIndex::P0, Choices::TACKLE);
    set_single_move(&mut state, PokemonIndex::P1, Choices::TACKLE);

    let (s1_options, _s2_options) = state.get_all_options_doubles();

    // Per slot: 1 move * 2 living foes * TERA_FACTOR move-variants, plus 2 switches.
    let per_slot = 2 * TERA_FACTOR + 2;
    // Cartesian product minus the double-switch-to-same-mon combos. The shared
    // switch targets are exactly {P2, P3}, so 2 combos are illegal.
    let expected = per_slot * per_slot - 2;
    assert_eq!(expected, s1_options.len());

    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);
    let foe1 = BattlePosition::new(SideReference::SideTwo, 1);

    // Both slots attacking, each free to pick either foe -> all 4 target combos present.
    let tackle = |target| MoveChoice::Move {
        move_index: PokemonMoveIndex::M0,
        target,
    };
    for t0 in [foe0, foe1] {
        for t1 in [foe0, foe1] {
            assert!(
                s1_options.contains(&SideAction::new([tackle(t0), tackle(t1)])),
                "missing combined action: slot0->{:?}, slot1->{:?}",
                t0,
                t1
            );
        }
    }

    // A simultaneous double switch to two *different* benched mons is legal...
    assert!(s1_options.contains(&SideAction::new([
        MoveChoice::Switch(PokemonIndex::P2),
        MoveChoice::Switch(PokemonIndex::P3),
    ])));
    // ...but both slots switching to the SAME benched mon is not.
    assert!(!s1_options.contains(&SideAction::new([
        MoveChoice::Switch(PokemonIndex::P2),
        MoveChoice::Switch(PokemonIndex::P2),
    ])));
}

/// An ally-target move (Helping Hand) targets the partner slot; with a fainted
/// partner the move has no legal target and is dropped from that slot's options.
#[test]
fn test_doubles_ally_target_requires_living_partner() {
    let mut state = State::default();
    state.side_one.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    state.side_two.active_indices = [PokemonIndex::P0, PokemonIndex::P1];

    // Only the slot-0 mon and a single bench mon are alive on side one.
    state.side_one.pokemon[PokemonIndex::P2].hp = 0;
    state.side_one.pokemon[PokemonIndex::P4].hp = 0;
    state.side_one.pokemon[PokemonIndex::P5].hp = 0;

    // Slot 0 uses Helping Hand (ally-target); slot 1 (P1) is alive as the partner.
    set_single_move(&mut state, PokemonIndex::P0, Choices::HELPINGHAND);

    let helping_hand_at = |target| MoveChoice::Move {
        move_index: PokemonMoveIndex::M0,
        target,
    };
    let ally_pos = BattlePosition::new(SideReference::SideOne, 1);

    // With the partner alive, Helping Hand targets the ally slot and appears in a combo.
    let (s1_living_partner, _) = state.get_all_options_doubles();
    assert!(s1_living_partner
        .iter()
        .any(|sa| sa.actions[0] == helping_hand_at(ally_pos)));

    // Faint the partner AND the last bench mon (P3), so there is no replacement
    // to make: slot 1 just stays empty and slot 0 acts normally. (With a living
    // bench mon this would instead become a replacement decision point — covered
    // by the multi-faint tests below.) Helping Hand now has no legal target.
    state.side_one.pokemon[PokemonIndex::P1].hp = 0;
    state.side_one.pokemon[PokemonIndex::P3].hp = 0;
    let (s1_dead_partner, _) = state.get_all_options_doubles();
    assert!(s1_dead_partner
        .iter()
        .all(|sa| !matches!(sa.actions[0], MoveChoice::Move { move_index: PokemonMoveIndex::M0, .. })));
    // Slot 1 is now empty -> its only sub-action is None.
    assert!(s1_dead_partner
        .iter()
        .all(|sa| sa.actions[1] == MoveChoice::None));
}

/// `root_get_all_options_doubles` delegates to `get_all_options_doubles` when not
/// in team preview, so the two agree for an ordinary mid-battle position.
#[test]
fn test_doubles_root_delegates_when_not_team_preview() {
    let mut state = State::default();
    state.side_one.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    state.side_two.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    assert!(!state.team_preview);

    let root = state.root_get_all_options_doubles();
    let plain = state.get_all_options_doubles();
    assert_eq!(plain.0, root.0);
    assert_eq!(plain.1, root.1);
}

// ===========================================================================
// Phase 5: turn-resolution (actor ordering / sequencing) tests.
// ===========================================================================

/// Give a single move (slot M0) to `side`'s `idx` Pokemon and silence the rest.
fn set_move_on(state: &mut State, side: SideReference, idx: PokemonIndex, choice: Choices) {
    let s = match side {
        SideReference::SideOne => &mut state.side_one,
        SideReference::SideTwo => &mut state.side_two,
    };
    let pkmn = &mut s.pokemon[idx];
    pkmn.moves[&PokemonMoveIndex::M0] = Move {
        id: choice,
        disabled: false,
        pp: 32,
        choice: MOVES.get(&choice).unwrap().clone(),
    };
    pkmn.moves[&PokemonMoveIndex::M1].pp = 0;
    pkmn.moves[&PokemonMoveIndex::M2].pp = 0;
    pkmn.moves[&PokemonMoveIndex::M3].pp = 0;
}

fn tackle_at(target: BattlePosition) -> MoveChoice {
    MoveChoice::Move {
        move_index: PokemonMoveIndex::M0,
        target,
    }
}

/// All four actives Tackle; with four distinct effective speeds the engine must
/// resolve them fastest-first. Each side's hits land on the opposing active, so
/// the order of `Damage` instructions (by defender side) reflects the actor order.
#[test]
fn test_doubles_four_actors_resolve_in_speed_order() {
    let mut state = State::default();
    state.side_one.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    state.side_two.active_indices = [PokemonIndex::P0, PokemonIndex::P1];

    // Bulky actives so nobody faints mid-turn (keeps all four Damage instrs).
    for side in [SideReference::SideOne, SideReference::SideTwo] {
        for idx in [PokemonIndex::P0, PokemonIndex::P1] {
            set_move_on(&mut state, side, idx, Choices::TACKLE);
            let s = match side {
                SideReference::SideOne => &mut state.side_one,
                SideReference::SideTwo => &mut state.side_two,
            };
            s.pokemon[idx].hp = 500;
            s.pokemon[idx].maxhp = 500;
        }
    }
    // Distinct speeds: s1/0 > s2/0 > s1/1 > s2/1.
    state.side_one.pokemon[PokemonIndex::P0].speed = 200;
    state.side_two.pokemon[PokemonIndex::P0].speed = 150;
    state.side_one.pokemon[PokemonIndex::P1].speed = 100;
    state.side_two.pokemon[PokemonIndex::P1].speed = 50;

    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);
    let ally_foe0 = BattlePosition::new(SideReference::SideOne, 0);
    let s1 = SideAction::new([tackle_at(foe0), tackle_at(foe0)]);
    let s2 = SideAction::new([tackle_at(ally_foe0), tackle_at(ally_foe0)]);

    let instructions = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert_eq!(1, instructions.len(), "no ties -> single branch");

    let damage_sides: Vec<SideReference> = instructions[0]
        .instruction_list
        .iter()
        .filter_map(|i| match i {
            Instruction::Damage(d) => Some(d.side_ref),
            _ => None,
        })
        .collect();
    // Fastest-first: s1/0, s2/0, s1/1, s2/1 -> defender sides Two, One, Two, One.
    assert_eq!(
        vec![
            SideReference::SideTwo,
            SideReference::SideOne,
            SideReference::SideTwo,
            SideReference::SideOne,
        ],
        damage_sides
    );
}

/// When two actors share priority AND effective speed, the resolver cannot order
/// them deterministically: it must spawn an equiprobable branch per permutation
/// of the tied group (the order of the other two actors is fixed). Here s1/0 and
/// s2/0 are tied at the top, s1/1 and s2/1 are tied at the bottom -> 2 x 2 = 4
/// equiprobable branches, each 25%.
#[test]
fn test_doubles_four_actor_speed_tie_branches() {
    let mut state = State::default();
    state.side_one.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    state.side_two.active_indices = [PokemonIndex::P0, PokemonIndex::P1];

    for side in [SideReference::SideOne, SideReference::SideTwo] {
        for idx in [PokemonIndex::P0, PokemonIndex::P1] {
            set_move_on(&mut state, side, idx, Choices::TACKLE);
            let s = match side {
                SideReference::SideOne => &mut state.side_one,
                SideReference::SideTwo => &mut state.side_two,
            };
            s.pokemon[idx].hp = 500;
            s.pokemon[idx].maxhp = 500;
        }
    }
    // Two tied speed groups: {s1/0, s2/0} at 200, {s1/1, s2/1} at 100.
    state.side_one.pokemon[PokemonIndex::P0].speed = 200;
    state.side_two.pokemon[PokemonIndex::P0].speed = 200;
    state.side_one.pokemon[PokemonIndex::P1].speed = 100;
    state.side_two.pokemon[PokemonIndex::P1].speed = 100;

    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);
    let ally_foe0 = BattlePosition::new(SideReference::SideOne, 0);
    let s1 = SideAction::new([tackle_at(foe0), tackle_at(foe0)]);
    let s2 = SideAction::new([tackle_at(ally_foe0), tackle_at(ally_foe0)]);

    let instructions = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert_eq!(
        4,
        instructions.len(),
        "two independent 2-way ties -> 4 equiprobable branches"
    );
    let total: f32 = instructions.iter().map(|i| i.percentage).sum();
    assert!((total - 100.0).abs() < 0.1, "branch percentages sum to 100");
    for branch in &instructions {
        assert!(
            (branch.percentage - 25.0).abs() < 0.1,
            "each tie permutation is equiprobable (25%)"
        );
    }
}

/// Trick Room reverses the speed order among same-priority actors.
#[test]
fn test_doubles_trick_room_reverses_order() {
    let mut state = State::default();
    state.trick_room.active = true;
    state.side_one.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    state.side_two.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    for side in [SideReference::SideOne, SideReference::SideTwo] {
        for idx in [PokemonIndex::P0, PokemonIndex::P1] {
            set_move_on(&mut state, side, idx, Choices::TACKLE);
            let s = match side {
                SideReference::SideOne => &mut state.side_one,
                SideReference::SideTwo => &mut state.side_two,
            };
            s.pokemon[idx].hp = 500;
            s.pokemon[idx].maxhp = 500;
        }
    }
    state.side_one.pokemon[PokemonIndex::P0].speed = 200;
    state.side_two.pokemon[PokemonIndex::P0].speed = 150;
    state.side_one.pokemon[PokemonIndex::P1].speed = 100;
    state.side_two.pokemon[PokemonIndex::P1].speed = 50;

    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);
    let ally_foe0 = BattlePosition::new(SideReference::SideOne, 0);
    let s1 = SideAction::new([tackle_at(foe0), tackle_at(foe0)]);
    let s2 = SideAction::new([tackle_at(ally_foe0), tackle_at(ally_foe0)]);

    let instructions = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert_eq!(1, instructions.len());
    let damage_sides: Vec<SideReference> = instructions[0]
        .instruction_list
        .iter()
        .filter_map(|i| match i {
            Instruction::Damage(d) => Some(d.side_ref),
            _ => None,
        })
        .collect();
    // Slowest-first under Trick Room: s2/1, s1/1, s2/0, s1/0 -> One, Two, One, Two.
    assert_eq!(
        vec![
            SideReference::SideOne,
            SideReference::SideTwo,
            SideReference::SideOne,
            SideReference::SideTwo,
        ],
        damage_sides
    );
}

/// A higher-priority move on a slow actor goes before faster neutral-priority moves.
#[test]
fn test_doubles_priority_bracket_beats_speed() {
    let mut state = State::default();
    state.side_one.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    state.side_two.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    for side in [SideReference::SideOne, SideReference::SideTwo] {
        for idx in [PokemonIndex::P0, PokemonIndex::P1] {
            set_move_on(&mut state, side, idx, Choices::TACKLE);
            let s = match side {
                SideReference::SideOne => &mut state.side_one,
                SideReference::SideTwo => &mut state.side_two,
            };
            s.pokemon[idx].hp = 500;
            s.pokemon[idx].maxhp = 500;
        }
    }
    // Distinct speeds among the Tackle users (no ties -> single ordering).
    state.side_one.pokemon[PokemonIndex::P0].speed = 200;
    state.side_two.pokemon[PokemonIndex::P0].speed = 150;
    state.side_one.pokemon[PokemonIndex::P1].speed = 100;
    // Slowest actor (s2/1) uses +1 priority Quick Attack; everyone else Tackle.
    set_move_on(&mut state, SideReference::SideTwo, PokemonIndex::P1, Choices::QUICKATTACK);
    state.side_two.pokemon[PokemonIndex::P1].speed = 1;

    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);
    let ally_foe0 = BattlePosition::new(SideReference::SideOne, 0);
    let s1 = SideAction::new([tackle_at(foe0), tackle_at(foe0)]);
    let s2 = SideAction::new([tackle_at(ally_foe0), tackle_at(ally_foe0)]);

    let instructions = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert_eq!(1, instructions.len());
    let first_damage_side = instructions[0]
        .instruction_list
        .iter()
        .find_map(|i| match i {
            Instruction::Damage(d) => Some(d.side_ref),
            _ => None,
        });
    // The +1 priority attacker is on side two, so its hit (on side one) lands first.
    assert_eq!(Some(SideReference::SideOne), first_damage_side);
}

/// A U-turn in doubles records a per-slot forced switch (ToggleForceSwitchSlot),
/// not the singles per-side ToggleSide{One,Two}ForceSwitch, and suppresses
/// end-of-turn effects via `any_force_switch`.
#[test]
fn test_doubles_uturn_sets_per_slot_force_switch() {
    let mut state = State::default();
    // U-turn user (side one slot 0) faster than the opponent.
    state.side_one.pokemon[PokemonIndex::P0].speed = 200;
    state.side_two.pokemon[PokemonIndex::P0].speed = 50;
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::UTURN);
    set_move_on(&mut state, SideReference::SideTwo, PokemonIndex::P0, Choices::TACKLE);

    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);
    let me0 = BattlePosition::new(SideReference::SideOne, 0);
    let instructions = generate_instructions_from_move_pair(
        &mut state,
        &tackle_at(foe0),
        &tackle_at(me0),
        false,
    );

    let has_per_slot_force_switch = instructions.iter().any(|si| {
        si.instruction_list.iter().any(|i| {
            *i == Instruction::ToggleForceSwitchSlot(ToggleForceSwitchSlotInstruction {
                side_ref: SideReference::SideOne,
                slot: 0,
            })
        })
    });
    assert!(
        has_per_slot_force_switch,
        "U-turn should set the per-slot force switch for side one slot 0"
    );
    // The singles per-side instruction must NOT appear in a doubles build.
    let has_per_side_force_switch = instructions.iter().any(|si| {
        si.instruction_list
            .iter()
            .any(|i| *i == Instruction::ToggleSideOneForceSwitch)
    });
    assert!(!has_per_side_force_switch);
}

// ===========================================================================
// Phase 6: per-position damage resolution & spread moves.
// ===========================================================================

/// All `(side, slot, amount)` triples of every `Damage` instruction produced.
fn damage_targets(instrs: &[StateInstructions]) -> Vec<(SideReference, u8, i16)> {
    instrs
        .iter()
        .flat_map(|si| {
            si.instruction_list.iter().filter_map(|i| match i {
                Instruction::Damage(d) => Some((d.side_ref, d.slot, d.damage_amount)),
                _ => None,
            })
        })
        .collect()
}

/// Total damage dealt to a specific position across all produced instructions.
fn damage_to(instrs: &[StateInstructions], side: SideReference, slot: u8) -> i16 {
    damage_targets(instrs)
        .into_iter()
        .filter(|(s, sl, _)| *s == side && *sl == slot)
        .map(|(_, _, amt)| amt)
        .sum()
}

/// Two living, bulky actives per side at distinct indices (slot1 != slot0).
fn two_v_two_bulky() -> State {
    let mut state = State::default();
    state.side_one.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    state.side_two.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    for side in [SideReference::SideOne, SideReference::SideTwo] {
        for idx in [PokemonIndex::P0, PokemonIndex::P1] {
            let s = match side {
                SideReference::SideOne => &mut state.side_one,
                SideReference::SideTwo => &mut state.side_two,
            };
            s.pokemon[idx].hp = 500;
            s.pokemon[idx].maxhp = 500;
        }
    }
    state
}

fn move_at(target: BattlePosition) -> MoveChoice {
    MoveChoice::Move {
        move_index: PokemonMoveIndex::M0,
        target,
    }
}

/// A single-target move lands on whichever foe the user selected (left or right),
/// recorded as the `slot` on the resulting `Damage` instruction.
#[test]
fn test_doubles_single_target_hits_chosen_foe_slot() {
    for chosen_slot in [0u8, 1u8] {
        let mut state = two_v_two_bulky();
        set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::TACKLE);

        let target = BattlePosition::new(SideReference::SideTwo, chosen_slot);
        let s1 = SideAction::new([move_at(target), MoveChoice::None]);
        let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);

        let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
        let dmg = damage_targets(&instrs);

        assert_eq!(dmg.len(), 1, "exactly one target is hit");
        assert_eq!(dmg[0].0, SideReference::SideTwo);
        assert_eq!(
            dmg[0].1, chosen_slot,
            "damage must land on the chosen foe slot {}",
            chosen_slot
        );
    }
}

/// Earthquake (AllAdjacent) hits both foes AND the ally, but never the user.
#[test]
fn test_doubles_earthquake_hits_ally_and_both_foes() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::EARTHQUAKE);

    // Nominal target is ignored for a spread move; the resolver expands to all
    // living adjacent targets.
    let nominal = BattlePosition::new(SideReference::SideTwo, 0);
    let s1 = SideAction::new([move_at(nominal), MoveChoice::None]);
    let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    let positions: Vec<(SideReference, u8)> =
        damage_targets(&instrs).into_iter().map(|(s, sl, _)| (s, sl)).collect();

    assert!(positions.contains(&(SideReference::SideTwo, 0)), "hits foe slot 0");
    assert!(positions.contains(&(SideReference::SideTwo, 1)), "hits foe slot 1");
    assert!(positions.contains(&(SideReference::SideOne, 1)), "hits the ally");
    assert!(
        !positions.contains(&(SideReference::SideOne, 0)),
        "must NOT hit the user (Earthquake never hits the user)"
    );
    assert_eq!(positions.len(), 3, "exactly three targets");
}

/// A spread move that hits more than one target deals 0.75x to each (gen4+).
/// Comparing the same foe's damage with one vs. three living targets shows the
/// reduction; with a single living target there is no reduction.
#[test]
fn test_doubles_earthquake_spread_reduction() {
    let foe0 = SideReference::SideTwo;

    // Single living target (faint the ally and the other foe) -> no reduction.
    let single_damage = {
        let mut state = two_v_two_bulky();
        set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::EARTHQUAKE);
        state.side_two.pokemon[PokemonIndex::P1].hp = 0; // other foe fainted
        state.side_one.pokemon[PokemonIndex::P1].hp = 0; // ally fainted
        let s1 = SideAction::new([
            move_at(BattlePosition::new(SideReference::SideTwo, 0)),
            MoveChoice::None,
        ]);
        let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);
        let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
        damage_to(&instrs, foe0, 0)
    };

    // Three living targets -> 0.75x spread reduction on each.
    let spread_damage = {
        let mut state = two_v_two_bulky();
        set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::EARTHQUAKE);
        let s1 = SideAction::new([
            move_at(BattlePosition::new(SideReference::SideTwo, 0)),
            MoveChoice::None,
        ]);
        let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);
        let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
        damage_to(&instrs, foe0, 0)
    };

    assert!(single_damage > 0, "sanity: the single-target hit deals damage");
    assert!(
        spread_damage < single_damage,
        "spread damage ({}) must be reduced vs single-target ({})",
        spread_damage,
        single_damage
    );
    let ratio = spread_damage as f32 / single_damage as f32;
    assert!(
        (0.72..=0.78).contains(&ratio),
        "spread reduction should be ~0.75x (got {:.3}: {} vs {})",
        ratio,
        spread_damage,
        single_damage
    );
}

/// The legacy 2-`MoveChoice` wrapper produces exactly what the actions-based core
/// produces for the equivalent single-slot-filled `SideAction`s.
#[test]
fn test_doubles_wrapper_matches_actions() {
    let build = || {
        let mut state = State::default();
        set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::TACKLE);
        set_move_on(&mut state, SideReference::SideTwo, PokemonIndex::P0, Choices::TACKLE);
        state
    };
    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);
    let me0 = BattlePosition::new(SideReference::SideOne, 0);

    let mut state_a = build();
    let via_wrapper =
        generate_instructions_from_move_pair(&mut state_a, &tackle_at(foe0), &tackle_at(me0), false);

    let mut state_b = build();
    let mut s1 = [MoveChoice::None; 2];
    let mut s2 = [MoveChoice::None; 2];
    s1[0] = tackle_at(foe0);
    s2[0] = tackle_at(me0);
    let via_actions = generate_instructions_from_actions(
        &mut state_b,
        &SideAction::new(s1),
        &SideAction::new(s2),
        false,
    );

    assert_eq!(via_wrapper, via_actions);
}

// ===========================================================================
// Phase 7: doubles-only mechanics (redirection, helping hand, ally abilities,
// area protection, ally targeting, multi-faint replacement).
// ===========================================================================

/// Every `Boost` instruction's `(side, slot, stat, amount)`.
fn boost_instrs(
    instrs: &[StateInstructions],
) -> Vec<(SideReference, u8, PokemonBoostableStat, i8)> {
    instrs
        .iter()
        .flat_map(|si| {
            si.instruction_list.iter().filter_map(|i| match i {
                Instruction::Boost(b) => Some((b.side_ref, b.slot, b.stat, b.amount)),
                _ => None,
            })
        })
        .collect()
}

/// Follow Me on a partner pulls a single-target foe move off the chosen target
/// and onto the Follow Me user.
#[test]
fn test_doubles_follow_me_redirects_single_target() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::TACKLE);
    // Side two slot 1 is drawing moves with Follow Me.
    state.side_two.pokemon[PokemonIndex::P1]
        .volatile_statuses
        .insert(PokemonVolatileStatus::FOLLOWME);

    // Aim at foe slot 0; redirection should move the hit to foe slot 1.
    let nominal = BattlePosition::new(SideReference::SideTwo, 0);
    let s1 = SideAction::new([move_at(nominal), MoveChoice::None]);
    let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert_eq!(damage_to(&instrs, SideReference::SideTwo, 0), 0, "no hit on the chosen foe");
    assert!(damage_to(&instrs, SideReference::SideTwo, 1) > 0, "redirected onto Follow Me user");
}

/// Spotlight marks a Pokemon as the move-magnet the same way Follow Me does.
#[test]
fn test_doubles_spotlight_redirects_single_target() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::TACKLE);
    state.side_two.pokemon[PokemonIndex::P1]
        .volatile_statuses
        .insert(PokemonVolatileStatus::SPOTLIGHT);

    let nominal = BattlePosition::new(SideReference::SideTwo, 0);
    let s1 = SideAction::new([move_at(nominal), MoveChoice::None]);
    let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert_eq!(damage_to(&instrs, SideReference::SideTwo, 0), 0);
    assert!(damage_to(&instrs, SideReference::SideTwo, 1) > 0);
}

/// Lightning Rod on a partner redirects an Electric move onto itself, takes no
/// damage (the move is nullified) and gains a +1 Special Attack boost.
#[test]
fn test_doubles_lightning_rod_redirects_nullifies_and_boosts() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::THUNDERBOLT);
    state.side_two.pokemon[PokemonIndex::P1].ability = Abilities::LIGHTNINGROD;

    let nominal = BattlePosition::new(SideReference::SideTwo, 0);
    let s1 = SideAction::new([move_at(nominal), MoveChoice::None]);
    let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);

    // No damage anywhere on side two (the rod holder absorbs the Electric move).
    assert_eq!(damage_to(&instrs, SideReference::SideTwo, 0), 0);
    assert_eq!(damage_to(&instrs, SideReference::SideTwo, 1), 0);
    // Lightning Rod holder (slot 1) gains +1 Special Attack.
    assert!(
        boost_instrs(&instrs).contains(&(
            SideReference::SideTwo,
            1,
            PokemonBoostableStat::SpecialAttack,
            1
        )),
        "Lightning Rod holder should gain +1 SpA, got {:?}",
        boost_instrs(&instrs)
    );
}

/// Rage Powder does not draw in a Grass-type attacker (it ignores powder moves).
#[test]
fn test_doubles_rage_powder_ignored_by_grass_attacker() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::TACKLE);
    state.side_one.pokemon[PokemonIndex::P0].types = (PokemonType::GRASS, PokemonType::TYPELESS);
    state.side_two.pokemon[PokemonIndex::P1]
        .volatile_statuses
        .insert(PokemonVolatileStatus::RAGEPOWDER);

    let nominal = BattlePosition::new(SideReference::SideTwo, 0);
    let s1 = SideAction::new([move_at(nominal), MoveChoice::None]);
    let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    // The Grass attacker ignores Rage Powder, so the chosen foe is still hit.
    assert!(damage_to(&instrs, SideReference::SideTwo, 0) > 0, "Grass attacker not redirected");
    assert_eq!(damage_to(&instrs, SideReference::SideTwo, 1), 0);
}

/// The single-turn move-drawing volatiles are cleared at end of turn so they
/// don't keep redirecting on subsequent turns.
#[test]
fn test_doubles_follow_me_cleared_end_of_turn() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::TACKLE);
    state.side_two.pokemon[PokemonIndex::P1]
        .volatile_statuses
        .insert(PokemonVolatileStatus::FOLLOWME);

    let nominal = BattlePosition::new(SideReference::SideTwo, 0);
    let s1 = SideAction::new([move_at(nominal), MoveChoice::None]);
    let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    let removes_follow_me = instrs.iter().any(|si| {
        si.instruction_list.iter().any(|i| match i {
            Instruction::RemoveVolatileStatus(r) => {
                r.volatile_status == PokemonVolatileStatus::FOLLOWME
            }
            _ => false,
        })
    });
    assert!(removes_follow_me, "Follow Me should be removed at end of turn");
}

/// Helping Hand from the partner boosts this attacker's damage by 1.5x for the
/// turn (the helper moves first thanks to +5 priority).
#[test]
fn test_doubles_helping_hand_boosts_ally_damage() {
    let foe = SideReference::SideTwo;
    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);
    let ally1 = BattlePosition::new(SideReference::SideOne, 1);

    // Baseline: slot 1 Tackles the foe with no help.
    let baseline = {
        let mut state = two_v_two_bulky();
        set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P1, Choices::TACKLE);
        let s1 = SideAction::new([MoveChoice::None, move_at(foe0)]);
        let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);
        let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
        damage_to(&instrs, foe, 0)
    };

    // With help: slot 0 uses Helping Hand on the ally (slot 1) before it Tackles.
    let helped = {
        let mut state = two_v_two_bulky();
        set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::HELPINGHAND);
        set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P1, Choices::TACKLE);
        let s1 = SideAction::new([
            MoveChoice::Move { move_index: PokemonMoveIndex::M0, target: ally1 },
            move_at(foe0),
        ]);
        let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);
        let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
        damage_to(&instrs, foe, 0)
    };

    assert!(baseline > 0, "sanity: unboosted Tackle deals damage");
    let ratio = helped as f32 / baseline as f32;
    assert!(
        (1.45..=1.55).contains(&ratio),
        "Helping Hand should boost damage ~1.5x (got {:.3}: {} vs {})",
        ratio,
        helped,
        baseline
    );
}

/// Wide Guard (put up first thanks to +3 priority) blocks a spread move aimed at
/// that side this turn.
#[test]
fn test_doubles_wide_guard_blocks_spread() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::ROCKSLIDE);
    set_move_on(&mut state, SideReference::SideTwo, PokemonIndex::P0, Choices::WIDEGUARD);

    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);
    let me0 = BattlePosition::new(SideReference::SideOne, 0);
    let s1 = SideAction::new([move_at(foe0), MoveChoice::None]);
    // Side two slot 0 puts up Wide Guard (target self); slot 1 does nothing.
    let s2 = SideAction::new([
        MoveChoice::Move { move_index: PokemonMoveIndex::M0, target: me0 },
        MoveChoice::None,
    ]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert_eq!(damage_to(&instrs, SideReference::SideTwo, 0), 0, "Wide Guard blocks spread on slot 0");
    assert_eq!(damage_to(&instrs, SideReference::SideTwo, 1), 0, "Wide Guard blocks spread on slot 1");
}

/// Quick Guard blocks an increased-priority move aimed at that side.
#[test]
fn test_doubles_quick_guard_blocks_priority() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::QUICKATTACK);
    set_move_on(&mut state, SideReference::SideTwo, PokemonIndex::P0, Choices::QUICKGUARD);

    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);
    let me0 = BattlePosition::new(SideReference::SideOne, 0);
    let s1 = SideAction::new([move_at(foe0), MoveChoice::None]);
    let s2 = SideAction::new([
        MoveChoice::Move { move_index: PokemonMoveIndex::M0, target: me0 },
        MoveChoice::None,
    ]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert_eq!(damage_to(&instrs, SideReference::SideTwo, 0), 0, "Quick Guard blocks the priority hit");
}

/// Quick Guard does NOT block a regular (neutral-priority) move.
#[test]
fn test_doubles_quick_guard_allows_normal_priority() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::TACKLE);
    set_move_on(&mut state, SideReference::SideTwo, PokemonIndex::P0, Choices::QUICKGUARD);

    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);
    let me0 = BattlePosition::new(SideReference::SideOne, 0);
    let s1 = SideAction::new([move_at(foe0), MoveChoice::None]);
    let s2 = SideAction::new([
        MoveChoice::Move { move_index: PokemonMoveIndex::M0, target: me0 },
        MoveChoice::None,
    ]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert!(damage_to(&instrs, SideReference::SideTwo, 0) > 0, "neutral-priority Tackle is not blocked");
}

/// Friend Guard on the defender's partner reduces incoming damage to ~0.75x.
#[test]
fn test_doubles_friend_guard_reduces_ally_damage() {
    let foe = SideReference::SideTwo;
    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);

    let damage_with_ally_ability = |ability: Abilities| {
        let mut state = two_v_two_bulky();
        set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::TACKLE);
        state.side_two.pokemon[PokemonIndex::P1].ability = ability; // the defender's partner
        let s1 = SideAction::new([move_at(foe0), MoveChoice::None]);
        let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);
        let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
        damage_to(&instrs, foe, 0)
    };

    let baseline = damage_with_ally_ability(Abilities::NONE);
    let guarded = damage_with_ally_ability(Abilities::FRIENDGUARD);
    assert!(baseline > 0);
    let ratio = guarded as f32 / baseline as f32;
    assert!(
        (0.72..=0.78).contains(&ratio),
        "Friend Guard should reduce damage to ~0.75x (got {:.3})",
        ratio
    );
}

/// Battery on the attacker's partner boosts the user's special moves ~1.3x.
#[test]
fn test_doubles_battery_boosts_ally_special() {
    let foe = SideReference::SideTwo;
    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);

    let damage_with_ally_ability = |ability: Abilities| {
        let mut state = two_v_two_bulky();
        set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::THUNDERBOLT);
        state.side_one.pokemon[PokemonIndex::P1].ability = ability; // the attacker's partner
        let s1 = SideAction::new([move_at(foe0), MoveChoice::None]);
        let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);
        let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
        damage_to(&instrs, foe, 0)
    };

    let baseline = damage_with_ally_ability(Abilities::NONE);
    let boosted = damage_with_ally_ability(Abilities::BATTERY);
    assert!(baseline > 0);
    let ratio = boosted as f32 / baseline as f32;
    assert!(
        (1.27..=1.33).contains(&ratio),
        "Battery should boost the ally's special move ~1.3x (got {:.3})",
        ratio
    );
}

/// Power Spot on the attacker's partner boosts any of the user's moves ~1.3x.
#[test]
fn test_doubles_power_spot_boosts_ally_move() {
    let foe = SideReference::SideTwo;
    let foe0 = BattlePosition::new(SideReference::SideTwo, 0);

    let damage_with_ally_ability = |ability: Abilities| {
        let mut state = two_v_two_bulky();
        set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::TACKLE);
        state.side_one.pokemon[PokemonIndex::P1].ability = ability;
        let s1 = SideAction::new([move_at(foe0), MoveChoice::None]);
        let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);
        let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
        damage_to(&instrs, foe, 0)
    };

    let baseline = damage_with_ally_ability(Abilities::NONE);
    let boosted = damage_with_ally_ability(Abilities::POWERSPOT);
    assert!(baseline > 0);
    let ratio = boosted as f32 / baseline as f32;
    assert!(
        (1.27..=1.33).contains(&ratio),
        "Power Spot should boost the ally's move ~1.3x (got {:.3})",
        ratio
    );
}

/// Telepathy makes a Pokemon take no damage from its own ally's spread move.
#[test]
fn test_doubles_telepathy_avoids_ally_spread() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::EARTHQUAKE);
    // The user's partner has Telepathy and should not be hit by Earthquake.
    state.side_one.pokemon[PokemonIndex::P1].ability = Abilities::TELEPATHY;

    let nominal = BattlePosition::new(SideReference::SideTwo, 0);
    let s1 = SideAction::new([move_at(nominal), MoveChoice::None]);
    let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert_eq!(
        damage_to(&instrs, SideReference::SideOne, 1),
        0,
        "Telepathy partner takes no damage from the ally's Earthquake"
    );
    // The foes are still hit normally.
    assert!(damage_to(&instrs, SideReference::SideTwo, 0) > 0);
    assert!(damage_to(&instrs, SideReference::SideTwo, 1) > 0);
}

/// An ally-target boost move (Decorate) applies its boosts to the partner slot.
#[test]
fn test_doubles_ally_target_boost_lands_on_partner() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::DECORATE);

    let ally1 = BattlePosition::new(SideReference::SideOne, 1);
    let s1 = SideAction::new([
        MoveChoice::Move { move_index: PokemonMoveIndex::M0, target: ally1 },
        MoveChoice::None,
    ]);
    let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    let boosts = boost_instrs(&instrs);
    assert!(
        boosts.contains(&(SideReference::SideOne, 1, PokemonBoostableStat::Attack, 2)),
        "ally should get +2 Attack, got {:?}",
        boosts
    );
    assert!(
        boosts.contains(&(SideReference::SideOne, 1, PokemonBoostableStat::SpecialAttack, 2)),
        "ally should get +2 Special Attack, got {:?}",
        boosts
    );
    // Nothing should land on the user's own slot 0.
    assert!(boosts.iter().all(|(s, sl, _, _)| !(*s == SideReference::SideOne && *sl == 0)));
}

/// A foes-only spread move (Rock Slide / AllAdjacentFoes) hits both foes but
/// never the ally.
#[test]
fn test_doubles_foe_spread_does_not_hit_ally() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::ROCKSLIDE);

    let nominal = BattlePosition::new(SideReference::SideTwo, 0);
    let s1 = SideAction::new([move_at(nominal), MoveChoice::None]);
    let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert!(damage_to(&instrs, SideReference::SideTwo, 0) > 0, "hits foe slot 0");
    assert!(damage_to(&instrs, SideReference::SideTwo, 1) > 0, "hits foe slot 1");
    assert_eq!(damage_to(&instrs, SideReference::SideOne, 1), 0, "must NOT hit the ally");
    assert_eq!(damage_to(&instrs, SideReference::SideOne, 0), 0, "must NOT hit the user");
}

/// When both of a side's actives faint, the replacement decision point makes
/// each empty slot send a distinct living bench mon while the other side waits.
#[test]
fn test_doubles_double_faint_enumerates_distinct_replacements() {
    let mut state = two_v_two_bulky();
    // Both side-one actives are fainted; exactly two living bench mons (P2, P3).
    state.side_one.pokemon[PokemonIndex::P0].hp = 0;
    state.side_one.pokemon[PokemonIndex::P1].hp = 0;
    state.side_one.pokemon[PokemonIndex::P4].hp = 0;
    state.side_one.pokemon[PokemonIndex::P5].hp = 0;

    let (s1, s2) = state.get_all_options_doubles();

    // Two empty slots, two bench mons -> distinct assignments only: (P2,P3),(P3,P2).
    assert_eq!(s1.len(), 2, "two distinct replacement assignments");
    assert!(s1.contains(&SideAction::new([
        MoveChoice::Switch(PokemonIndex::P2),
        MoveChoice::Switch(PokemonIndex::P3),
    ])));
    assert!(s1.contains(&SideAction::new([
        MoveChoice::Switch(PokemonIndex::P3),
        MoveChoice::Switch(PokemonIndex::P2),
    ])));
    // No combo sends the same mon into both slots.
    assert!(!s1.contains(&SideAction::new([
        MoveChoice::Switch(PokemonIndex::P2),
        MoveChoice::Switch(PokemonIndex::P2),
    ])));

    // The healthy side waits (does nothing) during the replacement.
    assert_eq!(s2.len(), 1);
    assert_eq!(s2[0], SideAction::new([MoveChoice::None, MoveChoice::None]));
}

/// A single faint replaces only the empty slot; the surviving slot does nothing.
#[test]
fn test_doubles_single_faint_replaces_only_empty_slot() {
    let mut state = two_v_two_bulky();
    state.side_one.pokemon[PokemonIndex::P1].hp = 0; // slot 1 fainted
    state.side_one.pokemon[PokemonIndex::P4].hp = 0;
    state.side_one.pokemon[PokemonIndex::P5].hp = 0; // bench: P2, P3 alive

    let (s1, _) = state.get_all_options_doubles();
    assert_eq!(s1.len(), 2, "the one empty slot picks one of the two bench mons");
    for sa in &s1 {
        assert_eq!(sa.actions[0], MoveChoice::None, "the surviving slot 0 does nothing");
        assert!(matches!(sa.actions[1], MoveChoice::Switch(_)), "slot 1 replaces");
    }
}

/// A slot-1 replacement is actually placed into slot 1 (not slot 0).
#[test]
fn test_doubles_replacement_lands_in_correct_slot() {
    let mut state = two_v_two_bulky();
    state.side_one.pokemon[PokemonIndex::P1].hp = 0; // slot 1 fainted

    let before_slot0 = state.side_one.active_indices[0];
    let s1 = SideAction::new([MoveChoice::None, MoveChoice::Switch(PokemonIndex::P2)]);
    let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);
    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);

    state.apply_instructions(&instrs[0].instruction_list);
    assert_eq!(
        state.side_one.active_indices[1],
        PokemonIndex::P2,
        "replacement should occupy slot 1"
    );
    assert_eq!(
        state.side_one.active_indices[0], before_slot0,
        "slot 0 is untouched by the slot-1 replacement"
    );
}

/// Ally Switch swaps the two active slots' occupants on the user's side.
#[test]
fn test_doubles_ally_switch_swaps_slots() {
    let mut state = two_v_two_bulky();
    set_move_on(&mut state, SideReference::SideOne, PokemonIndex::P0, Choices::ALLYSWITCH);

    let me0 = BattlePosition::new(SideReference::SideOne, 0);
    let s1 = SideAction::new([
        MoveChoice::Move { move_index: PokemonMoveIndex::M0, target: me0 },
        MoveChoice::None,
    ]);
    let s2 = SideAction::new([MoveChoice::None, MoveChoice::None]);

    let instrs = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    let swaps = instrs.iter().any(|si| {
        si.instruction_list.iter().any(|i| {
            *i == Instruction::SwapActiveSlots(SwapActiveSlotsInstruction {
                side_ref: SideReference::SideOne,
            })
        })
    });
    assert!(swaps, "Ally Switch should emit a SwapActiveSlots instruction");

    // Applying the produced instructions actually swaps slot 0 and slot 1.
    let before = state.side_one.active_indices;
    state.apply_instructions(&instrs[0].instruction_list);
    assert_eq!(state.side_one.active_indices[0], before[1]);
    assert_eq!(state.side_one.active_indices[1], before[0]);
}

// ---------------------------------------------------------------------------
// Phase 8: decision engines + serialization in doubles
// ---------------------------------------------------------------------------

/// Put `choice` in move slot M0 of `side`'s `idx` Pokemon and silence the rest,
/// so a slot's option fan-out is fully controlled (mirrors `set_single_move`,
/// which only ever touches side one).
fn set_single_move_on(
    state: &mut State,
    side_ref: SideReference,
    idx: PokemonIndex,
    choice: Choices,
) {
    let pkmn = &mut state.get_side(&side_ref).pokemon[idx];
    pkmn.moves[&PokemonMoveIndex::M0] = Move {
        id: choice,
        disabled: false,
        pp: 32,
        choice: MOVES.get(&choice).unwrap().clone(),
    };
    pkmn.moves[&PokemonMoveIndex::M1].pp = 0;
    pkmn.moves[&PokemonMoveIndex::M2].pp = 0;
    pkmn.moves[&PokemonMoveIndex::M3].pp = 0;
}

/// A 2v2 doubles state where every active has exactly one attacking move and no
/// living bench, so each side's combined options are just the target fan-out.
fn simple_doubles_state() -> State {
    let mut state = State::default();
    state.side_one.active_indices = [PokemonIndex::P0, PokemonIndex::P1];
    state.side_two.active_indices = [PokemonIndex::P0, PokemonIndex::P1];

    // No living bench on either side: only the four actives are alive.
    for idx in [PokemonIndex::P2, PokemonIndex::P3, PokemonIndex::P4, PokemonIndex::P5] {
        state.side_one.pokemon[idx].hp = 0;
        state.side_two.pokemon[idx].hp = 0;
    }

    for (side_ref, slots) in [
        (SideReference::SideOne, [PokemonIndex::P0, PokemonIndex::P1]),
        (SideReference::SideTwo, [PokemonIndex::P0, PokemonIndex::P1]),
    ] {
        for idx in slots {
            set_single_move_on(&mut state, side_ref, idx, Choices::TACKLE);
        }
    }
    state
}

/// Phase 8 acceptance: an MCTS search over a real doubles state runs and returns
/// combined (per-side) actions. The result rows line up one-to-one with the
/// enumerated `SideAction` options, the tree actually ran iterations, and every
/// returned move choice is a legal combined action.
#[test]
fn test_doubles_mcts_returns_valid_combined_actions() {
    let mut state = simple_doubles_state();
    let (s1_options, s2_options) = state.root_get_all_options_doubles();

    // Sanity: these are genuinely combined actions (each side picks for 2 slots).
    assert!(!s1_options.is_empty());
    assert!(!s2_options.is_empty());
    for opt in s1_options.iter() {
        assert_eq!(opt.actions.len(), 2);
    }

    let result = perform_mcts(
        &mut state,
        s1_options.clone(),
        s2_options.clone(),
        std::time::Duration::from_millis(100),
    );

    // One result row per enumerated option, in the same order.
    assert_eq!(result.s1.len(), s1_options.len());
    assert_eq!(result.s2.len(), s2_options.len());
    for (row, opt) in result.s1.iter().zip(s1_options.iter()) {
        assert_eq!(&row.move_choice, opt);
    }

    // The search actually explored the tree.
    assert!(result.iteration_count > 0);
    let total_visits: u32 = result.s1.iter().map(|r| r.visits).sum();
    assert!(total_visits > 0, "MCTS recorded no visits");

    // The recommended (most-visited) side-one action is one of the legal options.
    let best = result
        .s1
        .iter()
        .max_by_key(|r| r.visits)
        .expect("non-empty options");
    assert!(s1_options.contains(&best.move_choice));

    // The structural battle state is restored to the root after the search: all
    // four actives keep their original HP (no instructions leaked).
    // NOTE: we don't assert full `serialize()` equality because per-slot
    // terastallization reversal is a documented pre-Phase-8 deferral (the
    // `ToggleTerastallized` instruction is still per-side), which can leave a
    // slot-1 `terastallized` flag set after exploring a tera move.
    let fresh = simple_doubles_state();
    for slot in 0..2u8 {
        assert_eq!(
            state.side_one.get_active_slot_immutable(slot).hp,
            fresh.side_one.get_active_slot_immutable(slot).hp,
        );
        assert_eq!(
            state.side_two.get_active_slot_immutable(slot).hp,
            fresh.side_two.get_active_slot_immutable(slot).hp,
        );
    }
}

/// The generic decision glue dispatches to the doubles enumeration / resolution,
/// so `decision::get_all_options` matches `get_all_options_doubles` and
/// `decision::generate_instructions` matches `generate_instructions_from_actions`.
#[test]
fn test_doubles_decision_glue_dispatches_to_doubles() {
    let mut state = simple_doubles_state();

    let (glue_s1, glue_s2) = decision::get_all_options(&state);
    let (direct_s1, direct_s2) = state.get_all_options_doubles();
    assert_eq!(glue_s1, direct_s1);
    assert_eq!(glue_s2, direct_s2);

    let s1 = direct_s1[0];
    let s2 = direct_s2[0];
    let via_glue = decision::generate_instructions(&mut state, &s1, &s2, false);
    let direct = generate_instructions_from_actions(&mut state, &s1, &s2, false);
    assert_eq!(via_glue.len(), direct.len());
}

/// Round-trip a doubles state through serialize/deserialize, including the slot-1
/// active index and its per-active state (boosts / volatiles / last_used_move)
/// and the per-slot force-switch flags — none of which exist in the singles
/// format. `serialize()` must be a fixed point, and the slot-1 data must survive.
#[test]
fn test_doubles_serialization_round_trip() {
    let mut state = simple_doubles_state();

    // Distinct slot-1 active index so we can confirm it is serialized (the singles
    // format only carries the slot-0 index, defaulting slot 1 to it).
    state.side_one.pokemon[PokemonIndex::P3].hp = 100; // revive a bench mon
    state.side_one.active_indices = [PokemonIndex::P0, PokemonIndex::P3];

    // Per-active state on the slot-1 Pokemon (P3).
    {
        let slot1 = &mut state.side_one.pokemon[PokemonIndex::P3];
        slot1.attack_boost = 2;
        slot1.special_defense_boost = -1;
        slot1.speed_boost = 3;
        slot1.substitute_health = 25;
        slot1.volatile_statuses.insert(PokemonVolatileStatus::TAUNT);
        slot1.last_used_move = poke_engine::state::LastUsedMove::Move(PokemonMoveIndex::M0);
    }
    state.side_one.force_switch_slots = [false, true];

    let serialized = state.serialize();
    let restored = State::deserialize(&serialized);

    // serialize() is a fixed point across the round trip.
    assert_eq!(serialized, restored.serialize());

    // Slot-1 active index and per-active state survived.
    assert_eq!(restored.side_one.active_indices[1], PokemonIndex::P3);
    let r_slot1 = restored.side_one.get_active_slot_immutable(1);
    assert_eq!(r_slot1.attack_boost, 2);
    assert_eq!(r_slot1.special_defense_boost, -1);
    assert_eq!(r_slot1.speed_boost, 3);
    assert_eq!(r_slot1.substitute_health, 25);
    assert!(r_slot1
        .volatile_statuses
        .contains(&PokemonVolatileStatus::TAUNT));
    assert_eq!(
        r_slot1.last_used_move,
        poke_engine::state::LastUsedMove::Move(PokemonMoveIndex::M0)
    );
    assert_eq!(restored.side_one.force_switch_slots, [false, true]);
}

/// A singles-format serialized side (29 `=`-tokens, no doubles tail) still loads
/// in a doubles build: the slot-1 state simply defaults rather than panicking.
#[test]
fn test_doubles_deserializes_singles_format_state() {
    // Build a doubles state, then strip its appended doubles tokens from each side
    // to emulate an old singles-format string.
    let state = simple_doubles_state();
    let serialized = state.serialize();
    let parts: Vec<&str> = serialized.split('/').collect();
    let trim_side = |side: &str| -> String {
        side.split('=').take(29).collect::<Vec<_>>().join("=")
    };
    let singles_format = format!(
        "{}/{}/{}/{}/{}/{}",
        trim_side(parts[0]),
        trim_side(parts[1]),
        parts[2],
        parts[3],
        parts[4],
        parts[5],
    );

    // Must not panic; slot-1 active index defaults to the slot-0 index.
    let restored = State::deserialize(&singles_format);
    assert_eq!(
        restored.side_one.active_indices[1],
        restored.side_one.active_indices[0]
    );
}

/// CLI parsing of a combined doubles action: `;`-separated per-slot sub-actions,
/// each accepting an optional `,<slot>` opposing-target suffix.
#[test]
fn test_doubles_side_action_from_string() {
    let state = simple_doubles_state();

    // Two attacking sub-actions, the second aimed at opposing slot 1.
    let parsed = SideAction::from_string("tackle;tackle,1", &state.side_one, SideReference::SideOne)
        .expect("valid combined action");
    let expect = SideAction::new([
        MoveChoice::Move {
            move_index: PokemonMoveIndex::M0,
            target: BattlePosition::new(SideReference::SideTwo, 0),
        },
        MoveChoice::Move {
            move_index: PokemonMoveIndex::M0,
            target: BattlePosition::new(SideReference::SideTwo, 1),
        },
    ]);
    assert_eq!(parsed, expect);

    // A `none` sub-action leaves that slot idle.
    let parsed_none =
        SideAction::from_string("tackle;none", &state.side_one, SideReference::SideOne).unwrap();
    assert_eq!(parsed_none.actions[1], MoveChoice::None);

    // More sub-actions than slots is rejected.
    assert!(
        SideAction::from_string("tackle;tackle;tackle", &state.side_one, SideReference::SideOne)
            .is_none()
    );
}
