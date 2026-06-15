use super::abilities::{
    ability_after_damage_hit, ability_before_move, ability_end_of_turn,
    ability_modify_attack_against, ability_modify_attack_being_used, ability_on_switch_in,
    ability_on_switch_out, Abilities,
};
use super::choice_effects::{
    charge_choice_to_volatile, choice_after_damage_hit, choice_before_move, choice_hazard_clear,
    choice_special_effect, modify_choice,
};
use crate::choices::{
    Boost, Choices, Effect, Heal, MoveTarget, MultiHitMove, Secondary, SideCondition, StatBoosts,
    Status, VolatileStatus, MOVES,
};
use crate::instruction::{
    ApplyVolatileStatusInstruction, BoostInstruction, ChangeDamageDealtDamageInstruction,
    ChangeDamageDealtMoveCategoryInstruction, ChangeItemInstruction,
    ChangeSideConditionInstruction, ChangeTerrain, ChangeType,
    ChangeVolatileStatusDurationInstruction, ChangeWeather, DecrementRestTurnsInstruction,
    DecrementWishInstruction, HealInstruction, RemoveVolatileStatusInstruction,
    SetSecondMoveSwitchOutMoveInstruction, SetSleepTurnsInstruction, ToggleBatonPassingInstruction,
    ToggleDamageDealtHitSubstituteInstruction, ToggleShedTailingInstruction,
    ToggleTrickRoomInstruction,
};
use crate::instruction::{ChangeAbilityInstruction, ToggleTerastallizedInstruction};
use crate::instruction::{DecrementFutureSightInstruction, FormeChangeInstruction};
use crate::instruction::{DecrementPPInstruction, SetLastUsedMoveInstruction};

use super::damage_calc::calculate_futuresight_damage;
use super::damage_calc::{calculate_damage, type_effectiveness_modifier, DamageRolls};
use super::items::{
    item_before_move, item_end_of_turn, item_modify_attack_against, item_modify_attack_being_used,
    item_on_switch_in, Items,
};
use super::state::{MoveChoice, PokemonVolatileStatus, Terrain, Weather};
use crate::choices::{Choice, MoveCategory};
use crate::instruction::{
    ChangeStatusInstruction, DamageInstruction, Instruction, StateInstructions, SwitchInstruction,
};
use crate::state::{
    BattlePosition, LastUsedMove, Pokemon, PokemonBoostableStat, PokemonIndex, PokemonMoveIndex,
    PokemonSideCondition, PokemonStatus, PokemonType, Side, SideMovesFirst, SideReference, State,
};
use std::cmp;

#[cfg(feature = "doubles")]
use super::state::SideAction;
#[cfg(feature = "doubles")]
use crate::instruction::ToggleForceSwitchSlotInstruction;
#[cfg(feature = "doubles")]
use crate::state::ACTIVE_PER_SIDE;

#[cfg(feature = "terastallization")]
use crate::choices::MultiAccuracyMove;

#[cfg(any(feature = "gen3", feature = "gen4", feature = "gen5", feature = "gen6"))]
pub const BASE_CRIT_CHANCE: f32 = 1.0 / 16.0;

#[cfg(any(feature = "gen7", feature = "gen8", feature = "gen9"))]
pub const BASE_CRIT_CHANCE: f32 = 1.0 / 24.0;

#[cfg(any(feature = "gen3", feature = "gen4"))]
pub const MAX_SLEEP_TURNS: i8 = 4;

#[cfg(any(
    feature = "gen5",
    feature = "gen6",
    feature = "gen7",
    feature = "gen8",
    feature = "gen9"
))]
pub const MAX_SLEEP_TURNS: i8 = 3;

#[cfg(any(feature = "gen7", feature = "gen8", feature = "gen9"))]
pub const HIT_SELF_IN_CONFUSION_CHANCE: f32 = 1.0 / 3.0;

#[cfg(any(feature = "gen3", feature = "gen4", feature = "gen5", feature = "gen6"))]
pub const HIT_SELF_IN_CONFUSION_CHANCE: f32 = 1.0 / 2.0;

#[cfg(any(
    feature = "gen5",
    feature = "gen6",
    feature = "gen7",
    feature = "gen8",
    feature = "gen9"
))]
pub const CONSECUTIVE_PROTECT_CHANCE: f32 = 1.0 / 3.0;

#[cfg(any(feature = "gen3", feature = "gen4"))]
pub const CONSECUTIVE_PROTECT_CHANCE: f32 = 1.0 / 2.0;

pub const SIDE_CONDITION_DURATION: i8 = 5;
pub const TAILWIND_DURATION: i8 = 4;

const PROTECT_VOLATILES: [PokemonVolatileStatus; 6] = [
    PokemonVolatileStatus::PROTECT,
    PokemonVolatileStatus::BANEFULBUNKER,
    PokemonVolatileStatus::BURNINGBULWARK,
    PokemonVolatileStatus::SPIKYSHIELD,
    PokemonVolatileStatus::SILKTRAP,
    PokemonVolatileStatus::ENDURE,
];

fn chance_to_wake_up(turns_asleep: i8) -> f32 {
    if turns_asleep == 0 {
        0.0
    } else {
        1.0 / (1 + MAX_SLEEP_TURNS - turns_asleep) as f32
    }
}

fn set_last_used_move_as_switch(
    side: &mut Side,
    new_pokemon_index: PokemonIndex,
    switching_side_ref: SideReference,
    slot: u8,
    incoming_instructions: &mut StateInstructions,
) {
    incoming_instructions
        .instruction_list
        .push(Instruction::SetLastUsedMove(SetLastUsedMoveInstruction::new(
            switching_side_ref,
            slot,
            LastUsedMove::Switch(new_pokemon_index),
            side.get_active_slot(slot).last_used_move,
        )));
    side.get_active_slot(slot).last_used_move = LastUsedMove::Switch(new_pokemon_index);
}

fn set_last_used_move_as_move(
    side: &mut Side,
    used_move: PokemonMoveIndex,
    switching_side_ref: SideReference,
    slot: u8,
    incoming_instructions: &mut StateInstructions,
) {
    if side
        .get_active_slot(slot).volatile_statuses
        .contains(&PokemonVolatileStatus::FLINCH)
    {
        // if we were flinched after just switching in we don't want our last used move to be switch
        // this makes sure fakeout/firstimpression can't be used on the following turn
        if matches!(side.get_active_slot(slot).last_used_move, LastUsedMove::Switch(_)) {
            incoming_instructions
                .instruction_list
                .push(Instruction::SetLastUsedMove(SetLastUsedMoveInstruction::new(
                    switching_side_ref,
                    slot,
                    LastUsedMove::None,
                    side.get_active_slot(slot).last_used_move,
                )));
            side.get_active_slot(slot).last_used_move = LastUsedMove::None;
        }
        return;
    }
    match side.get_active_slot(slot).last_used_move {
        LastUsedMove::Move(last_used_move) => {
            if last_used_move == used_move {
                return;
            }
        }
        _ => {}
    }
    incoming_instructions
        .instruction_list
        .push(Instruction::SetLastUsedMove(SetLastUsedMoveInstruction::new(
            switching_side_ref,
            slot,
            LastUsedMove::Move(used_move),
            side.get_active_slot(slot).last_used_move,
        )));
    side.get_active_slot(slot).last_used_move = LastUsedMove::Move(used_move);
}

fn generate_instructions_from_switch(
    state: &mut State,
    new_pokemon_index: PokemonIndex,
    switching_side_ref: SideReference,
    incoming_instructions: &mut StateInstructions,
) {
    let should_last_used_move = state.use_last_used_move;
    #[cfg(feature = "doubles")]
    let acting_slot = state.acting_slot;
    state.apply_instructions(&incoming_instructions.instruction_list);

    let (side, opposite_side) = state.get_both_sides(&switching_side_ref);
    if side.force_switch {
        side.force_switch = false;
        match switching_side_ref {
            SideReference::SideOne => {
                incoming_instructions
                    .instruction_list
                    .push(Instruction::ToggleSideOneForceSwitch);
            }
            SideReference::SideTwo => {
                incoming_instructions
                    .instruction_list
                    .push(Instruction::ToggleSideTwoForceSwitch);
            }
        }
    }
    // Doubles: a per-slot forced switch is cleared as that slot's replacement
    // switches in. In a doubles build `force_switch` (per-side) is never set by
    // pivots (they set the per-slot flag instead), so the block above is inert.
    #[cfg(feature = "doubles")]
    if side.force_switch_slot(acting_slot) {
        side.set_force_switch_slot(acting_slot, false);
        incoming_instructions
            .instruction_list
            .push(Instruction::ToggleForceSwitchSlot(
                ToggleForceSwitchSlotInstruction {
                    side_ref: switching_side_ref,
                    slot: acting_slot,
                },
            ));
    }

    let mut baton_passing = false;
    if side.baton_passing {
        baton_passing = true;
        side.baton_passing = false;
        match switching_side_ref {
            SideReference::SideOne => {
                incoming_instructions
                    .instruction_list
                    .push(Instruction::ToggleBatonPassing(
                        ToggleBatonPassingInstruction {
                            side_ref: SideReference::SideOne,
                        },
                    ));
            }
            SideReference::SideTwo => {
                incoming_instructions
                    .instruction_list
                    .push(Instruction::ToggleBatonPassing(
                        ToggleBatonPassingInstruction {
                            side_ref: SideReference::SideTwo,
                        },
                    ));
            }
        }
    }

    let mut shed_tailing = false;
    if side.shed_tailing {
        shed_tailing = true;
        side.shed_tailing = false;
        match switching_side_ref {
            SideReference::SideOne => {
                incoming_instructions
                    .instruction_list
                    .push(Instruction::ToggleShedTailing(
                        ToggleShedTailingInstruction {
                            side_ref: SideReference::SideOne,
                        },
                    ));
            }
            SideReference::SideTwo => {
                incoming_instructions
                    .instruction_list
                    .push(Instruction::ToggleShedTailing(
                        ToggleShedTailingInstruction {
                            side_ref: SideReference::SideTwo,
                        },
                    ));
            }
        }
    }

    #[cfg(feature = "gen5")]
    if side.get_active_immutable().status == PokemonStatus::SLEEP {
        let current_active_index = side.active_indices[0];
        let active = side.get_active();
        if active.rest_turns > 0 {
            let current_rest_turns = active.rest_turns;
            incoming_instructions
                .instruction_list
                .push(Instruction::SetRestTurns(SetSleepTurnsInstruction {
                    side_ref: switching_side_ref,
                    pokemon_index: current_active_index,
                    new_turns: 3,
                    previous_turns: current_rest_turns,
                }));
            active.rest_turns = 3
        } else {
            let current_sleep_turns = active.sleep_turns;
            incoming_instructions
                .instruction_list
                .push(Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                    side_ref: switching_side_ref,
                    pokemon_index: current_active_index,
                    new_turns: 0,
                    previous_turns: current_sleep_turns,
                }));
            active.sleep_turns = 0
        }
    }

    if opposite_side
        .get_active().volatile_statuses
        .contains(&PokemonVolatileStatus::PARTIALLYTRAPPED)
    {
        // FIXME(doubles): slot
        incoming_instructions
            .instruction_list
            .push(Instruction::RemoveVolatileStatus(
                RemoveVolatileStatusInstruction::new(
                    switching_side_ref.get_other_side(),
                    0,
                    PokemonVolatileStatus::PARTIALLYTRAPPED,
                ),
            ));
        opposite_side
            .get_active().volatile_statuses
            .remove(&PokemonVolatileStatus::PARTIALLYTRAPPED);
    }

    state.re_enable_disabled_moves(
        &switching_side_ref,
        &mut incoming_instructions.instruction_list,
    );
    state.remove_volatile_statuses_on_switch(
        &switching_side_ref,
        &mut incoming_instructions.instruction_list,
        baton_passing,
        shed_tailing,
    );
    state.reset_toxic_count(
        &switching_side_ref,
        &mut incoming_instructions.instruction_list,
    );
    if !baton_passing {
        state.reset_boosts(
            &switching_side_ref,
            &mut incoming_instructions.instruction_list,
        );
    }

    ability_on_switch_out(state, &switching_side_ref, incoming_instructions);

    // Slot being switched (the acting slot; 0 in singles). A slot-1 replacement
    // — partner pivot or a multi-faint replacement — switches into slot 1.
    let switch_slot = state.actor_slot() as usize;
    let switch_instruction = Instruction::Switch(SwitchInstruction {
        side_ref: switching_side_ref,
        previous_index: state.get_side(&switching_side_ref).active_indices[switch_slot],
        next_index: new_pokemon_index,
    });

    let side = state.get_side(&switching_side_ref);
    // Mirror the Switch instruction's effect: per-active state follows the
    // active pointer (transfers kept state on baton pass; no-op otherwise).
    let previous_index = side.active_indices[switch_slot];
    side.swap_active_state(previous_index, new_pokemon_index);
    side.active_indices[switch_slot] = new_pokemon_index;
    incoming_instructions
        .instruction_list
        .push(switch_instruction);

    if should_last_used_move {
        // Switch active-swap is still slot-0 bound (per-slot switch is deferred),
        // so the outgoing Pokemon's last_used_move is recorded at slot 0 to match.
        set_last_used_move_as_switch(
            side,
            new_pokemon_index,
            switching_side_ref,
            0,
            incoming_instructions,
        );
    }

    if side.side_conditions.healing_wish > 0 {
        #[cfg(any(feature = "gen8", feature = "gen9"))]
        let mut healing_wish_consumed = false;

        #[cfg(any(
            feature = "gen3",
            feature = "gen4",
            feature = "gen5",
            feature = "gen6",
            feature = "gen7"
        ))]
        let mut healing_wish_consumed = true;

        let switched_in_pkmn = side.get_active();
        if switched_in_pkmn.hp < switched_in_pkmn.maxhp {
            let heal_amount = switched_in_pkmn.maxhp - switched_in_pkmn.hp;
            let heal_instruction = Instruction::Heal(HealInstruction::new(
                switching_side_ref,
                0,
                heal_amount,
            ));
            incoming_instructions
                .instruction_list
                .push(heal_instruction);
            switched_in_pkmn.hp += heal_amount;
            healing_wish_consumed = true;
        }
        if switched_in_pkmn.status != PokemonStatus::NONE {
            add_remove_status_instructions(
                incoming_instructions,
                new_pokemon_index,
                switching_side_ref,
                side,
            );
            healing_wish_consumed = true;
        }

        if healing_wish_consumed {
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeSideCondition(
                    ChangeSideConditionInstruction {
                        side_ref: switching_side_ref,
                        side_condition: PokemonSideCondition::HealingWish,
                        amount: -1 * side.side_conditions.healing_wish,
                    },
                ));
            side.side_conditions.healing_wish = 0;
        }
    }

    let active = side.get_active_immutable();
    if active.item != Items::HEAVYDUTYBOOTS {
        let switched_in_pkmn = side.get_active_immutable();
        if side.side_conditions.sticky_web == 1 && switched_in_pkmn.is_grounded() {
            // a pkmn switching in doesn't have any other speed drops,
            // so no need to check for going below -6
            apply_boost_instruction(
                side,
                0, // FIXME(doubles): switch-in is slot-0 bound (deferred)
                &PokemonBoostableStat::Speed,
                &-1,
                &switching_side_ref,
                &switching_side_ref,
                incoming_instructions,
            );
        }

        let side = state.get_side_immutable(&switching_side_ref);
        let switched_in_pkmn = side.get_active_immutable();
        let mut toxic_spike_instruction: Option<Instruction> = None;
        if side.side_conditions.toxic_spikes > 0 && switched_in_pkmn.is_grounded() {
            if !immune_to_status(
                &state,
                &MoveTarget::User,
                &switching_side_ref,
                &PokemonStatus::POISON,
            ) {
                if side.side_conditions.toxic_spikes == 1 {
                    toxic_spike_instruction =
                        Some(Instruction::ChangeStatus(ChangeStatusInstruction {
                            side_ref: switching_side_ref,
                            pokemon_index: side.active_indices[0],
                            old_status: switched_in_pkmn.status,
                            new_status: PokemonStatus::POISON,
                        }))
                } else if side.side_conditions.toxic_spikes == 2 {
                    toxic_spike_instruction =
                        Some(Instruction::ChangeStatus(ChangeStatusInstruction {
                            side_ref: switching_side_ref,
                            pokemon_index: side.active_indices[0],
                            old_status: switched_in_pkmn.status,
                            new_status: PokemonStatus::TOXIC,
                        }))
                }
            } else if switched_in_pkmn.has_type(&PokemonType::POISON) {
                toxic_spike_instruction = Some(Instruction::ChangeSideCondition(
                    ChangeSideConditionInstruction {
                        side_ref: switching_side_ref,
                        side_condition: PokemonSideCondition::ToxicSpikes,
                        amount: -1 * side.side_conditions.toxic_spikes,
                    },
                ))
            }

            if let Some(i) = toxic_spike_instruction {
                state.apply_one_instruction(&i);
                incoming_instructions.instruction_list.push(i);
            }
        }

        let side = state.get_side(&switching_side_ref);
        let active = side.get_active_immutable();
        if active.ability != Abilities::MAGICGUARD {
            if side.side_conditions.stealth_rock == 1 {
                let switched_in_pkmn = side.get_active();
                let multiplier = type_effectiveness_modifier(&PokemonType::ROCK, &switched_in_pkmn);

                let dmg_amount = cmp::min(
                    (switched_in_pkmn.maxhp as f32 * multiplier / 8.0) as i16,
                    switched_in_pkmn.hp,
                );
                let stealth_rock_dmg_instruction = Instruction::Damage(DamageInstruction::new(
                    switching_side_ref,
                    0,
                    dmg_amount,
                ));
                switched_in_pkmn.hp -= dmg_amount;
                incoming_instructions
                    .instruction_list
                    .push(stealth_rock_dmg_instruction);
            }

            let switched_in_pkmn = side.get_active_immutable();
            if side.side_conditions.spikes > 0 && switched_in_pkmn.is_grounded() {
                let dmg_amount = cmp::min(
                    switched_in_pkmn.maxhp * side.side_conditions.spikes as i16 / 8,
                    switched_in_pkmn.hp,
                );
                let spikes_dmg_instruction = Instruction::Damage(DamageInstruction::new(
                    switching_side_ref,
                    0,
                    dmg_amount,
                ));
                side.get_active().hp -= dmg_amount;
                incoming_instructions
                    .instruction_list
                    .push(spikes_dmg_instruction);
            }
        }
    }

    ability_on_switch_in(state, &switching_side_ref, incoming_instructions);
    item_on_switch_in(state, &switching_side_ref, incoming_instructions);

    state.reverse_instructions(&incoming_instructions.instruction_list);
}

fn generate_instructions_from_increment_side_condition(
    state: &mut State,
    side_condition: &SideCondition,
    attacking_side_reference: &SideReference,
    incoming_instructions: &mut StateInstructions,
) {
    let affected_side_ref = side_condition.target.affected_side(*attacking_side_reference);

    let max_layers = match side_condition.condition {
        PokemonSideCondition::Spikes => 3,
        PokemonSideCondition::ToxicSpikes => 2,
        _ => 1,
    };

    let affected_side = state.get_side(&affected_side_ref);
    if affected_side.get_side_condition(side_condition.condition) < max_layers {
        let ins = Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
            side_ref: affected_side_ref,
            side_condition: side_condition.condition,
            amount: 1,
        });
        affected_side.update_side_condition(side_condition.condition, 1);
        incoming_instructions.instruction_list.push(ins);
    }
}

fn generate_instructions_from_duration_side_conditions(
    state: &mut State,
    side_condition: &SideCondition,
    attacking_side_reference: &SideReference,
    incoming_instructions: &mut StateInstructions,
    duration: i8,
) {
    let affected_side_ref = side_condition.target.affected_side(*attacking_side_reference);
    if side_condition.condition == PokemonSideCondition::AuroraVeil
        && !state.weather_is_active(&Weather::HAIL)
        && !state.weather_is_active(&Weather::SNOW)
    {
        return;
    }
    let affected_side = state.get_side(&affected_side_ref);
    if affected_side.get_side_condition(side_condition.condition) == 0 {
        let ins = Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
            side_ref: affected_side_ref,
            side_condition: side_condition.condition,
            amount: duration,
        });
        affected_side.update_side_condition(side_condition.condition, duration);
        incoming_instructions.instruction_list.push(ins);
    }
}

fn generate_instructions_from_side_conditions(
    state: &mut State,
    side_condition: &SideCondition,
    attacking_side_reference: &SideReference,
    incoming_instructions: &mut StateInstructions,
) {
    match side_condition.condition {
        PokemonSideCondition::AuroraVeil
        | PokemonSideCondition::LightScreen
        | PokemonSideCondition::Reflect
        | PokemonSideCondition::Safeguard
        | PokemonSideCondition::Mist => {
            generate_instructions_from_duration_side_conditions(
                state,
                side_condition,
                attacking_side_reference,
                incoming_instructions,
                SIDE_CONDITION_DURATION,
            );
        }
        PokemonSideCondition::Tailwind => {
            generate_instructions_from_duration_side_conditions(
                state,
                side_condition,
                attacking_side_reference,
                incoming_instructions,
                TAILWIND_DURATION,
            );
        }
        _ => generate_instructions_from_increment_side_condition(
            state,
            side_condition,
            attacking_side_reference,
            incoming_instructions,
        ),
    }
}

fn get_instructions_from_volatile_statuses(
    state: &mut State,
    attacker_choice: &Choice,
    volatile_status: &VolatileStatus,
    attacking_side_reference: &SideReference,
    incoming_instructions: &mut StateInstructions,
) {
    let act_slot = state.actor_slot();
    let def_pos = state.defender_position(attacking_side_reference);
    let target_side = volatile_status.target.affected_side(*attacking_side_reference);
    // The volatile lands on the user's own slot for User/UserSide targets, and on
    // the move's resolved target position otherwise — which, for an `Ally` target,
    // is the partner slot (`target_position` was set to the ally). In singles
    // every slot is 0, so this is bit-for-bit identical.
    let target_slot = match volatile_status.target {
        MoveTarget::User | MoveTarget::UserSide => act_slot,
        _ => def_pos.slot,
    };

    if volatile_status.volatile_status == PokemonVolatileStatus::YAWN
        && immune_to_status(
            state,
            &MoveTarget::Opponent,
            &target_side,
            &PokemonStatus::SLEEP,
        )
    {
        return;
    }
    let side = state.get_side(&target_side);
    let affected_pkmn = side.get_active_slot_immutable(target_slot);
    if affected_pkmn.volatile_status_can_be_applied(
        &volatile_status.volatile_status,
        &side.get_active_slot_immutable(target_slot).volatile_statuses,
        attacker_choice.first_move,
    ) {
        let ins = Instruction::ApplyVolatileStatus(ApplyVolatileStatusInstruction::new(
            target_side,
            target_slot,
            volatile_status.volatile_status,
        ));

        side.get_active_slot(target_slot)
            .volatile_statuses
            .insert(volatile_status.volatile_status);
        incoming_instructions.instruction_list.push(ins);
    }
}

pub fn add_remove_status_instructions(
    incoming_instructions: &mut StateInstructions,
    pokemon_index: PokemonIndex,
    side_reference: SideReference,
    side: &mut Side,
) {
    /*
    Single place to check for status removals, add the necessary instructions, and update the pokemon's status

    This is necessary because of some side effects to removing statuses
    i.e. a pre-mature wake-up from rest must set rest_turns to 0
    */
    let pkmn = &mut side.pokemon[pokemon_index];
    incoming_instructions
        .instruction_list
        .push(Instruction::ChangeStatus(ChangeStatusInstruction {
            side_ref: side_reference,
            pokemon_index: pokemon_index,
            old_status: pkmn.status,
            new_status: PokemonStatus::NONE,
        }));
    match pkmn.status {
        PokemonStatus::SLEEP => {
            if pkmn.rest_turns > 0 {
                incoming_instructions
                    .instruction_list
                    .push(Instruction::SetRestTurns(SetSleepTurnsInstruction {
                        side_ref: side_reference,
                        pokemon_index,
                        new_turns: 0,
                        previous_turns: pkmn.rest_turns,
                    }));
                pkmn.rest_turns = 0;
            } else if pkmn.sleep_turns > 0 {
                incoming_instructions
                    .instruction_list
                    .push(Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                        side_ref: side_reference,
                        pokemon_index,
                        new_turns: 0,
                        previous_turns: pkmn.sleep_turns,
                    }));
                pkmn.sleep_turns = 0;
            }
        }
        PokemonStatus::TOXIC => {
            if side.side_conditions.toxic_count != 0 {
                incoming_instructions
                    .instruction_list
                    .push(Instruction::ChangeSideCondition(
                        ChangeSideConditionInstruction {
                            side_ref: side_reference,
                            side_condition: PokemonSideCondition::ToxicCount,
                            amount: -1 * side.side_conditions.toxic_count,
                        },
                    ));
                side.side_conditions.toxic_count = 0;
            }
        }
        _ => {}
    }
    pkmn.status = PokemonStatus::NONE;
}

pub fn immune_to_status(
    state: &State,
    status_target: &MoveTarget,
    target_side_ref: &SideReference,
    status: &PokemonStatus,
) -> bool {
    let (target_side, attacking_side) = state.get_both_sides_immutable(target_side_ref);
    let target_pkmn = target_side.get_active_immutable();
    let attacking_pkmn = attacking_side.get_active_immutable();

    // General Status Immunity
    match target_pkmn.ability {
        Abilities::SHIELDSDOWN => return target_pkmn.hp > target_pkmn.maxhp / 2,
        Abilities::PURIFYINGSALT => return true,
        Abilities::COMATOSE => return true,
        Abilities::LEAFGUARD => return state.weather_is_active(&Weather::SUN),
        _ => {}
    }

    if target_pkmn.status != PokemonStatus::NONE || target_pkmn.hp <= 0 {
        true
    } else if state.terrain.terrain_type == Terrain::MISTYTERRAIN && target_pkmn.is_grounded() {
        true
    } else if (target_side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::SUBSTITUTE)
        || target_side.side_conditions.safeguard > 0)
        && status_target == &MoveTarget::Opponent
    // substitute/safeguard don't block if the target is yourself (eg. rest)
    {
        true
    } else {
        // Specific status immunity
        match status {
            PokemonStatus::BURN => {
                target_pkmn.has_type(&PokemonType::FIRE)
                    || [
                        Abilities::WATERVEIL,
                        Abilities::WATERBUBBLE,
                        Abilities::THERMALEXCHANGE,
                    ]
                    .contains(&target_pkmn.ability)
            }
            PokemonStatus::FREEZE => {
                target_pkmn.has_type(&PokemonType::ICE)
                    || target_pkmn.ability == Abilities::MAGMAARMOR
                    || state.weather_is_active(&Weather::SUN)
                    || state.weather_is_active(&Weather::HARSHSUN)
            }
            PokemonStatus::SLEEP => {
                (state.terrain.terrain_type == Terrain::ELECTRICTERRAIN
                    && target_pkmn.is_grounded())
                    || [
                        Abilities::INSOMNIA,
                        Abilities::SWEETVEIL,
                        Abilities::VITALSPIRIT,
                    ]
                    .contains(&target_pkmn.ability)
                    || (status_target == &MoveTarget::Opponent
                        && target_side.has_alive_non_rested_sleeping_pkmn())
                // sleep clause
            }

            #[cfg(any(feature = "gen6", feature = "gen7", feature = "gen8", feature = "gen9"))]
            PokemonStatus::PARALYZE => {
                target_pkmn.has_type(&PokemonType::ELECTRIC)
                    || target_pkmn.ability == Abilities::LIMBER
            }

            #[cfg(any(feature = "gen4", feature = "gen5", feature = "gen3"))]
            PokemonStatus::PARALYZE => target_pkmn.ability == Abilities::LIMBER,

            PokemonStatus::POISON | PokemonStatus::TOXIC => {
                ((target_pkmn.has_type(&PokemonType::POISON)
                    || target_pkmn.has_type(&PokemonType::STEEL))
                    && attacking_pkmn.ability != Abilities::CORROSION)
                    || [Abilities::IMMUNITY, Abilities::PASTELVEIL].contains(&target_pkmn.ability)
            }
            _ => false,
        }
    }
}

fn get_instructions_from_status_effects(
    state: &mut State,
    status: &Status,
    attacking_side_reference: &SideReference,
    incoming_instructions: &mut StateInstructions,
    hit_sub: bool,
) {
    let act_slot = state.actor_slot();
    let def_pos = state.defender_position(attacking_side_reference);
    let target_side_ref = status.target.affected_side(*attacking_side_reference);
    let target_slot = if status.target.targets_user_side() {
        act_slot
    } else {
        def_pos.slot
    };

    if hit_sub || immune_to_status(state, &status.target, &target_side_ref, &status.status) {
        return;
    }

    let target_side = state.get_side(&target_side_ref);
    let target_side_active = target_side.active_indices[0];
    let target_pkmn = target_side.get_active();

    let instruction = if target_pkmn.item == Items::LUMBERRY {
        target_pkmn.item = Items::NONE;
        Instruction::ChangeItem(ChangeItemInstruction::new(
            target_side_ref,
            target_slot,
            Items::LUMBERRY,
            Items::NONE,
        ))
    } else if target_pkmn.item == Items::CHESTOBERRY && status.status == PokemonStatus::SLEEP {
        target_pkmn.item = Items::NONE;
        Instruction::ChangeItem(ChangeItemInstruction::new(
            target_side_ref,
            target_slot,
            Items::CHESTOBERRY,
            Items::NONE,
        ))
    } else {
        let old_status = target_pkmn.status;
        target_pkmn.status = status.status;
        Instruction::ChangeStatus(ChangeStatusInstruction {
            side_ref: target_side_ref,
            pokemon_index: target_side_active,
            old_status,
            new_status: status.status,
        })
    };
    incoming_instructions.instruction_list.push(instruction);
}

pub fn get_boost_amount(side: &Side, slot: u8, boost: &PokemonBoostableStat, amount: i8) -> i8 {
    /*
    returns that amount that can actually be applied from the attempted boost amount
        e.g. using swordsdance at +5 attack would result in a +1 boost instead of +2
    */
    let active = side.get_active_slot_immutable(slot);
    let current_boost = match boost {
        PokemonBoostableStat::Attack => active.attack_boost,
        PokemonBoostableStat::Defense => active.defense_boost,
        PokemonBoostableStat::SpecialAttack => active.special_attack_boost,
        PokemonBoostableStat::SpecialDefense => active.special_defense_boost,
        PokemonBoostableStat::Speed => active.speed_boost,
        PokemonBoostableStat::Evasion => active.evasion_boost,
        PokemonBoostableStat::Accuracy => active.accuracy_boost,
    };

    if amount > 0 {
        return cmp::min(6 - current_boost, amount);
    } else if amount < 0 {
        return cmp::max(-6 - current_boost, amount);
    }
    0
}

pub fn apply_boost_instruction(
    target_side: &mut Side,
    target_slot: u8,
    stat: &PokemonBoostableStat,
    boost: &i8,
    attacking_side_ref: &SideReference,
    target_side_ref: &SideReference,
    instructions: &mut StateInstructions,
) -> bool {
    // Single point for checking whether a boost can be applied to a pokemon
    // along with side effects that that boost
    // applies the boost & side effects if applicable
    // returns whether the requested boost was actually applied
    // `target_slot` is which active on `target_side` is boosted (always 0 in singles).
    let mut boost_was_applied = false;
    let target_pkmn = target_side.get_active_slot_immutable(target_slot);
    let target_pkmn_ability = target_pkmn.ability;

    if boost != &0
        && !(target_side_ref != attacking_side_ref
            && target_pkmn.immune_to_stats_lowered_by_opponent(
                &stat,
                &target_side
                    .get_active_slot_immutable(target_slot)
                    .volatile_statuses,
            ))
        && target_pkmn.hp != 0
    {
        let mut boost_amount = *boost;
        if target_pkmn_ability == Abilities::CONTRARY {
            boost_amount *= -1;
        }
        boost_amount = get_boost_amount(target_side, target_slot, &stat, boost_amount);
        if boost_amount != 0 {
            boost_was_applied = true;
            let target_active = target_side.get_active_slot(target_slot);
            match stat {
                PokemonBoostableStat::Attack => target_active.attack_boost += boost_amount,
                PokemonBoostableStat::Defense => target_active.defense_boost += boost_amount,
                PokemonBoostableStat::SpecialAttack => {
                    target_active.special_attack_boost += boost_amount
                }
                PokemonBoostableStat::SpecialDefense => {
                    target_active.special_defense_boost += boost_amount
                }
                PokemonBoostableStat::Speed => target_active.speed_boost += boost_amount,
                PokemonBoostableStat::Evasion => target_active.evasion_boost += boost_amount,
                PokemonBoostableStat::Accuracy => target_active.accuracy_boost += boost_amount,
            }
            instructions
                .instruction_list
                .push(Instruction::Boost(BoostInstruction::new(
                    *target_side_ref,
                    target_slot,
                    *stat,
                    boost_amount,
                )));

            if boost_amount < 0 {
                if target_pkmn_ability == Abilities::DEFIANT
                    && attacking_side_ref != target_side_ref
                    && target_side.get_active_slot(target_slot).attack_boost < 6
                {
                    let defiant_boost_amount =
                        cmp::min(6 - target_side.get_active_slot(target_slot).attack_boost, 2);
                    target_side.get_active_slot(target_slot).attack_boost += defiant_boost_amount;
                    instructions
                        .instruction_list
                        .push(Instruction::Boost(BoostInstruction::new(
                            *target_side_ref,
                            target_slot,
                            PokemonBoostableStat::Attack,
                            defiant_boost_amount,
                        )));
                } else if target_pkmn_ability == Abilities::COMPETITIVE
                    && attacking_side_ref != target_side_ref
                    && target_side.get_active_slot(target_slot).special_attack_boost < 6
                {
                    let competitive_boost_amount = cmp::min(
                        6 - target_side
                            .get_active_slot(target_slot)
                            .special_attack_boost,
                        2,
                    );
                    target_side
                        .get_active_slot(target_slot)
                        .special_attack_boost += competitive_boost_amount;
                    instructions
                        .instruction_list
                        .push(Instruction::Boost(BoostInstruction::new(
                            *target_side_ref,
                            target_slot,
                            PokemonBoostableStat::SpecialAttack,
                            competitive_boost_amount,
                        )));
                }
            }
        }
    }
    boost_was_applied
}

fn get_instructions_from_boosts(
    state: &mut State,
    boosts: &Boost,
    attacking_side_reference: &SideReference,
    incoming_instructions: &mut StateInstructions,
) {
    let target_side_ref = boosts.target.affected_side(*attacking_side_reference);
    // The boost lands on the user's own slot for User/UserSide, and on the move's
    // resolved target otherwise — which for an `Ally` target is the partner slot.
    // All slots are 0 in singles, so this is bit-for-bit identical there.
    let target_slot = match boosts.target {
        MoveTarget::User | MoveTarget::UserSide => state.actor_slot(),
        _ => state.defender_position(attacking_side_reference).slot,
    };
    let boostable_stats = boosts.boosts.get_as_pokemon_boostable();
    for (pkmn_boostable_stat, boost) in boostable_stats.iter().filter(|(_, b)| b != &0) {
        let side = state.get_side(&target_side_ref);
        apply_boost_instruction(
            side,
            target_slot,
            pkmn_boostable_stat,
            boost,
            attacking_side_reference,
            &target_side_ref,
            incoming_instructions,
        );
    }
}

fn compare_health_with_damage_multiples(max_damage: i16, health: i16) -> (i16, i16) {
    let max_damage_f32 = max_damage as f32;
    let health_f32 = health as f32;

    let mut total_less_than = 0;
    let mut num_less_than = 0;
    let mut num_greater_than = 0;
    let increment = max_damage as f32 * 0.01;
    let mut damage = max_damage_f32 * 0.85;
    for _ in 0..16 {
        if damage < health_f32 {
            total_less_than += damage as i16;
            num_less_than += 1;
        } else if damage > health_f32 {
            num_greater_than += 1;
        }
        damage += increment;
    }

    (total_less_than / num_less_than, num_greater_than)
}

fn get_instructions_from_secondaries(
    state: &mut State,
    attacker_choice: &Choice,
    secondaries: &Vec<Secondary>,
    side_reference: &SideReference,
    incoming_instructions: StateInstructions,
    hit_sub: bool,
) -> Vec<StateInstructions> {
    let act_slot = state.actor_slot();
    let def_pos = state.defender_position(side_reference);
    let mut return_instruction_list = Vec::with_capacity(4);
    return_instruction_list.push(incoming_instructions);

    for secondary in secondaries {
        if secondary.target == MoveTarget::Opponent && hit_sub {
            continue;
        }
        let secondary_percent_hit = (secondary.chance / 100.0).min(1.0);

        let mut i = 0;
        while i < return_instruction_list.len() {
            let mut secondary_hit_instructions = return_instruction_list.remove(i);

            if secondary_percent_hit < 1.0 {
                let mut secondary_miss_instructions = secondary_hit_instructions.clone();
                secondary_miss_instructions.update_percentage(1.0 - secondary_percent_hit);
                return_instruction_list.insert(i, secondary_miss_instructions);
                i += 1;
            }

            if secondary_percent_hit > 0.0 {
                secondary_hit_instructions.update_percentage(secondary_percent_hit);

                state.apply_instructions(&secondary_hit_instructions.instruction_list);
                match &secondary.effect {
                    Effect::VolatileStatus(volatile_status) => {
                        get_instructions_from_volatile_statuses(
                            state,
                            attacker_choice,
                            &VolatileStatus {
                                target: secondary.target.clone(),
                                volatile_status: volatile_status.clone(),
                            },
                            side_reference,
                            &mut secondary_hit_instructions,
                        );
                    }
                    Effect::Boost(boost) => {
                        get_instructions_from_boosts(
                            state,
                            &Boost {
                                target: secondary.target.clone(),
                                boosts: boost.clone(),
                            },
                            side_reference,
                            &mut secondary_hit_instructions,
                        );
                    }
                    Effect::Status(status) => {
                        get_instructions_from_status_effects(
                            state,
                            &Status {
                                target: secondary.target.clone(),
                                status: status.clone(),
                            },
                            side_reference,
                            &mut secondary_hit_instructions,
                            hit_sub,
                        );
                    }
                    Effect::Heal(heal_amount) => {
                        get_instructions_from_heal(
                            state,
                            &Heal {
                                target: secondary.target.clone(),
                                amount: *heal_amount,
                            },
                            side_reference,
                            &mut secondary_hit_instructions,
                        );
                    }
                    Effect::RemoveItem => {
                        let secondary_target_side_ref =
                            secondary.target.affected_side(*side_reference);
                        let secondary_target_slot = if secondary.target.targets_user_side() {
                            act_slot
                        } else {
                            def_pos.slot
                        };
                        let target_pkmn = state.get_side(&secondary_target_side_ref).get_active();
                        secondary_hit_instructions
                            .instruction_list
                            .push(Instruction::ChangeItem(ChangeItemInstruction::new(
                                secondary_target_side_ref,
                                secondary_target_slot,
                                target_pkmn.item.clone(),
                                Items::NONE,
                            )));
                        target_pkmn.item = Items::NONE;
                    }
                }
                state.reverse_instructions(&secondary_hit_instructions.instruction_list);
                return_instruction_list.insert(i, secondary_hit_instructions);
                i += 1; // Increment i only if we didn't remove an element
            }
        }
    }

    return_instruction_list
}

fn get_instructions_from_heal(
    state: &mut State,
    heal: &Heal,
    attacking_side_reference: &SideReference,
    incoming_instructions: &mut StateInstructions,
) {
    let act_slot = state.actor_slot();
    let def_pos = state.defender_position(attacking_side_reference);
    let target_side_ref = heal.target.affected_side(*attacking_side_reference);
    // User/UserSide heals the acting slot; an `Ally`/`Opponent` heal lands on the
    // move's resolved target (the partner for an ally-target move). Slot 0 in singles.
    let target_slot = match heal.target {
        MoveTarget::User | MoveTarget::UserSide => act_slot,
        _ => def_pos.slot,
    };

    let target_pkmn = state.get_side(&target_side_ref).get_active_slot(target_slot);

    let mut health_recovered = (heal.amount * target_pkmn.maxhp as f32) as i16;
    let final_health = target_pkmn.hp + health_recovered;
    if final_health > target_pkmn.maxhp {
        health_recovered -= final_health - target_pkmn.maxhp;
    } else if final_health < 0 {
        health_recovered -= final_health;
    }

    if health_recovered != 0 {
        let ins = Instruction::Heal(HealInstruction::new(
            target_side_ref,
            target_slot,
            health_recovered,
        ));
        target_pkmn.hp += health_recovered;
        incoming_instructions.instruction_list.push(ins);
    }
}

fn boosted_accuracy(accuracy_boost: i8) -> f32 {
    if accuracy_boost < 0 {
        3.0 / (3.0 - accuracy_boost as f32)
    } else {
        (3.0 + accuracy_boost as f32) / 3.0
    }
}

fn check_move_hit_or_miss(
    state: &mut State,
    choice: &Choice,
    attacking_side_ref: &SideReference,
    damage: Option<(i16, i16)>,
    incoming_instructions: &mut StateInstructions,
    frozen_instructions: &mut Vec<StateInstructions>,
) {
    /*
    Checks whether a move can miss

    If the move can miss - adds it to `frozen_instructions`, signifying that the rest of the
    half-turn will not run.

    Otherwise, update the incoming instructions' percent_hit to reflect the chance of the move hitting
    */
    let act_slot = state.actor_slot();
    let _def_pos = state.defender_position(attacking_side_ref);
    let attacking_side = state.get_side(attacking_side_ref);
    let attacking_pokemon = attacking_side.get_active_immutable();

    let mut percent_hit =
        ((choice.accuracy / 100.0) * boosted_accuracy(attacking_side.get_active_immutable().accuracy_boost)).min(1.0);
    if Some((0, 0)) == damage {
        percent_hit = 0.0;
    }

    if percent_hit < 1.0 {
        let mut move_missed_instruction = incoming_instructions.clone();
        move_missed_instruction.update_percentage(1.0 - percent_hit);
        if let Some(crash_fraction) = choice.crash {
            let crash_amount = (attacking_pokemon.maxhp as f32 * crash_fraction) as i16;
            let crash_instruction = Instruction::Damage(DamageInstruction::new(
                *attacking_side_ref,
                act_slot,
                cmp::min(crash_amount, attacking_pokemon.hp),
            ));

            move_missed_instruction
                .instruction_list
                .push(crash_instruction);
        }

        if Items::BLUNDERPOLICY == attacking_pokemon.item {
            let boost_amount =
                get_boost_amount(attacking_side, act_slot, &PokemonBoostableStat::Speed, 2);
            move_missed_instruction
                .instruction_list
                .push(Instruction::Boost(BoostInstruction::new(
                    *attacking_side_ref,
                    act_slot,
                    PokemonBoostableStat::Speed,
                    boost_amount,
                )));
            move_missed_instruction
                .instruction_list
                .push(Instruction::ChangeItem(ChangeItemInstruction::new(
                    *attacking_side_ref,
                    act_slot,
                    Items::BLUNDERPOLICY,
                    Items::NONE,
                )));
        }

        frozen_instructions.push(move_missed_instruction);
    }
    incoming_instructions.update_percentage(percent_hit);
}

fn get_instructions_from_drag(
    state: &mut State,
    attacking_side_reference: &SideReference,
    incoming_instructions: StateInstructions,
    frozen_instructions: &mut Vec<StateInstructions>,
) {
    // The drag targets the move's current target side (a chosen foe in doubles).
    let def_pos = state.defender_position(attacking_side_reference);
    let defending_side = state.get_side(&def_pos.side);
    if defending_side.get_active_slot_immutable(def_pos.slot).hp == 0 {
        state.reverse_instructions(&incoming_instructions.instruction_list);
        frozen_instructions.push(incoming_instructions);
        return;
    }

    let defending_side_alive_reserve_indices = defending_side.get_alive_pkmn_indices();

    state.reverse_instructions(&incoming_instructions.instruction_list);

    let num_alive_reserve = defending_side_alive_reserve_indices.len();
    if num_alive_reserve == 0 {
        frozen_instructions.push(incoming_instructions);
        return;
    }

    for pkmn_id in defending_side_alive_reserve_indices {
        let mut cloned_instructions = incoming_instructions.clone();
        generate_instructions_from_switch(
            state,
            pkmn_id,
            def_pos.side,
            &mut cloned_instructions,
        );
        cloned_instructions.update_percentage(1.0 / num_alive_reserve as f32);
        frozen_instructions.push(cloned_instructions);
    }
}

fn reset_damage_dealt(
    active: &Pokemon,
    side_reference: &SideReference,
    slot: u8,
    incoming_instructions: &mut StateInstructions,
) {
    // This creates instructions but does not modify the Pokemon
    // because this function is called before the state applies the instructions

    if active.damage_dealt.damage != 0 {
        incoming_instructions
            .instruction_list
            .push(Instruction::ChangeDamageDealtDamage(
                ChangeDamageDealtDamageInstruction::new(
                    *side_reference,
                    slot,
                    0 - active.damage_dealt.damage,
                ),
            ));
    }
    if active.damage_dealt.move_category != MoveCategory::Physical {
        incoming_instructions
            .instruction_list
            .push(Instruction::ChangeDamageDealtMoveCatagory(
                ChangeDamageDealtMoveCategoryInstruction::new(
                    *side_reference,
                    slot,
                    MoveCategory::Physical,
                    active.damage_dealt.move_category,
                ),
            ));
    }
    if active.damage_dealt.hit_substitute {
        incoming_instructions
            .instruction_list
            .push(Instruction::ToggleDamageDealtHitSubstitute(
                ToggleDamageDealtHitSubstituteInstruction::new(*side_reference, slot),
            ));
    }
}

fn set_damage_dealt(
    attacker: &mut Pokemon,
    attacking_side_ref: &SideReference,
    slot: u8,
    damage_dealt: i16,
    choice: &Choice,
    hit_substitute: bool,
    incoming_instructions: &mut StateInstructions,
) {
    if attacker.damage_dealt.damage != damage_dealt {
        incoming_instructions
            .instruction_list
            .push(Instruction::ChangeDamageDealtDamage(
                ChangeDamageDealtDamageInstruction::new(
                    *attacking_side_ref,
                    slot,
                    damage_dealt - attacker.damage_dealt.damage,
                ),
            ));
        attacker.damage_dealt.damage = damage_dealt;
    }

    if attacker.damage_dealt.move_category != choice.category {
        incoming_instructions
            .instruction_list
            .push(Instruction::ChangeDamageDealtMoveCatagory(
                ChangeDamageDealtMoveCategoryInstruction::new(
                    *attacking_side_ref,
                    slot,
                    choice.category,
                    attacker.damage_dealt.move_category,
                ),
            ));
        attacker.damage_dealt.move_category = choice.category;
    }

    if attacker.damage_dealt.hit_substitute != hit_substitute {
        incoming_instructions
            .instruction_list
            .push(Instruction::ToggleDamageDealtHitSubstitute(
                ToggleDamageDealtHitSubstituteInstruction::new(*attacking_side_ref, slot),
            ));
        attacker.damage_dealt.hit_substitute = hit_substitute;
    }
}

fn generate_instructions_from_damage(
    mut state: &mut State,
    choice: &mut Choice,
    calculated_damage: i16,
    attacking_side_ref: &SideReference,
    mut incoming_instructions: &mut StateInstructions,
) -> bool {
    /*
    TODO:
        - arbitrary other after_move as well from the old engine (triggers on hit OR miss)
            - dig/dive/bounce/fly volatilestatus
    */
    let mut hit_sub = false;
    // Doubles: resolve the actual attacker/defender positions instead of assuming
    // "this side's slot 0" and "the opposing slot 0". `defender_position` is the
    // move's current target (a chosen foe, or an ally hit by a spread move);
    // `actor_slot` is the attacking slot. In singles these reduce to slot 0 and
    // the opposing slot 0, so the resolved instructions are identical.
    let acting_slot = state.actor_slot();
    let target_pos = state.defender_position(attacking_side_ref);
    let attacker_pos = BattlePosition::new(*attacking_side_ref, acting_slot);

    if calculated_damage <= 0 {
        if let Some(crash_fraction) = choice.crash {
            let attacking_pokemon = state
                .get_side(attacking_side_ref)
                .get_active_slot(acting_slot);
            let crash_amount = (attacking_pokemon.maxhp as f32 * crash_fraction) as i16;
            let damage_taken = cmp::min(crash_amount, attacking_pokemon.hp);
            attacking_pokemon.hp -= damage_taken;
            incoming_instructions
                .instruction_list
                .push(Instruction::Damage(DamageInstruction::new(
                    *attacking_side_ref,
                    acting_slot,
                    damage_taken,
                )));
        }
        return hit_sub;
    }

    let percent_hit = (choice.accuracy / 100.0).min(1.0);

    if percent_hit > 0.0 {
        let should_use_damage_dealt = state.use_damage_dealt;
        let mut damage_dealt;
        {
            let (attacking_pokemon, defending_pokemon) =
                state.get_two_actives(attacker_pos, target_pos);
            if defending_pokemon
                .volatile_statuses
                .contains(&PokemonVolatileStatus::SUBSTITUTE)
                && !choice.flags.sound
                && attacking_pokemon.ability != Abilities::INFILTRATOR
            {
                damage_dealt = cmp::min(calculated_damage, defending_pokemon.substitute_health);
                let substitute_damage_dealt = cmp::min(calculated_damage, damage_dealt);
                defending_pokemon.substitute_health -= substitute_damage_dealt;
                incoming_instructions
                    .instruction_list
                    .push(Instruction::DamageSubstitute(DamageInstruction::new(
                        target_pos.side,
                        target_pos.slot,
                        substitute_damage_dealt,
                    )));

                if should_use_damage_dealt {
                    set_damage_dealt(
                        attacking_pokemon,
                        attacking_side_ref,
                        acting_slot,
                        damage_dealt,
                        choice,
                        true,
                        &mut incoming_instructions,
                    );
                }

                if defending_pokemon
                    .volatile_statuses
                    .contains(&PokemonVolatileStatus::SUBSTITUTE)
                    && defending_pokemon.substitute_health == 0
                {
                    incoming_instructions
                        .instruction_list
                        .push(Instruction::RemoveVolatileStatus(
                            RemoveVolatileStatusInstruction::new(
                                target_pos.side,
                                target_pos.slot,
                                PokemonVolatileStatus::SUBSTITUTE,
                            ),
                        ));
                    defending_pokemon
                        .volatile_statuses
                        .remove(&PokemonVolatileStatus::SUBSTITUTE);
                }

                hit_sub = true;
            } else {
                let has_endure = defending_pokemon
                    .volatile_statuses
                    .contains(&PokemonVolatileStatus::ENDURE);
                let mut knocked_out = false;
                damage_dealt = cmp::min(calculated_damage, defending_pokemon.hp);
                if damage_dealt != 0 {
                    if has_endure
                        || ((defending_pokemon.ability == Abilities::STURDY
                            || defending_pokemon.item == Items::FOCUSSASH)
                            && defending_pokemon.maxhp == defending_pokemon.hp)
                    {
                        damage_dealt -= 1;
                    }

                    if damage_dealt >= defending_pokemon.hp {
                        knocked_out = true;
                    }

                    defending_pokemon.hp -= damage_dealt;
                    incoming_instructions
                        .instruction_list
                        .push(Instruction::Damage(DamageInstruction::new(
                            target_pos.side,
                            target_pos.slot,
                            damage_dealt,
                        )));

                    if knocked_out
                        && defending_pokemon
                            .volatile_statuses
                            .contains(&PokemonVolatileStatus::DESTINYBOND)
                    {
                        incoming_instructions
                            .instruction_list
                            .push(Instruction::Damage(DamageInstruction::new(
                                *attacking_side_ref,
                                acting_slot,
                                attacking_pokemon.hp,
                            )));
                        attacking_pokemon.hp = 0;
                    }

                    if should_use_damage_dealt {
                        set_damage_dealt(
                            attacking_pokemon,
                            attacking_side_ref,
                            acting_slot,
                            damage_dealt,
                            choice,
                            false,
                            &mut incoming_instructions,
                        );
                    }
                }
            }
        }

        // `ability_after_damage_hit` runs only on a non-substitute damaging hit,
        // matching the original control flow. It re-borrows `state`, so it must
        // run after the two-active borrow above is released.
        if !hit_sub && damage_dealt != 0 {
            ability_after_damage_hit(
                &mut state,
                choice,
                attacking_side_ref,
                damage_dealt,
                &mut incoming_instructions,
            );
        }

        let attacking_pokemon = state
            .get_side(attacking_side_ref)
            .get_active_slot(acting_slot);
        if let Some(drain_fraction) = choice.drain {
            let drain_amount = (damage_dealt as f32 * drain_fraction) as i16;
            let heal_amount =
                cmp::min(drain_amount, attacking_pokemon.maxhp - attacking_pokemon.hp);
            if heal_amount != 0 {
                attacking_pokemon.hp += heal_amount;
                incoming_instructions
                    .instruction_list
                    .push(Instruction::Heal(HealInstruction::new(
                        *attacking_side_ref,
                        acting_slot,
                        heal_amount,
                    )));
            }
        }

        let attacking_pokemon = state
            .get_side(attacking_side_ref)
            .get_active_slot(acting_slot);
        if let Some(recoil_fraction) = choice.recoil {
            let recoil_amount = (damage_dealt as f32 * recoil_fraction) as i16;
            let damage_amount = cmp::min(recoil_amount, attacking_pokemon.hp);
            attacking_pokemon.hp -= damage_amount;
            incoming_instructions
                .instruction_list
                .push(Instruction::Damage(DamageInstruction::new(
                    *attacking_side_ref,
                    acting_slot,
                    damage_amount,
                )));
        }
        choice_after_damage_hit(
            &mut state,
            &choice,
            attacking_side_ref,
            &mut incoming_instructions,
            hit_sub,
        );
    }
    hit_sub
}

fn move_has_no_effect(state: &State, choice: &Choice, attacking_side_ref: &SideReference) -> bool {
    // Read the move's actual target (a chosen foe in doubles, the opposing slot
    // 0 in singles) rather than assuming the opposing active.
    let def_pos = state.defender_position(attacking_side_ref);
    let defender = state
        .get_side_immutable(&def_pos.side)
        .get_active_slot_immutable(def_pos.slot);

    #[cfg(any(feature = "gen6", feature = "gen7", feature = "gen8", feature = "gen9"))]
    if choice.flags.powder
        && choice.target.targets_opponent_side()
        && defender.has_type(&PokemonType::GRASS)
    {
        return true;
    }

    if choice.move_type == PokemonType::ELECTRIC
        && choice.target.targets_opponent_side()
        && defender.has_type(&PokemonType::GROUND)
    {
        return true;
    } else if choice.move_id == Choices::ENCORE {
        return match state
            .get_side_immutable(&def_pos.side)
            .get_active_slot_immutable(def_pos.slot)
            .last_used_move
        {
            LastUsedMove::None => true,
            LastUsedMove::Move(_) => false,
            LastUsedMove::Switch(_) => true,
        };
    } else if state.terrain_is_active(&Terrain::PSYCHICTERRAIN)
        && defender.is_grounded()
        && choice.target.targets_opponent_side()
        && choice.priority > 0
    {
        return true;
    }
    false
}

fn cannot_use_move(state: &State, choice: &Choice, attacking_side_ref: &SideReference) -> bool {
    let acting_slot = state.actor_slot();
    let def_pos = state.defender_position(attacking_side_ref);
    let attacking_active = state
        .get_side_immutable(attacking_side_ref)
        .get_active_slot_immutable(acting_slot);

    // If the (single) target has 0 hp, you can't use a non-status move. For a
    // spread move a single fainted target does not stop it (it still hits the
    // remaining living targets), so the check is skipped there.
    if !choice.target.hits_multiple_targets()
        && state
            .get_side_immutable(&def_pos.side)
            .get_active_slot_immutable(def_pos.slot)
            .hp
            == 0
        && choice.category != MoveCategory::Status
    {
        return true;
    }

    // If you were taunted, you can't use a Physical/Special move
    if attacking_active
        .volatile_statuses
        .contains(&PokemonVolatileStatus::TAUNT)
        && matches!(choice.category, MoveCategory::Status)
    {
        return true;
    } else if attacking_active
        .volatile_statuses
        .contains(&PokemonVolatileStatus::FLINCH)
    {
        return true;
    } else if choice.flags.heal
        && attacking_active
            .volatile_statuses
            .contains(&PokemonVolatileStatus::HEALBLOCK)
    {
        return true;
    }
    false
}

#[cfg(feature = "terastallization")]
fn terastallized_base_power_floor(
    state: &mut State,
    choice: &mut Choice,
    attacking_side: &SideReference,
) {
    let attacker = state
        .get_side_immutable(attacking_side)
        .get_active_immutable();

    if attacker.terastallized
        && choice.move_type == attacker.tera_type
        && choice.base_power < 60.0
        && choice.priority <= 0
        && choice.multi_hit() == MultiHitMove::None
        && choice.multi_accuracy() == MultiAccuracyMove::None
    {
        choice.base_power = 60.0;
    }
}

fn before_move(
    state: &mut State,
    choice: &mut Choice,
    defender_choice: &Choice,
    attacking_side: &SideReference,
    incoming_instructions: &mut StateInstructions,
) {
    #[cfg(feature = "terastallization")]
    terastallized_base_power_floor(state, choice, attacking_side);

    ability_before_move(state, choice, attacking_side, incoming_instructions);
    item_before_move(state, choice, attacking_side, incoming_instructions);
    choice_before_move(state, choice, attacking_side, incoming_instructions);

    modify_choice(state, choice, defender_choice, attacking_side);

    ability_modify_attack_being_used(state, choice, defender_choice, attacking_side);
    ability_modify_attack_against(state, choice, defender_choice, attacking_side);

    item_modify_attack_being_used(state, choice, attacking_side);
    item_modify_attack_against(state, choice, attacking_side);

    // Slot the move is aimed at (the chosen foe in doubles; opposing slot 0 in
    // singles). Captured before borrowing the sides below.
    let def_slot = state.defender_position(attacking_side).slot;

    // Doubles area protection: Wide Guard blocks spread moves and Quick Guard
    // blocks increased-priority moves aimed at a side that put the guard up this
    // turn. Both protect the whole side, so this is keyed on the target side's
    // side conditions, not a per-slot volatile.
    #[cfg(feature = "doubles")]
    if choice.target.targets_opponent_side() && choice.category != MoveCategory::Status {
        let target_conditions = &state
            .get_side_immutable(&attacking_side.get_other_side())
            .side_conditions;
        let blocked = (choice.target.hits_multiple_targets()
            && target_conditions.wide_guard > 0)
            || (choice.priority > 0 && target_conditions.quick_guard > 0);
        if blocked {
            choice.remove_effects_for_protect();
            if choice.crash.is_some() {
                choice.accuracy = 0.0;
            }
        }
    }

    /*
        TODO: this needs to be here because from_drag is called after the substitute volatilestatus
            has already been removed
    */
    let (attacking_side, defending_side) = state.get_both_sides_immutable(attacking_side);
    if defending_side
        .get_active_slot_immutable(def_slot).volatile_statuses
        .contains(&PokemonVolatileStatus::SUBSTITUTE)
        && choice.category != MoveCategory::Status
    {
        choice.flags.drag = false;
    }

    // Update Choice for `charge` moves
    if choice.flags.charge {
        let charge_volatile_status = charge_choice_to_volatile(&choice.move_id);
        if !attacking_side
            .get_active_immutable().volatile_statuses
            .contains(&charge_volatile_status)
        {
            choice.remove_all_effects();
            choice.volatile_status = Some(VolatileStatus {
                target: MoveTarget::User,
                volatile_status: charge_volatile_status,
            });
        }
    }

    // modify choice if defender has protect active
    if (defending_side
        .get_active_slot_immutable(def_slot).volatile_statuses
        .contains(&PokemonVolatileStatus::PROTECT)
        || defending_side
            .get_active_slot_immutable(def_slot).volatile_statuses
            .contains(&PokemonVolatileStatus::SPIKYSHIELD)
        || defending_side
            .get_active_slot_immutable(def_slot).volatile_statuses
            .contains(&PokemonVolatileStatus::BANEFULBUNKER)
        || defending_side
            .get_active_slot_immutable(def_slot).volatile_statuses
            .contains(&PokemonVolatileStatus::BURNINGBULWARK)
        || defending_side
            .get_active_slot_immutable(def_slot).volatile_statuses
            .contains(&PokemonVolatileStatus::SILKTRAP))
        && choice.flags.protect
    {
        choice.remove_effects_for_protect();
        if choice.crash.is_some() {
            choice.accuracy = 0.0;
        }

        if defending_side
            .get_active_slot_immutable(def_slot).volatile_statuses
            .contains(&PokemonVolatileStatus::SPIKYSHIELD)
            && choice.flags.contact
        {
            choice.heal = Some(Heal {
                target: MoveTarget::User,
                amount: -0.125,
            })
        } else if defending_side
            .get_active_slot_immutable(def_slot).volatile_statuses
            .contains(&PokemonVolatileStatus::BANEFULBUNKER)
            && choice.flags.contact
        {
            choice.status = Some(Status {
                target: MoveTarget::User,
                status: PokemonStatus::POISON,
            })
        } else if defending_side
            .get_active_slot_immutable(def_slot).volatile_statuses
            .contains(&PokemonVolatileStatus::BURNINGBULWARK)
            && choice.flags.contact
        {
            choice.status = Some(Status {
                target: MoveTarget::User,
                status: PokemonStatus::BURN,
            })
        } else if defending_side
            .get_active_slot_immutable(def_slot).volatile_statuses
            .contains(&PokemonVolatileStatus::SILKTRAP)
            && choice.flags.contact
        {
            choice.boost = Some(Boost {
                target: MoveTarget::User,
                boosts: StatBoosts {
                    attack: 0,
                    defense: 0,
                    special_attack: 0,
                    special_defense: 0,
                    speed: -1,
                    accuracy: 0,
                },
            })
        }
    }
}

fn generate_instructions_from_existing_status_conditions(
    state: &mut State,
    attacking_side_ref: &SideReference,
    attacker_choice: &Choice,
    incoming_instructions: &mut StateInstructions,
    final_instructions: &mut Vec<StateInstructions>,
) {
    let act_slot = state.actor_slot();
    let _def_pos = state.defender_position(attacking_side_ref);
    let (attacking_side, _defending_side) = state.get_both_sides(attacking_side_ref);
    let current_active_index = attacking_side.active_indices[0];
    let attacker_active = attacking_side.get_active();
    match attacker_active.status {
        PokemonStatus::PARALYZE => {
            // Fully-Paralyzed Branch
            let mut fully_paralyzed_instruction = incoming_instructions.clone();
            fully_paralyzed_instruction.update_percentage(0.25);
            final_instructions.push(fully_paralyzed_instruction);

            // Non-Paralyzed Branch
            incoming_instructions.update_percentage(0.75);
        }
        PokemonStatus::FREEZE => {
            let mut still_frozen_instruction = incoming_instructions.clone();
            still_frozen_instruction.update_percentage(0.80);
            final_instructions.push(still_frozen_instruction);

            incoming_instructions.update_percentage(0.20);
            attacker_active.status = PokemonStatus::NONE;
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeStatus(ChangeStatusInstruction {
                    side_ref: attacking_side_ref.clone(),
                    pokemon_index: current_active_index,
                    old_status: PokemonStatus::FREEZE,
                    new_status: PokemonStatus::NONE,
                }));
        }
        PokemonStatus::SLEEP => {
            match attacker_active.rest_turns {
                // Pokemon is not asleep because of Rest.
                0 => {
                    let current_sleep_turns = attacker_active.sleep_turns;
                    let chance_to_wake = chance_to_wake_up(current_sleep_turns);
                    if chance_to_wake == 1.0 {
                        attacker_active.status = PokemonStatus::NONE;
                        attacker_active.sleep_turns = 0;
                        incoming_instructions
                            .instruction_list
                            .push(Instruction::ChangeStatus(ChangeStatusInstruction {
                                side_ref: *attacking_side_ref,
                                pokemon_index: current_active_index,
                                old_status: PokemonStatus::SLEEP,
                                new_status: PokemonStatus::NONE,
                            }));
                        incoming_instructions
                            .instruction_list
                            .push(Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                                side_ref: *attacking_side_ref,
                                pokemon_index: current_active_index,
                                new_turns: 0,
                                previous_turns: current_sleep_turns,
                            }));
                    } else if chance_to_wake == 0.0 {
                        if attacker_choice.move_id == Choices::SLEEPTALK {
                            // if we are using sleeptalk we want to continue using this move
                            incoming_instructions.instruction_list.push(
                                Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                                    side_ref: *attacking_side_ref,
                                    pokemon_index: current_active_index,
                                    new_turns: current_sleep_turns + 1,
                                    previous_turns: current_sleep_turns,
                                }),
                            );
                        } else {
                            let mut still_asleep_instruction = incoming_instructions.clone();
                            still_asleep_instruction.update_percentage(1.0);
                            still_asleep_instruction.instruction_list.push(
                                Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                                    side_ref: *attacking_side_ref,
                                    pokemon_index: current_active_index,
                                    new_turns: current_sleep_turns + 1,
                                    previous_turns: current_sleep_turns,
                                }),
                            );
                            final_instructions.push(still_asleep_instruction);
                            incoming_instructions.update_percentage(0.0);
                        }
                    } else {
                        // This code deals with the situation where there is a chance to wake up
                        // as well as a chance to stay asleep.
                        // This logic will branch the state and one branch will represent where
                        // nothing happens and the other will represent where something happens
                        // Normally "nothing happens" means you stay asleep and "something happens"
                        // means you wake up. If the move is sleeptalk these are reversed.
                        let do_nothing_percentage;
                        let mut do_nothing_instructions = incoming_instructions.clone();
                        if attacker_choice.move_id == Choices::SLEEPTALK {
                            do_nothing_percentage = chance_to_wake;
                            do_nothing_instructions.instruction_list.push(
                                Instruction::ChangeStatus(ChangeStatusInstruction {
                                    side_ref: *attacking_side_ref,
                                    pokemon_index: current_active_index,
                                    old_status: PokemonStatus::SLEEP,
                                    new_status: PokemonStatus::NONE,
                                }),
                            );
                            do_nothing_instructions.instruction_list.push(
                                Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                                    side_ref: *attacking_side_ref,
                                    pokemon_index: current_active_index,
                                    new_turns: 0,
                                    previous_turns: current_sleep_turns,
                                }),
                            );
                            incoming_instructions.instruction_list.push(
                                Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                                    side_ref: *attacking_side_ref,
                                    pokemon_index: current_active_index,
                                    new_turns: current_sleep_turns + 1,
                                    previous_turns: current_sleep_turns,
                                }),
                            );
                            attacker_active.sleep_turns += 1;
                        } else {
                            do_nothing_percentage = 1.0 - chance_to_wake;
                            do_nothing_instructions.instruction_list.push(
                                Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                                    side_ref: *attacking_side_ref,
                                    pokemon_index: current_active_index,
                                    new_turns: current_sleep_turns + 1,
                                    previous_turns: current_sleep_turns,
                                }),
                            );
                            incoming_instructions
                                .instruction_list
                                .push(Instruction::ChangeStatus(ChangeStatusInstruction {
                                    side_ref: *attacking_side_ref,
                                    pokemon_index: current_active_index,
                                    old_status: PokemonStatus::SLEEP,
                                    new_status: PokemonStatus::NONE,
                                }));
                            incoming_instructions.instruction_list.push(
                                Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                                    side_ref: *attacking_side_ref,
                                    pokemon_index: current_active_index,
                                    new_turns: 0,
                                    previous_turns: current_sleep_turns,
                                }),
                            );
                            attacker_active.status = PokemonStatus::NONE;
                            attacker_active.sleep_turns = 0;
                        }
                        do_nothing_instructions.update_percentage(do_nothing_percentage);
                        incoming_instructions.update_percentage(1.0 - do_nothing_percentage);
                        final_instructions.push(do_nothing_instructions);
                    }
                }
                // Pokemon is asleep because of Rest, and will wake up this turn
                1 => {
                    attacker_active.status = PokemonStatus::NONE;
                    attacker_active.rest_turns -= 1;
                    incoming_instructions
                        .instruction_list
                        .push(Instruction::ChangeStatus(ChangeStatusInstruction {
                            side_ref: *attacking_side_ref,
                            pokemon_index: current_active_index,
                            old_status: PokemonStatus::SLEEP,
                            new_status: PokemonStatus::NONE,
                        }));
                    incoming_instructions
                        .instruction_list
                        .push(Instruction::DecrementRestTurns(
                            DecrementRestTurnsInstruction {
                                side_ref: *attacking_side_ref,
                            },
                        ));
                }
                // Pokemon is asleep because of Rest, and will stay asleep this turn
                2 | 3 => {
                    attacker_active.rest_turns -= 1;
                    incoming_instructions
                        .instruction_list
                        .push(Instruction::DecrementRestTurns(
                            DecrementRestTurnsInstruction {
                                side_ref: *attacking_side_ref,
                            },
                        ));
                }
                _ => panic!("Invalid rest_turns value: {}", attacker_active.rest_turns),
            }
        }
        _ => {}
    }

    if attacking_side
        .get_active().volatile_statuses
        .contains(&PokemonVolatileStatus::CONFUSION)
    {
        let mut hit_yourself_instruction = incoming_instructions.clone();
        hit_yourself_instruction.update_percentage(HIT_SELF_IN_CONFUSION_CHANCE);

        let attacking_stat = attacking_side.calculate_boosted_stat(PokemonBoostableStat::Attack);
        let defending_stat = attacking_side.calculate_boosted_stat(PokemonBoostableStat::Defense);

        let attacker_active = attacking_side.get_active();
        let mut damage_dealt = 2.0 * attacker_active.level as f32;
        damage_dealt = damage_dealt.floor() / 5.0;
        damage_dealt = damage_dealt.floor() + 2.0;
        damage_dealt = damage_dealt.floor() * 40.0; // 40 is the base power of confusion damage
        damage_dealt = damage_dealt * attacking_stat as f32 / defending_stat as f32;
        damage_dealt = damage_dealt.floor() / 50.0;
        damage_dealt = damage_dealt.floor() + 2.0;
        if attacker_active.status == PokemonStatus::BURN {
            damage_dealt /= 2.0;
        }

        let damage_dealt = cmp::min(damage_dealt as i16, attacker_active.hp);
        let damage_instruction = Instruction::Damage(DamageInstruction::new(
            *attacking_side_ref,
            act_slot,
            damage_dealt,
        ));
        hit_yourself_instruction
            .instruction_list
            .push(damage_instruction);

        final_instructions.push(hit_yourself_instruction);

        incoming_instructions.update_percentage(1.0 - HIT_SELF_IN_CONFUSION_CHANCE);
    }

    if attacking_side.side_conditions.protect > 0 {
        if let Some(vs) = &attacker_choice.volatile_status {
            if PROTECT_VOLATILES.contains(&vs.volatile_status) {
                let protect_success_chance =
                    CONSECUTIVE_PROTECT_CHANCE.powi(attacking_side.side_conditions.protect as i32);
                let mut protect_fail_instruction = incoming_instructions.clone();
                protect_fail_instruction.update_percentage(1.0 - protect_success_chance);
                final_instructions.push(protect_fail_instruction);
                incoming_instructions.update_percentage(protect_success_chance);
            }
        }
    }
}

/// Doubles redirection: a single-target move aimed at a foe is pulled onto a
/// partner of the original target that is actively drawing moves. Move-drawing
/// volatiles (Follow Me / Rage Powder / Spotlight) take precedence over the
/// redirection abilities (Lightning Rod for Electric, Storm Drain for Water).
///
/// Returns the (possibly redirected) target position. Only affects single-target
/// foe moves; spread/self/ally/side moves and the few redirection-immune cases
/// (Snipe Shot, Propeller Tail / Stalwart attackers) are returned unchanged.
/// No-op in singles (there is never a second slot to redirect to).
#[cfg(feature = "doubles")]
fn redirect_target(
    state: &State,
    attacking_side: SideReference,
    choice: &Choice,
    nominal: BattlePosition,
) -> BattlePosition {
    // Redirection only applies to moves that single out one foe.
    if choice.target != MoveTarget::Opponent || choice.category == MoveCategory::Switch {
        return nominal;
    }
    let attacker = state
        .get_side_immutable(&attacking_side)
        .get_active_slot_immutable(state.actor_slot());
    // Moves/abilities that ignore redirection entirely.
    if choice.move_id == Choices::SNIPESHOT
        || attacker.ability == Abilities::PROPELLERTAIL
        || attacker.ability == Abilities::STALWART
    {
        return nominal;
    }
    let attacker_is_grass =
        attacker.types.0 == PokemonType::GRASS || attacker.types.1 == PokemonType::GRASS;
    let attacker_ignores_powder = attacker_is_grass || attacker.ability == Abilities::OVERCOAT;

    let target_side_ref = nominal.side;
    let target_side = state.get_side_immutable(&target_side_ref);

    let mut volatile_redirect: Option<BattlePosition> = None;
    let mut ability_redirect: Option<BattlePosition> = None;
    for slot in 0..crate::state::ACTIVE_PER_SIDE as u8 {
        let pkmn = target_side.get_active_slot_immutable(slot);
        if pkmn.hp <= 0 {
            continue;
        }
        let pos = BattlePosition::new(target_side_ref, slot);
        if pkmn
            .volatile_statuses
            .contains(&PokemonVolatileStatus::FOLLOWME)
            || pkmn
                .volatile_statuses
                .contains(&PokemonVolatileStatus::SPOTLIGHT)
        {
            volatile_redirect = Some(pos);
        } else if pkmn
            .volatile_statuses
            .contains(&PokemonVolatileStatus::RAGEPOWDER)
            && !attacker_ignores_powder
        {
            volatile_redirect = Some(pos);
        }
        if (pkmn.ability == Abilities::LIGHTNINGROD && choice.move_type == PokemonType::ELECTRIC)
            || (pkmn.ability == Abilities::STORMDRAIN && choice.move_type == PokemonType::WATER)
        {
            ability_redirect = Some(pos);
        }
    }
    volatile_redirect.or(ability_redirect).unwrap_or(nominal)
}

pub fn generate_instructions_from_move(
    state: &mut State,
    choice: &mut Choice,
    defender_choice: &Choice,
    attacking_side: SideReference,
    mut incoming_instructions: StateInstructions,
    mut final_instructions: &mut Vec<StateInstructions>,
    branch_on_damage: bool,
) {
    if state.use_damage_dealt {
        let acting_slot = state.actor_slot();
        reset_damage_dealt(
            state.get_side(&attacking_side).get_active_slot(acting_slot),
            &attacking_side,
            acting_slot,
            &mut incoming_instructions,
        );
    }

    if choice.category == MoveCategory::Switch {
        generate_instructions_from_switch(
            state,
            choice.switch_id,
            attacking_side,
            &mut incoming_instructions,
        );
        final_instructions.push(incoming_instructions);
        return;
    }

    let acting_slot = state.actor_slot();

    let attacker_side = state.get_side(&attacking_side);

    if choice.move_id == Choices::NONE {
        if attacker_side
            .get_active_slot(acting_slot).volatile_statuses
            .contains(&PokemonVolatileStatus::MUSTRECHARGE)
        {
            incoming_instructions
                .instruction_list
                .push(Instruction::RemoveVolatileStatus(
                    RemoveVolatileStatusInstruction::new(
                        attacking_side,
                        acting_slot,
                        PokemonVolatileStatus::MUSTRECHARGE,
                    ),
                ));
        }
        final_instructions.push(incoming_instructions);
        return;
    }

    if attacker_side
        .get_active_slot(acting_slot).volatile_statuses
        .contains(&PokemonVolatileStatus::TRUANT)
    {
        incoming_instructions
            .instruction_list
            .push(Instruction::RemoveVolatileStatus(
                RemoveVolatileStatusInstruction::new(
                    attacking_side,
                    acting_slot,
                    PokemonVolatileStatus::TRUANT,
                ),
            ));
        final_instructions.push(incoming_instructions);
        return;
    }

    // TODO: test first-turn dragontail missing - it should not trigger this early return
    if !choice.first_move && defender_choice.flags.drag {
        final_instructions.push(incoming_instructions);
        return;
    }

    state.apply_instructions(&incoming_instructions.instruction_list);

    let side = state.get_side(&attacking_side);
    if side
        .get_active_slot(acting_slot).volatile_statuses
        .contains(&PokemonVolatileStatus::ENCORE)
    {
        match side.get_active_slot(acting_slot).last_used_move {
            LastUsedMove::Move(last_used_move) => {
                if choice.move_index != last_used_move {
                    *choice = MOVES
                        .get(&side.get_active_slot_immutable(acting_slot).moves[&last_used_move].id)
                        .unwrap()
                        .clone();
                    choice.move_index = last_used_move;
                }
            }
            _ => panic!("Encore should not be active when last used move is not a move"),
        }

        // this value is incremented when an encored move has been used
        // the value being 2 means we are currently using the 3rd move so we can remove it
        #[cfg(any(
            feature = "gen5",
            feature = "gen6",
            feature = "gen7",
            feature = "gen8",
            feature = "gen9"
        ))]
        if side.get_active_slot(acting_slot).volatile_status_durations.encore == 2 {
            incoming_instructions
                .instruction_list
                .push(Instruction::RemoveVolatileStatus(
                    RemoveVolatileStatusInstruction::new(
                        attacking_side,
                        acting_slot,
                        PokemonVolatileStatus::ENCORE,
                    ),
                ));
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeVolatileStatusDuration(
                    ChangeVolatileStatusDurationInstruction::new(
                        attacking_side,
                        acting_slot,
                        PokemonVolatileStatus::ENCORE,
                        -2,
                    ),
                ));
            side.get_active_slot(acting_slot).volatile_status_durations.encore = 0;
            side.get_active_slot(acting_slot).volatile_statuses
                .remove(&PokemonVolatileStatus::ENCORE);
        } else {
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeVolatileStatusDuration(
                    ChangeVolatileStatusDurationInstruction::new(
                        attacking_side,
                        acting_slot,
                        PokemonVolatileStatus::ENCORE,
                        1,
                    ),
                ));
            side.get_active_slot(acting_slot).volatile_status_durations.encore += 1;
        }
    }

    #[cfg(any(
        feature = "gen5",
        feature = "gen6",
        feature = "gen7",
        feature = "gen8",
        feature = "gen9"
    ))]
    if side
        .get_active_slot(acting_slot).volatile_statuses
        .contains(&PokemonVolatileStatus::TAUNT)
    {
        match side.get_active_slot(acting_slot).volatile_status_durations.taunt {
            0 | 1 => {
                incoming_instructions.instruction_list.push(
                    Instruction::ChangeVolatileStatusDuration(
                        ChangeVolatileStatusDurationInstruction::new(
                            attacking_side,
                            acting_slot,
                            PokemonVolatileStatus::TAUNT,
                            1,
                        ),
                    ),
                );
                side.get_active_slot(acting_slot).volatile_status_durations.taunt += 1;
            }

            // Technically taunt is removed at the end of the turn but because we are already
            // dealing with taunt here we can save a check at the end of the turn
            // This shouldn't change anything because taunt only affects which move is selected
            // and by this point a move has been chosen
            2 => {
                side.get_active_slot(acting_slot).volatile_statuses.remove(&PokemonVolatileStatus::TAUNT);
                incoming_instructions
                    .instruction_list
                    .push(Instruction::RemoveVolatileStatus(
                        RemoveVolatileStatusInstruction::new(
                            attacking_side,
                            acting_slot,
                            PokemonVolatileStatus::TAUNT,
                        ),
                    ));
                incoming_instructions.instruction_list.push(
                    Instruction::ChangeVolatileStatusDuration(
                        ChangeVolatileStatusDurationInstruction::new(
                            attacking_side,
                            acting_slot,
                            PokemonVolatileStatus::TAUNT,
                            -2,
                        ),
                    ),
                );
                side.get_active_slot(acting_slot).volatile_status_durations.taunt = 0;
                state.re_enable_disabled_moves(
                    &attacking_side,
                    &mut incoming_instructions.instruction_list,
                );
            }
            _ => panic!(
                "Taunt duration cannot be {} when taunt volatile is active",
                side.get_active_slot(acting_slot).volatile_status_durations.taunt
            ),
        }
    }

    if !choice.first_move
        && state
            .get_side(&attacking_side.get_other_side())
            .any_force_switch()
    {
        state
            .get_side(&attacking_side)
            .switch_out_move_second_saved_move = choice.move_id;
        state.reverse_instructions(&incoming_instructions.instruction_list);
        final_instructions.push(incoming_instructions);
        return;
    }

    if state
        .get_side_immutable(&attacking_side)
        .get_active_slot_immutable(acting_slot)
        .hp
        == 0
    {
        state.reverse_instructions(&incoming_instructions.instruction_list);
        final_instructions.push(incoming_instructions);
        return;
    }

    // If the move is a charge move, remove the volatile status if damage was done
    if choice.flags.charge {
        let side = state.get_side(&attacking_side);
        let volatile_status = charge_choice_to_volatile(&choice.move_id);
        if side.get_active_slot(acting_slot).volatile_statuses.contains(&volatile_status) {
            choice.flags.charge = false;
            let instruction = Instruction::RemoveVolatileStatus(
                RemoveVolatileStatusInstruction::new(
                    attacking_side,
                    acting_slot,
                    volatile_status,
                ),
            );
            incoming_instructions.instruction_list.push(instruction);
            side.get_active_slot(acting_slot).volatile_statuses.remove(&volatile_status);
        }
    }

    // Doubles redirection: pull a single-target foe move onto a partner that is
    // drawing moves (Follow Me / Rage Powder / Spotlight) or has a redirection
    // ability (Lightning Rod / Storm Drain). Done before targeting-dependent logic
    // (cannot_use_move's fainted-target check, damage calc, ability immunity) so a
    // Lightning Rod holder the move is redirected onto applies its own
    // immunity+boost via the existing defender-ability path.
    #[cfg(feature = "doubles")]
    {
        state.target_position =
            redirect_target(state, attacking_side, choice, state.target_position);
    }

    before_move(
        state,
        choice,
        defender_choice,
        &attacking_side,
        &mut incoming_instructions,
    );
    if incoming_instructions.percentage == 0.0 {
        state.reverse_instructions(&incoming_instructions.instruction_list);
        return;
    }

    if state.use_last_used_move {
        set_last_used_move_as_move(
            state.get_side(&attacking_side),
            choice.move_index,
            attacking_side,
            acting_slot,
            &mut incoming_instructions,
        );
    }

    if cannot_use_move(state, &choice, &attacking_side) {
        state.reverse_instructions(&incoming_instructions.instruction_list);
        final_instructions.push(incoming_instructions);
        return;
    }

    // most of the time pp decrement doesn't matter and just adds another instruction
    // so we only decrement pp if the move is at 10 or less pp since that is when it starts
    // to matter
    let def_pos = state.defender_position(&attacking_side);
    let defender_has_pressure = state
        .get_side_immutable(&def_pos.side)
        .get_active_slot_immutable(def_pos.slot)
        .ability
        == Abilities::PRESSURE;
    let active = state.get_side(&attacking_side).get_active_slot(acting_slot);
    if active.moves[&choice.move_index].pp < 10 {
        let pp_decrement_amount = if choice.target.targets_opponent_side() && defender_has_pressure {
            2
        } else {
            1
        };
        incoming_instructions
            .instruction_list
            .push(Instruction::DecrementPP(DecrementPPInstruction::new(
                attacking_side,
                acting_slot,
                choice.move_index,
                pp_decrement_amount,
            )));
        active.moves[&choice.move_index].pp -= pp_decrement_amount;
    }

    if !choice.sleep_talk_move {
        generate_instructions_from_existing_status_conditions(
            state,
            &attacking_side,
            &choice,
            &mut incoming_instructions,
            &mut final_instructions,
        );
    }
    let attacker = state
        .get_side_immutable(&attacking_side)
        .get_active_immutable();
    if choice.move_id == Choices::SLEEPTALK && attacker.status == PokemonStatus::SLEEP {
        let new_choices = attacker.get_sleep_talk_choices();
        state.reverse_instructions(&incoming_instructions.instruction_list);
        let num_choices = new_choices.len() as f32;
        for mut new_choice in new_choices {
            new_choice.sleep_talk_move = true;
            let mut sleep_talk_instructions = incoming_instructions.clone();
            sleep_talk_instructions.update_percentage(1.0 / num_choices);
            generate_instructions_from_move(
                state,
                &mut new_choice,
                defender_choice,
                attacking_side,
                sleep_talk_instructions,
                &mut final_instructions,
                false,
            );
        }
        return;
    } else if attacker.status == PokemonStatus::SLEEP && !choice.sleep_talk_move {
        state.reverse_instructions(&incoming_instructions.instruction_list);
        if incoming_instructions.percentage > 0.0 {
            final_instructions.push(incoming_instructions);
        }
        return;
    }

    if move_has_no_effect(state, &choice, &attacking_side) {
        state.reverse_instructions(&incoming_instructions.instruction_list);
        final_instructions.push(incoming_instructions);
        return;
    }
    choice_special_effect(state, choice, &attacking_side, &mut incoming_instructions);
    let damage = calculate_damage(state, &attacking_side, &choice, DamageRolls::Max);
    check_move_hit_or_miss(
        state,
        &choice,
        &attacking_side,
        damage,
        &mut incoming_instructions,
        &mut final_instructions,
    );

    if incoming_instructions.percentage == 0.0 {
        state.reverse_instructions(&incoming_instructions.instruction_list);
        return;
    }

    // start multi-hit
    let hit_count;
    match choice.multi_hit() {
        MultiHitMove::None => {
            hit_count = 1;
        }
        MultiHitMove::DoubleHit => {
            hit_count = 2;
        }
        MultiHitMove::TripleHit => {
            hit_count = 3;
        }
        MultiHitMove::TwoToFiveHits => {
            hit_count =
                if state.get_side(&attacking_side).get_active().ability == Abilities::SKILLLINK {
                    5
                } else if state.get_side(&attacking_side).get_active().item == Items::LOADEDDICE {
                    4
                } else {
                    3 // too lazy to implement branching here. Average is 3.2 so this is a fine approximation
                };
        }
        MultiHitMove::PopulationBomb => {
            // population bomb checks accuracy each time but lets approximate
            hit_count = if state.get_side(&attacking_side).get_active().item == Items::WIDELENS {
                9
            } else {
                6
            };
        }
        MultiHitMove::TripleAxel => {
            // triple axel checks accuracy each time but until multi-accuracy is implemented this
            // is the best we can do
            hit_count = 3
        }
    }

    // Doubles spread moves (Earthquake, Rock Slide, …) hit every living adjacent
    // target. Resolve the damage against each target in turn (reusing the normal
    // per-target damage path, which already applies the 0.75x spread reduction in
    // `calculate_damage`), then run the move's non-damage effects once. Kill/crit
    // branching is intentionally skipped for the multi-target case (the engine
    // already approximates elsewhere); a fainted/immune target simply takes 0.
    #[cfg(feature = "doubles")]
    if choice.target.hits_multiple_targets() {
        let targets = state.spread_target_positions(attacking_side, choice.target);
        if targets.len() > 1 {
            for target in targets {
                state.target_position = target;
                let target_damage = match calculate_damage(
                    state,
                    &attacking_side,
                    &choice,
                    DamageRolls::Max,
                ) {
                    Some((max_damage_dealt, _)) => (max_damage_dealt as f32 * 0.925) as i16,
                    None => 0,
                };
                generate_instructions_from_damage(
                    state,
                    choice,
                    target_damage,
                    &attacking_side,
                    &mut incoming_instructions,
                );
            }
            // Non-damage effects (self boosts, side conditions, pivot, the move's
            // secondaries) run once; `does_damage = false` so run_move skips its
            // own single-target damage step.
            run_move(
                state,
                attacking_side,
                incoming_instructions,
                hit_count,
                false,
                0,
                choice,
                defender_choice,
                &mut final_instructions,
            );
            combine_duplicate_instructions(&mut final_instructions);
            return;
        } else if let Some(only_target) = targets.first() {
            // Exactly one living target: hit it as a normal single-target move
            // (no spread reduction). Zero living targets: the nominal target stays
            // and resolves to 0 damage.
            state.target_position = *only_target;
        }
    }

    // Kill/crit branching reads the move's current target (the chosen foe in
    // doubles), not blindly the opposing slot 0.
    let branch_def_pos = state.defender_position(&attacking_side);
    let defender_active = state
        .get_side_immutable(&branch_def_pos.side)
        .get_active_slot_immutable(branch_def_pos.slot);
    let mut does_damage = false;
    let (mut branch_damage, mut regular_damage) = (0, 0);
    let mut branch_instructions: Option<StateInstructions> = None;
    if let Some((max_damage_dealt, max_crit_damage)) = damage {
        does_damage = true;
        let avg_damage_dealt = (max_damage_dealt as f32 * 0.925) as i16;
        let min_damage_dealt = (max_damage_dealt as f32 * 0.85) as i16;
        if branch_on_damage
            && max_damage_dealt >= defender_active.hp
            && min_damage_dealt < defender_active.hp
        {
            let (average_non_kill_damage, num_kill_rolls) =
                compare_health_with_damage_multiples(max_damage_dealt, defender_active.hp);

            let crit_rate = if defender_active.ability == Abilities::BATTLEARMOR
                || defender_active.ability == Abilities::SHELLARMOR
            {
                0.0
            } else if choice.move_id.guaranteed_crit() {
                1.0
            } else if choice.move_id.increased_crit_ratio() {
                1.0 / 8.0
            } else {
                BASE_CRIT_CHANCE
            };

            // the chance of a branch is the chance of the roll killing + the chance of a crit
            let branch_chance = ((1.0 - crit_rate) * (num_kill_rolls as f32 / 16.0)) + crit_rate;

            let mut branch_ins = incoming_instructions.clone();
            branch_ins.update_percentage(branch_chance);
            branch_instructions = Some(branch_ins);
            branch_damage = defender_active.hp;

            incoming_instructions.update_percentage(1.0 - branch_chance);
            regular_damage = average_non_kill_damage;
        } else if branch_on_damage && max_damage_dealt < defender_active.hp {
            let crit_rate = if defender_active.ability == Abilities::BATTLEARMOR
                || defender_active.ability == Abilities::SHELLARMOR
            {
                0.0
            } else if choice.move_id.guaranteed_crit() {
                1.0
            } else if choice.move_id.increased_crit_ratio() {
                1.0 / 8.0
            } else {
                BASE_CRIT_CHANCE
            };
            let mut branch_ins = incoming_instructions.clone();
            branch_ins.update_percentage(crit_rate);
            branch_instructions = Some(branch_ins);
            branch_damage = (max_crit_damage as f32 * 0.925) as i16;
            incoming_instructions.update_percentage(1.0 - crit_rate);
            regular_damage = (max_damage_dealt as f32 * 0.925) as i16;
        } else {
            regular_damage = avg_damage_dealt;
        }
    }

    if incoming_instructions.percentage != 0.0 {
        run_move(
            state,
            attacking_side,
            incoming_instructions,
            hit_count,
            does_damage,
            regular_damage,
            choice,
            defender_choice,
            &mut final_instructions,
        );
    } else {
        state.reverse_instructions(&incoming_instructions.instruction_list);
    }

    // A branch representing either a roll that kills the opponent or a crit
    if let Some(branch_ins) = branch_instructions {
        if branch_ins.percentage != 0.0 {
            state.apply_instructions(&branch_ins.instruction_list);
            run_move(
                state,
                attacking_side,
                branch_ins,
                hit_count,
                does_damage,
                branch_damage,
                choice,
                defender_choice,
                &mut final_instructions,
            );
        }
    }

    combine_duplicate_instructions(&mut final_instructions);
    return;
}

fn combine_duplicate_instructions(list_of_instructions: &mut Vec<StateInstructions>) {
    for i in 0..list_of_instructions.len() {
        let mut j = i + 1;
        while j < list_of_instructions.len() {
            if list_of_instructions[i].instruction_list == list_of_instructions[j].instruction_list
            {
                list_of_instructions[i].percentage += list_of_instructions[j].percentage;
                list_of_instructions.remove(j);
            } else {
                j += 1;
            }
        }
    }
}

// Used by the singles orderer (`moves_first`) and by unit tests; under a
// doubles build the non-test binary uses `get_effective_speed_slot` instead.
#[cfg_attr(feature = "doubles", allow(dead_code))]
fn get_effective_speed(state: &State, side_reference: &SideReference) -> i16 {
    let side = state.get_side_immutable(side_reference);
    let active_pkmn = side.get_active_immutable();

    let mut boosted_speed = side.calculate_boosted_stat(PokemonBoostableStat::Speed) as f32;

    match state.weather.weather_type {
        Weather::SUN | Weather::HARSHSUN if active_pkmn.ability == Abilities::CHLOROPHYLL => {
            boosted_speed *= 2.0
        }
        Weather::RAIN | Weather::HEAVYRAIN if active_pkmn.ability == Abilities::SWIFTSWIM => {
            boosted_speed *= 2.0
        }
        Weather::SAND if active_pkmn.ability == Abilities::SANDRUSH => boosted_speed *= 2.0,
        Weather::HAIL if active_pkmn.ability == Abilities::SLUSHRUSH => boosted_speed *= 2.0,
        _ => {}
    }

    match active_pkmn.ability {
        Abilities::SURGESURFER if state.terrain.terrain_type == Terrain::ELECTRICTERRAIN => {
            boosted_speed *= 2.0
        }
        Abilities::UNBURDEN
            if side
                .get_active_immutable().volatile_statuses
                .contains(&PokemonVolatileStatus::UNBURDEN) =>
        {
            boosted_speed *= 2.0
        }
        Abilities::QUICKFEET if active_pkmn.status != PokemonStatus::NONE => boosted_speed *= 1.5,
        _ => {}
    }

    if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::SLOWSTART)
    {
        boosted_speed *= 0.5;
    }

    if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::PROTOSYNTHESISSPE)
        || side
            .get_active_immutable().volatile_statuses
            .contains(&PokemonVolatileStatus::QUARKDRIVESPE)
    {
        boosted_speed *= 1.5;
    }

    if side.side_conditions.tailwind > 0 {
        boosted_speed *= 2.0
    }

    match active_pkmn.item {
        Items::IRONBALL => boosted_speed *= 0.5,
        Items::CHOICESCARF => boosted_speed *= 1.5,
        _ => {}
    }

    #[cfg(any(feature = "gen3", feature = "gen4", feature = "gen5", feature = "gen6"))]
    if active_pkmn.status == PokemonStatus::PARALYZE && active_pkmn.ability != Abilities::QUICKFEET
    {
        boosted_speed *= 0.25;
    }

    #[cfg(any(feature = "gen7", feature = "gen8", feature = "gen9"))]
    if active_pkmn.status == PokemonStatus::PARALYZE && active_pkmn.ability != Abilities::QUICKFEET
    {
        boosted_speed *= 0.50;
    }

    boosted_speed as i16
}

fn modify_choice_priority(state: &State, side_reference: &SideReference, choice: &mut Choice) {
    let side = state.get_side_immutable(side_reference);
    let active_pkmn = side.get_active_immutable();

    if choice.move_id == Choices::GRASSYGLIDE && state.terrain_is_active(&Terrain::GRASSYTERRAIN) {
        choice.priority += 1;
    }

    match active_pkmn.ability {
        Abilities::PRANKSTER if choice.category == MoveCategory::Status => choice.priority += 1,
        Abilities::GALEWINGS
            if choice.move_type == PokemonType::FLYING && active_pkmn.hp == active_pkmn.maxhp =>
        {
            choice.priority += 1
        }
        Abilities::TRIAGE if choice.flags.heal => choice.priority += 3,
        _ => {}
    }
}

// The singles orderer. Still compiled in a doubles build (its unit tests run
// there and `compare_actors` mirrors it), but unused by the doubles turn core.
#[cfg_attr(feature = "doubles", allow(dead_code))]
fn moves_first(
    state: &State,
    side_one_choice: &Choice,
    side_two_choice: &Choice,
    incoming_instructions: &mut StateInstructions,
) -> SideMovesFirst {
    let side_one_effective_speed = get_effective_speed(&state, &SideReference::SideOne);
    let side_two_effective_speed = get_effective_speed(&state, &SideReference::SideTwo);

    if side_one_choice.category == MoveCategory::Switch
        && side_two_choice.category == MoveCategory::Switch
    {
        return if side_one_effective_speed > side_two_effective_speed {
            SideMovesFirst::SideOne
        } else if side_one_effective_speed == side_two_effective_speed {
            SideMovesFirst::SpeedTie
        } else {
            SideMovesFirst::SideTwo
        };
    } else if side_one_choice.category == MoveCategory::Switch {
        return if side_two_choice.move_id != Choices::PURSUIT {
            SideMovesFirst::SideOne
        } else {
            SideMovesFirst::SideTwo
        };
    } else if side_two_choice.category == MoveCategory::Switch {
        return if side_one_choice.move_id == Choices::PURSUIT {
            SideMovesFirst::SideOne
        } else {
            SideMovesFirst::SideTwo
        };
    }

    let side_one_active = state.side_one.get_active_immutable();
    let side_two_active = state.side_two.get_active_immutable();
    if side_one_choice.priority == side_two_choice.priority {
        if side_one_active.item == Items::CUSTAPBERRY
            && side_one_active.hp < side_one_active.maxhp / 4
        {
            // FIXME(doubles): slot
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeItem(ChangeItemInstruction::new(
                    SideReference::SideOne,
                    0,
                    Items::CUSTAPBERRY,
                    Items::NONE,
                )));
            return SideMovesFirst::SideOne;
        } else if side_two_active.item == Items::CUSTAPBERRY
            && side_two_active.hp < side_two_active.maxhp / 4
        {
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeItem(ChangeItemInstruction::new(
                    SideReference::SideTwo,
                    0,
                    Items::CUSTAPBERRY,
                    Items::NONE,
                )));
            return SideMovesFirst::SideTwo;
        }

        if side_one_effective_speed == side_two_effective_speed {
            return SideMovesFirst::SpeedTie;
        }

        match state.trick_room.active {
            true => {
                if side_one_effective_speed < side_two_effective_speed {
                    SideMovesFirst::SideOne
                } else {
                    SideMovesFirst::SideTwo
                }
            }
            false => {
                if side_one_effective_speed > side_two_effective_speed {
                    SideMovesFirst::SideOne
                } else {
                    SideMovesFirst::SideTwo
                }
            }
        }
    } else {
        if side_one_choice.priority > side_two_choice.priority {
            SideMovesFirst::SideOne
        } else {
            SideMovesFirst::SideTwo
        }
    }
}

fn get_active_protosynthesis(side: &Side) -> Option<PokemonVolatileStatus> {
    if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::PROTOSYNTHESISATK)
    {
        Some(PokemonVolatileStatus::PROTOSYNTHESISATK)
    } else if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::PROTOSYNTHESISDEF)
    {
        Some(PokemonVolatileStatus::PROTOSYNTHESISDEF)
    } else if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::PROTOSYNTHESISSPA)
    {
        Some(PokemonVolatileStatus::PROTOSYNTHESISSPA)
    } else if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::PROTOSYNTHESISSPD)
    {
        Some(PokemonVolatileStatus::PROTOSYNTHESISSPD)
    } else if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::PROTOSYNTHESISSPE)
    {
        Some(PokemonVolatileStatus::PROTOSYNTHESISSPE)
    } else {
        None
    }
}

fn get_active_quarkdrive(side: &Side) -> Option<PokemonVolatileStatus> {
    if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::QUARKDRIVEATK)
    {
        Some(PokemonVolatileStatus::QUARKDRIVEATK)
    } else if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::QUARKDRIVEDEF)
    {
        Some(PokemonVolatileStatus::QUARKDRIVEDEF)
    } else if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::QUARKDRIVESPA)
    {
        Some(PokemonVolatileStatus::QUARKDRIVESPA)
    } else if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::QUARKDRIVESPD)
    {
        Some(PokemonVolatileStatus::QUARKDRIVESPD)
    } else if side
        .get_active_immutable().volatile_statuses
        .contains(&PokemonVolatileStatus::QUARKDRIVESPE)
    {
        Some(PokemonVolatileStatus::QUARKDRIVESPE)
    } else {
        None
    }
}

fn on_weather_end(
    state: &mut State,
    sides: [&SideReference; 2],
    incoming_instructions: &mut StateInstructions,
) {
    match state.weather.weather_type {
        Weather::SUN => {
            for side_ref in sides {
                let side = state.get_side(side_ref);
                if side.get_active_immutable().ability == Abilities::PROTOSYNTHESIS {
                    if let Some(volatile_status) = get_active_protosynthesis(side) {
                        let active = side.get_active();
                        if active.item == Items::BOOSTERENERGY {
                            // FIXME(doubles): slot
                            incoming_instructions
                                .instruction_list
                                .push(Instruction::ChangeItem(ChangeItemInstruction::new(
                                    *side_ref,
                                    0,
                                    Items::BOOSTERENERGY,
                                    Items::NONE,
                                )));
                            active.item = Items::NONE;
                        } else {
                            incoming_instructions.instruction_list.push(
                                Instruction::RemoveVolatileStatus(
                                    RemoveVolatileStatusInstruction::new(
                                        *side_ref,
                                        0,
                                        volatile_status,
                                    ),
                                ),
                            );
                            side.get_active().volatile_statuses.remove(&volatile_status);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn on_terrain_end(
    state: &mut State,
    sides: [&SideReference; 2],
    incoming_instructions: &mut StateInstructions,
) {
    match state.terrain.terrain_type {
        Terrain::ELECTRICTERRAIN => {
            for side_ref in sides {
                let side = state.get_side(side_ref);
                if side.get_active_immutable().ability == Abilities::QUARKDRIVE {
                    if let Some(volatile_status) = get_active_quarkdrive(side) {
                        let active = side.get_active();
                        if active.item == Items::BOOSTERENERGY {
                            // FIXME(doubles): slot
                            incoming_instructions
                                .instruction_list
                                .push(Instruction::ChangeItem(ChangeItemInstruction::new(
                                    *side_ref,
                                    0,
                                    Items::BOOSTERENERGY,
                                    Items::NONE,
                                )));
                            active.item = Items::NONE;
                        } else {
                            incoming_instructions.instruction_list.push(
                                Instruction::RemoveVolatileStatus(
                                    RemoveVolatileStatusInstruction::new(
                                        *side_ref,
                                        0,
                                        volatile_status,
                                    ),
                                ),
                            );
                            side.get_active().volatile_statuses.remove(&volatile_status);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn add_end_of_turn_instructions(
    state: &mut State,
    mut incoming_instructions: &mut StateInstructions,
    first_move_side: &SideReference,
) {
    if state.side_one.any_force_switch() || state.side_two.any_force_switch() {
        return;
    }

    let sides = [first_move_side, &first_move_side.get_other_side()];

    // Doubles: clear the single-turn move-drawing / damage-boost volatiles
    // (Follow Me, Rage Powder, Spotlight, Helping Hand) on every living slot so
    // they don't carry into the next turn. These are never set in singles, so
    // this whole block is doubles-only and a no-op for singles.
    #[cfg(feature = "doubles")]
    for side_ref in sides {
        for slot in 0..crate::state::ACTIVE_PER_SIDE as u8 {
            for vs in [
                PokemonVolatileStatus::FOLLOWME,
                PokemonVolatileStatus::RAGEPOWDER,
                PokemonVolatileStatus::SPOTLIGHT,
                PokemonVolatileStatus::HELPINGHAND,
            ] {
                if state
                    .get_side(side_ref)
                    .get_active_slot(slot)
                    .volatile_statuses
                    .remove(&vs)
                {
                    incoming_instructions.instruction_list.push(
                        Instruction::RemoveVolatileStatus(RemoveVolatileStatusInstruction::new(
                            *side_ref, slot, vs,
                        )),
                    );
                }
            }
        }
    }

    // Weather decrement / dissipation
    if state.weather.turns_remaining > 0 && state.weather.weather_type != Weather::NONE {
        let weather_dissipate_instruction = Instruction::DecrementWeatherTurnsRemaining;
        incoming_instructions
            .instruction_list
            .push(weather_dissipate_instruction);
        state.weather.turns_remaining -= 1;
        if state.weather.turns_remaining == 0 {
            on_weather_end(state, sides, &mut incoming_instructions);
            let weather_end_instruction = Instruction::ChangeWeather(ChangeWeather {
                new_weather: Weather::NONE,
                new_weather_turns_remaining: 0,
                previous_weather: state.weather.weather_type,
                previous_weather_turns_remaining: 0,
            });
            incoming_instructions
                .instruction_list
                .push(weather_end_instruction);
            state.weather.weather_type = Weather::NONE;
        }
    }

    // Trick Room decrement / dissipation
    if state.trick_room.turns_remaining > 0 && state.trick_room.active {
        incoming_instructions
            .instruction_list
            .push(Instruction::DecrementTrickRoomTurnsRemaining);
        state.trick_room.turns_remaining -= 1;
        if state.trick_room.turns_remaining == 0 {
            incoming_instructions
                .instruction_list
                .push(Instruction::ToggleTrickRoom(ToggleTrickRoomInstruction {
                    currently_active: true,
                    new_trickroom_turns_remaining: 0,
                    previous_trickroom_turns_remaining: 0,
                }));
            state.trick_room.active = false;
        }
    }

    // Terrain decrement / dissipation
    if state.terrain.turns_remaining > 0 && state.terrain.terrain_type != Terrain::NONE {
        if state.terrain.terrain_type == Terrain::GRASSYTERRAIN {
            for side_ref in sides {
                let side = state.get_side(side_ref);
                let active_pkmn = side.get_active();
                if active_pkmn.hp == 0 || !active_pkmn.is_grounded() {
                    continue;
                }
                let heal_amount = cmp::min(
                    (active_pkmn.maxhp as f32 * 0.0625) as i16,
                    active_pkmn.maxhp - active_pkmn.hp,
                );
                if heal_amount > 0 {
                    // FIXME(doubles): slot
                    let heal_instruction = Instruction::Heal(HealInstruction::new(
                        *side_ref,
                        0,
                        heal_amount,
                    ));
                    active_pkmn.hp += heal_amount;
                    incoming_instructions
                        .instruction_list
                        .push(heal_instruction);
                }
            }
        }
        let terrain_dissipate_instruction = Instruction::DecrementTerrainTurnsRemaining;
        incoming_instructions
            .instruction_list
            .push(terrain_dissipate_instruction);
        state.terrain.turns_remaining -= 1;
        if state.terrain.turns_remaining == 0 {
            on_terrain_end(state, sides, &mut incoming_instructions);
            let terrain_end_instruction = Instruction::ChangeTerrain(ChangeTerrain {
                new_terrain: Terrain::NONE,
                new_terrain_turns_remaining: 0,
                previous_terrain: state.terrain.terrain_type,
                previous_terrain_turns_remaining: 0,
            });
            incoming_instructions
                .instruction_list
                .push(terrain_end_instruction);
            state.terrain.terrain_type = Terrain::NONE;
        }
    }

    // Side Condition decrement
    for side_ref in sides {
        let side = state.get_side(side_ref);
        if side.side_conditions.reflect > 0 {
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeSideCondition(
                    ChangeSideConditionInstruction {
                        side_ref: *side_ref,
                        side_condition: PokemonSideCondition::Reflect,
                        amount: -1,
                    },
                ));
            side.side_conditions.reflect -= 1;
        }
        if side.side_conditions.light_screen > 0 {
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeSideCondition(
                    ChangeSideConditionInstruction {
                        side_ref: *side_ref,
                        side_condition: PokemonSideCondition::LightScreen,
                        amount: -1,
                    },
                ));
            side.side_conditions.light_screen -= 1;
        }
        if side.side_conditions.aurora_veil > 0 {
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeSideCondition(
                    ChangeSideConditionInstruction {
                        side_ref: *side_ref,
                        side_condition: PokemonSideCondition::AuroraVeil,
                        amount: -1,
                    },
                ));
            side.side_conditions.aurora_veil -= 1;
        }
        if side.side_conditions.tailwind > 0 {
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeSideCondition(
                    ChangeSideConditionInstruction {
                        side_ref: *side_ref,
                        side_condition: PokemonSideCondition::Tailwind,
                        amount: -1,
                    },
                ));
            side.side_conditions.tailwind -= 1;
        }
    }

    // Weather Damage
    for side_ref in sides {
        if state.weather_is_active(&Weather::HAIL) {
            let active_pkmn = state.get_side(side_ref).get_active();
            if active_pkmn.hp == 0
                || active_pkmn.ability == Abilities::MAGICGUARD
                || active_pkmn.ability == Abilities::OVERCOAT
                || active_pkmn.ability == Abilities::ICEBODY
                || active_pkmn.has_type(&PokemonType::ICE)
            {
                continue;
            }

            let damage_amount =
                cmp::min((active_pkmn.maxhp as f32 * 0.0625) as i16, active_pkmn.hp);
            let hail_damage_instruction = Instruction::Damage(DamageInstruction::new(
                *side_ref,
                0,
                damage_amount,
            ));

            active_pkmn.hp -= damage_amount;
            incoming_instructions
                .instruction_list
                .push(hail_damage_instruction);
        } else if state.weather_is_active(&Weather::SAND) {
            let active_pkmn = state.get_side(side_ref).get_active();
            if active_pkmn.hp == 0
                || active_pkmn.ability == Abilities::MAGICGUARD
                || active_pkmn.ability == Abilities::OVERCOAT
                || active_pkmn.has_type(&PokemonType::GROUND)
                || active_pkmn.has_type(&PokemonType::STEEL)
                || active_pkmn.has_type(&PokemonType::ROCK)
            {
                continue;
            }
            let damage_amount =
                cmp::min((active_pkmn.maxhp as f32 * 0.0625) as i16, active_pkmn.hp);
            let sand_damage_instruction = Instruction::Damage(DamageInstruction::new(
                *side_ref,
                0,
                damage_amount,
            ));
            active_pkmn.hp -= damage_amount;
            incoming_instructions
                .instruction_list
                .push(sand_damage_instruction);
        }
    }

    // future sight
    for side_ref in sides {
        let (attacking_side, defending_side) = state.get_both_sides(side_ref);
        if attacking_side.future_sight.0 > 0 {
            let decrement_future_sight_instruction =
                Instruction::DecrementFutureSight(DecrementFutureSightInstruction {
                    side_ref: *side_ref,
                });
            if attacking_side.future_sight.0 == 1 {
                let mut damage = calculate_futuresight_damage(
                    &attacking_side,
                    &defending_side,
                    &attacking_side.future_sight.1,
                );
                let defender = defending_side.get_active();
                damage = cmp::min(damage, defender.hp);
                let future_sight_damage_instruction = Instruction::Damage(DamageInstruction::new(
                    side_ref.get_other_side(),
                    0,
                    damage,
                ));
                incoming_instructions
                    .instruction_list
                    .push(future_sight_damage_instruction);
                defender.hp -= damage;
            }
            attacking_side.future_sight.0 -= 1;
            incoming_instructions
                .instruction_list
                .push(decrement_future_sight_instruction);
        }
    }

    // wish
    for side_ref in sides {
        let side = state.get_side(side_ref);
        let side_wish = side.wish;
        let active_pkmn = side.get_active();

        if side_wish.0 > 0 {
            let decrement_wish_instruction = Instruction::DecrementWish(DecrementWishInstruction {
                side_ref: *side_ref,
            });
            if side_wish.0 == 1 && 0 < active_pkmn.hp && active_pkmn.hp < active_pkmn.maxhp {
                #[cfg(not(feature = "gen4"))]
                let heal_amount = cmp::min(active_pkmn.maxhp - active_pkmn.hp, side_wish.1);

                #[cfg(feature = "gen4")]
                let heal_amount =
                    cmp::min(active_pkmn.maxhp - active_pkmn.hp, active_pkmn.maxhp / 2);

                let wish_heal_instruction = Instruction::Heal(HealInstruction::new(
                    *side_ref,
                    0,
                    heal_amount,
                ));
                incoming_instructions
                    .instruction_list
                    .push(wish_heal_instruction);
                active_pkmn.hp += heal_amount;
            }
            side.wish.0 -= 1;
            incoming_instructions
                .instruction_list
                .push(decrement_wish_instruction);
        }
    }

    // status damage
    for side_ref in sides {
        let (side, other_side) = state.get_both_sides(side_ref);
        let toxic_count = side.side_conditions.toxic_count as f32;
        let active_pkmn = side.get_active();
        let other_side_active = other_side.get_active();
        if active_pkmn.hp == 0 || active_pkmn.ability == Abilities::MAGICGUARD {
            continue;
        }

        match active_pkmn.status {
            PokemonStatus::BURN => {
                #[cfg(any(feature = "gen3", feature = "gen4", feature = "gen5", feature = "gen6"))]
                let mut damage_factor = 0.125;

                #[cfg(any(feature = "gen7", feature = "gen8", feature = "gen9",))]
                let mut damage_factor = 0.0625;

                if active_pkmn.ability == Abilities::HEATPROOF {
                    damage_factor /= 2.0;
                }
                let damage_amount = cmp::max(
                    cmp::min(
                        (active_pkmn.maxhp as f32 * damage_factor) as i16,
                        active_pkmn.hp,
                    ),
                    1,
                );
                let burn_damage_instruction = Instruction::Damage(DamageInstruction::new(
                    *side_ref,
                    0,
                    damage_amount,
                ));
                active_pkmn.hp -= damage_amount;
                incoming_instructions
                    .instruction_list
                    .push(burn_damage_instruction);
            }
            PokemonStatus::POISON if active_pkmn.ability != Abilities::POISONHEAL => {
                let damage_amount = cmp::max(
                    1,
                    cmp::min((active_pkmn.maxhp as f32 * 0.125) as i16, active_pkmn.hp),
                );

                let poison_damage_instruction = Instruction::Damage(DamageInstruction::new(
                    *side_ref,
                    0,
                    damage_amount,
                ));
                active_pkmn.hp -= damage_amount;
                incoming_instructions
                    .instruction_list
                    .push(poison_damage_instruction);
            }
            PokemonStatus::TOXIC => {
                if active_pkmn.ability != Abilities::POISONHEAL
                    || other_side_active.ability == Abilities::NEUTRALIZINGGAS
                {
                    let toxic_multiplier = (1.0 / 16.0) * toxic_count + (1.0 / 16.0);
                    let damage_amount = cmp::max(
                        cmp::min(
                            (active_pkmn.maxhp as f32 * toxic_multiplier) as i16,
                            active_pkmn.hp,
                        ),
                        1,
                    );
                    let toxic_damage_instruction = Instruction::Damage(DamageInstruction::new(
                        *side_ref,
                        0,
                        damage_amount,
                    ));

                    active_pkmn.hp -= damage_amount;
                    incoming_instructions
                        .instruction_list
                        .push(toxic_damage_instruction);
                }

                // toxic counter is always incremented, even if the pokemon has poison heal
                let toxic_counter_increment_instruction =
                    Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                        side_ref: *side_ref,
                        side_condition: PokemonSideCondition::ToxicCount,
                        amount: 1,
                    });
                side.side_conditions.toxic_count += 1;
                incoming_instructions
                    .instruction_list
                    .push(toxic_counter_increment_instruction);
            }
            _ => {}
        }
    }

    // ability/item end-of-turn effects
    for side_ref in sides {
        let side = state.get_side(side_ref);
        let active_pkmn = side.get_active();
        if active_pkmn.hp == 0 {
            continue;
        }

        item_end_of_turn(state, side_ref, &mut incoming_instructions);
        ability_end_of_turn(state, side_ref, &mut incoming_instructions);
    }

    // leechseed sap
    for side_ref in sides {
        let (leechseed_side, other_side) = state.get_both_sides(side_ref);
        if leechseed_side
            .get_active().volatile_statuses
            .contains(&PokemonVolatileStatus::LEECHSEED)
        {
            let active_pkmn = leechseed_side.get_active();
            let other_active_pkmn = other_side.get_active();
            if active_pkmn.hp == 0
                || other_active_pkmn.hp == 0
                || active_pkmn.ability == Abilities::MAGICGUARD
            {
                continue;
            }

            let health_sapped = cmp::min((active_pkmn.maxhp as f32 * 0.125) as i16, active_pkmn.hp);
            let damage_ins = Instruction::Damage(DamageInstruction::new(
                *side_ref,
                0,
                health_sapped,
            ));
            active_pkmn.hp -= health_sapped;
            incoming_instructions.instruction_list.push(damage_ins);

            let health_recovered = cmp::min(
                health_sapped,
                other_active_pkmn.maxhp - other_active_pkmn.hp,
            );
            if health_recovered > 0 {
                let heal_ins = Instruction::Heal(HealInstruction::new(
                    side_ref.get_other_side(),
                    0,
                    health_recovered,
                ));
                other_active_pkmn.hp += health_recovered;
                incoming_instructions.instruction_list.push(heal_ins);
            }
        }
    }

    // volatile statuses
    for side_ref in sides {
        let side = state.get_side(side_ref);
        if side.get_active().hp == 0 {
            continue;
        }

        if side
            .get_active().volatile_statuses
            .contains(&PokemonVolatileStatus::SLOWSTART)
        {
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeVolatileStatusDuration(
                    ChangeVolatileStatusDurationInstruction::new(
                        *side_ref,
                        0,
                        PokemonVolatileStatus::SLOWSTART,
                        -1,
                    ),
                ));
            side.get_active().volatile_status_durations.slowstart -= 1;
            if side.get_active().volatile_status_durations.slowstart == 0 {
                incoming_instructions
                    .instruction_list
                    .push(Instruction::RemoveVolatileStatus(
                        RemoveVolatileStatusInstruction::new(
                            *side_ref,
                            0,
                            PokemonVolatileStatus::SLOWSTART,
                        ),
                    ));
                side.get_active().volatile_statuses
                    .remove(&PokemonVolatileStatus::SLOWSTART);
            }
        }

        if side
            .get_active().volatile_statuses
            .contains(&PokemonVolatileStatus::LOCKEDMOVE)
        {
            // the number says 2 but this is 3 turns of using a locking move
            // because turn 0 is the first turn the move is used
            // branching is not implemented here so the engine assumes it always lasts 3 turns
            if side.get_active().volatile_status_durations.lockedmove == 2 {
                side.get_active().volatile_status_durations.lockedmove = 0;
                side.get_active().volatile_statuses
                    .remove(&PokemonVolatileStatus::LOCKEDMOVE);
                incoming_instructions.instruction_list.push(
                    Instruction::ChangeVolatileStatusDuration(
                        ChangeVolatileStatusDurationInstruction::new(
                            *side_ref,
                            0,
                            PokemonVolatileStatus::LOCKEDMOVE,
                            -2,
                        ),
                    ),
                );
                incoming_instructions
                    .instruction_list
                    .push(Instruction::RemoveVolatileStatus(
                        RemoveVolatileStatusInstruction::new(
                            *side_ref,
                            0,
                            PokemonVolatileStatus::LOCKEDMOVE,
                        ),
                    ));
                if !side
                    .get_active().volatile_statuses
                    .contains(&PokemonVolatileStatus::CONFUSION)
                {
                    incoming_instructions
                        .instruction_list
                        .push(Instruction::ApplyVolatileStatus(
                            ApplyVolatileStatusInstruction::new(
                                *side_ref,
                                0,
                                PokemonVolatileStatus::CONFUSION,
                            ),
                        ));
                    side.get_active().volatile_statuses
                        .insert(PokemonVolatileStatus::CONFUSION);
                }
            } else {
                side.get_active().volatile_status_durations.lockedmove += 1;
                incoming_instructions.instruction_list.push(
                    Instruction::ChangeVolatileStatusDuration(
                        ChangeVolatileStatusDurationInstruction::new(
                            *side_ref,
                            0,
                            PokemonVolatileStatus::LOCKEDMOVE,
                            1,
                        ),
                    ),
                );
            }
        }

        if side
            .get_active().volatile_statuses
            .contains(&PokemonVolatileStatus::YAWN)
        {
            match side.get_active().volatile_status_durations.yawn {
                0 => {
                    incoming_instructions.instruction_list.push(
                        Instruction::ChangeVolatileStatusDuration(
                            ChangeVolatileStatusDurationInstruction::new(
                                *side_ref,
                                0,
                                PokemonVolatileStatus::YAWN,
                                1,
                            ),
                        ),
                    );
                    side.get_active().volatile_status_durations.yawn += 1;
                }
                1 => {
                    side.get_active().volatile_statuses.remove(&PokemonVolatileStatus::YAWN);
                    incoming_instructions
                        .instruction_list
                        .push(Instruction::RemoveVolatileStatus(
                            RemoveVolatileStatusInstruction::new(
                                *side_ref,
                                0,
                                PokemonVolatileStatus::YAWN,
                            ),
                        ));
                    incoming_instructions.instruction_list.push(
                        Instruction::ChangeVolatileStatusDuration(
                            ChangeVolatileStatusDurationInstruction::new(
                                *side_ref,
                                0,
                                PokemonVolatileStatus::YAWN,
                                -1,
                            ),
                        ),
                    );
                    side.get_active().volatile_status_durations.yawn -= 1;

                    let active = side.get_active();
                    if active.status == PokemonStatus::NONE {
                        active.status = PokemonStatus::SLEEP;
                        incoming_instructions
                            .instruction_list
                            .push(Instruction::ChangeStatus(ChangeStatusInstruction {
                                side_ref: *side_ref,
                                pokemon_index: side.active_indices[0],
                                old_status: PokemonStatus::NONE,
                                new_status: PokemonStatus::SLEEP,
                            }));
                    }
                }
                _ => panic!(
                    "Yawn duration cannot be {} when yawn volatile is active",
                    side.get_active().volatile_status_durations.yawn
                ),
            }
        }

        if side
            .get_active().volatile_statuses
            .contains(&PokemonVolatileStatus::PERISH1)
        {
            let active_pkmn = side.get_active();
            incoming_instructions
                .instruction_list
                .push(Instruction::Damage(DamageInstruction::new(
                    *side_ref,
                    0,
                    active_pkmn.hp,
                )));
            active_pkmn.hp = 0;
        }

        if side
            .get_active().volatile_statuses
            .remove(&PokemonVolatileStatus::PERISH2)
        {
            side.get_active().volatile_statuses
                .insert(PokemonVolatileStatus::PERISH1);
            incoming_instructions
                .instruction_list
                .push(Instruction::RemoveVolatileStatus(
                    RemoveVolatileStatusInstruction::new(
                        *side_ref,
                        0,
                        PokemonVolatileStatus::PERISH2,
                    ),
                ));
            incoming_instructions
                .instruction_list
                .push(Instruction::ApplyVolatileStatus(
                    ApplyVolatileStatusInstruction::new(
                        *side_ref,
                        0,
                        PokemonVolatileStatus::PERISH1,
                    ),
                ));
        }
        if side
            .get_active().volatile_statuses
            .remove(&PokemonVolatileStatus::PERISH3)
        {
            side.get_active().volatile_statuses
                .insert(PokemonVolatileStatus::PERISH2);
            incoming_instructions
                .instruction_list
                .push(Instruction::RemoveVolatileStatus(
                    RemoveVolatileStatusInstruction::new(
                        *side_ref,
                        0,
                        PokemonVolatileStatus::PERISH3,
                    ),
                ));
            incoming_instructions
                .instruction_list
                .push(Instruction::ApplyVolatileStatus(
                    ApplyVolatileStatusInstruction::new(
                        *side_ref,
                        0,
                        PokemonVolatileStatus::PERISH2,
                    ),
                ));
        }
        if side
            .get_active().volatile_statuses
            .remove(&PokemonVolatileStatus::PERISH4)
        {
            side.get_active().volatile_statuses
                .insert(PokemonVolatileStatus::PERISH3);
            incoming_instructions
                .instruction_list
                .push(Instruction::RemoveVolatileStatus(
                    RemoveVolatileStatusInstruction::new(
                        *side_ref,
                        0,
                        PokemonVolatileStatus::PERISH4,
                    ),
                ));
            incoming_instructions
                .instruction_list
                .push(Instruction::ApplyVolatileStatus(
                    ApplyVolatileStatusInstruction::new(
                        *side_ref,
                        0,
                        PokemonVolatileStatus::PERISH3,
                    ),
                ));
        }

        if side
            .get_active().volatile_statuses
            .remove(&PokemonVolatileStatus::FLINCH)
        {
            incoming_instructions
                .instruction_list
                .push(Instruction::RemoveVolatileStatus(
                    RemoveVolatileStatusInstruction::new(
                        *side_ref,
                        0,
                        PokemonVolatileStatus::FLINCH,
                    ),
                ));
        }
        if side.get_active().volatile_statuses.remove(&PokemonVolatileStatus::ROOST) {
            incoming_instructions
                .instruction_list
                .push(Instruction::RemoveVolatileStatus(
                    RemoveVolatileStatusInstruction::new(
                        *side_ref,
                        0,
                        PokemonVolatileStatus::ROOST,
                    ),
                ));
        }

        if side
            .get_active().volatile_statuses
            .contains(&PokemonVolatileStatus::PARTIALLYTRAPPED)
        {
            let active_pkmn = side.get_active();

            #[cfg(any(feature = "gen3", feature = "gen4", feature = "gen5"))]
            let damage_amount = cmp::min((active_pkmn.maxhp as f32 / 16.0) as i16, active_pkmn.hp);

            #[cfg(any(feature = "gen6", feature = "gen7", feature = "gen8", feature = "gen9"))]
            let damage_amount = cmp::min((active_pkmn.maxhp as f32 / 8.0) as i16, active_pkmn.hp);

            incoming_instructions
                .instruction_list
                .push(Instruction::Damage(DamageInstruction::new(
                    *side_ref,
                    0,
                    damage_amount,
                )));
            active_pkmn.hp -= damage_amount;
        }
        if side
            .get_active().volatile_statuses
            .contains(&PokemonVolatileStatus::SALTCURE)
        {
            let active_pkmn = side.get_active();
            let mut divisor = 8.0;
            if active_pkmn.has_type(&PokemonType::WATER)
                || active_pkmn.has_type(&PokemonType::STEEL)
            {
                divisor = 4.0;
            }
            let damage_amount =
                cmp::min((active_pkmn.maxhp as f32 / divisor) as i16, active_pkmn.hp);
            incoming_instructions
                .instruction_list
                .push(Instruction::Damage(DamageInstruction::new(
                    *side_ref,
                    0,
                    damage_amount,
                )));
            active_pkmn.hp -= damage_amount;
        }

        let possible_statuses = [
            PokemonVolatileStatus::PROTECT,
            PokemonVolatileStatus::BANEFULBUNKER,
            PokemonVolatileStatus::BURNINGBULWARK,
            PokemonVolatileStatus::SPIKYSHIELD,
            PokemonVolatileStatus::SILKTRAP,
            PokemonVolatileStatus::ENDURE,
        ];

        let mut protect_vs = None;
        for status in &possible_statuses {
            if side.get_active().volatile_statuses.contains(status) {
                protect_vs = Some(*status);
                break;
            }
        }

        if let Some(protect_vs) = protect_vs {
            incoming_instructions
                .instruction_list
                .push(Instruction::RemoveVolatileStatus(
                    RemoveVolatileStatusInstruction::new(
                        *side_ref,
                        0,
                        protect_vs,
                    ),
                ));
            side.get_active().volatile_statuses.remove(&protect_vs);
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeSideCondition(
                    ChangeSideConditionInstruction {
                        side_ref: *side_ref,
                        side_condition: PokemonSideCondition::Protect,
                        amount: 1,
                    },
                ));
            side.side_conditions.protect += 1;
        } else if side.side_conditions.protect > 0 {
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeSideCondition(
                    ChangeSideConditionInstruction {
                        side_ref: *side_ref,
                        side_condition: PokemonSideCondition::Protect,
                        amount: -1 * side.side_conditions.protect,
                    },
                ));
            side.side_conditions.protect -= side.side_conditions.protect;
        }

        // Doubles: the area-protection side conditions only last the turn they are
        // set, like Protect. Cleared here so they don't persist. Set only in
        // doubles (singles never blocks on them), so this is doubles-only.
        #[cfg(feature = "doubles")]
        for area_guard in [
            PokemonSideCondition::WideGuard,
            PokemonSideCondition::QuickGuard,
            PokemonSideCondition::CraftyShield,
        ] {
            let current = match area_guard {
                PokemonSideCondition::WideGuard => side.side_conditions.wide_guard,
                PokemonSideCondition::QuickGuard => side.side_conditions.quick_guard,
                PokemonSideCondition::CraftyShield => side.side_conditions.crafty_shield,
                _ => 0,
            };
            if current > 0 {
                incoming_instructions
                    .instruction_list
                    .push(Instruction::ChangeSideCondition(
                        ChangeSideConditionInstruction {
                            side_ref: *side_ref,
                            side_condition: area_guard,
                            amount: -current,
                        },
                    ));
                match area_guard {
                    PokemonSideCondition::WideGuard => side.side_conditions.wide_guard = 0,
                    PokemonSideCondition::QuickGuard => side.side_conditions.quick_guard = 0,
                    PokemonSideCondition::CraftyShield => side.side_conditions.crafty_shield = 0,
                    _ => {}
                }
            }
        }
    } // end volatile statuses
}

fn run_move(
    state: &mut State,
    attacking_side: SideReference,
    mut instructions: StateInstructions,
    hit_count: i8,
    does_damage: bool,
    damage_amount: i16,
    choice: &mut Choice,
    defender_choice: &Choice,
    final_instructions: &mut Vec<StateInstructions>,
) {
    let mut hit_sub = false;
    for _ in 0..hit_count {
        if does_damage {
            hit_sub = generate_instructions_from_damage(
                state,
                choice,
                damage_amount,
                &attacking_side,
                &mut instructions,
            );
        }
        if let Some(side_condition) = &choice.side_condition {
            generate_instructions_from_side_conditions(
                state,
                side_condition,
                &attacking_side,
                &mut instructions,
            );
        }
        choice_hazard_clear(state, &choice, &attacking_side, &mut instructions);
        if let Some(volatile_status) = &choice.volatile_status {
            get_instructions_from_volatile_statuses(
                state,
                &choice,
                volatile_status,
                &attacking_side,
                &mut instructions,
            );
        }
        if let Some(status) = &choice.status {
            get_instructions_from_status_effects(
                state,
                status,
                &attacking_side,
                &mut instructions,
                hit_sub,
            );
        }
        if let Some(heal) = &choice.heal {
            get_instructions_from_heal(state, heal, &attacking_side, &mut instructions);
        }
    } // end multi-hit
      // this is wrong, but I am deciding it is good enough for this engine (for now)
      // each multi-hit move should trigger a chance for a secondary effect,
      // but the way this engine was structured makes it difficult to implement
      // without some performance hits.

    if let Some(boost) = &choice.boost {
        get_instructions_from_boosts(state, boost, &attacking_side, &mut instructions);
    }

    let drag_target = state.defender_position(&attacking_side);
    if choice.flags.drag
        && state
            .get_side_immutable(&drag_target.side)
            .get_active_slot_immutable(drag_target.slot)
            .ability
            != Abilities::GUARDDOG
    {
        get_instructions_from_drag(state, &attacking_side, instructions, final_instructions);
        return;
    }

    // Only entered if the move causes a switch-out
    // U-turn, Volt Switch, Baton Pass, etc.
    // This deals with a bunch of flags that are required for the next turn to run properly
    if choice.flags.pivot {
        match attacking_side {
            SideReference::SideOne => {
                if state.side_one.visible_alive_pkmn() > 1 {
                    if choice.move_id == Choices::BATONPASS {
                        state.side_one.baton_passing = !state.side_one.baton_passing;
                        instructions
                            .instruction_list
                            .push(Instruction::ToggleBatonPassing(
                                ToggleBatonPassingInstruction {
                                    side_ref: SideReference::SideOne,
                                },
                            ));
                    } else if choice.move_id == Choices::SHEDTAIL {
                        state.side_one.shed_tailing = !state.side_one.shed_tailing;
                        instructions
                            .instruction_list
                            .push(Instruction::ToggleShedTailing(
                                ToggleShedTailingInstruction {
                                    side_ref: SideReference::SideOne,
                                },
                            ));
                    }
                    #[cfg(not(feature = "doubles"))]
                    {
                        state.side_one.force_switch = !state.side_one.force_switch;
                        instructions
                            .instruction_list
                            .push(Instruction::ToggleSideOneForceSwitch);
                    }
                    #[cfg(feature = "doubles")]
                    {
                        let slot = state.acting_slot;
                        state
                            .side_one
                            .set_force_switch_slot(slot, !state.side_one.force_switch_slot(slot));
                        instructions.instruction_list.push(
                            Instruction::ToggleForceSwitchSlot(ToggleForceSwitchSlotInstruction {
                                side_ref: SideReference::SideOne,
                                slot,
                            }),
                        );
                    }

                    if choice.first_move {
                        instructions.instruction_list.push(
                            Instruction::SetSideTwoMoveSecondSwitchOutMove(
                                SetSecondMoveSwitchOutMoveInstruction {
                                    new_choice: defender_choice.move_id,
                                    previous_choice: state
                                        .side_two
                                        .switch_out_move_second_saved_move,
                                },
                            ),
                        );
                        state.side_two.switch_out_move_second_saved_move = defender_choice.move_id;
                    } else {
                        instructions.instruction_list.push(
                            Instruction::SetSideTwoMoveSecondSwitchOutMove(
                                SetSecondMoveSwitchOutMoveInstruction {
                                    new_choice: Choices::NONE,
                                    previous_choice: state
                                        .side_two
                                        .switch_out_move_second_saved_move,
                                },
                            ),
                        );
                        state.side_two.switch_out_move_second_saved_move = defender_choice.move_id;
                    }
                }
            }
            SideReference::SideTwo => {
                if state.side_two.visible_alive_pkmn() > 1 {
                    if choice.move_id == Choices::BATONPASS {
                        state.side_two.baton_passing = !state.side_two.baton_passing;
                        instructions
                            .instruction_list
                            .push(Instruction::ToggleBatonPassing(
                                ToggleBatonPassingInstruction {
                                    side_ref: SideReference::SideTwo,
                                },
                            ));
                    } else if choice.move_id == Choices::SHEDTAIL {
                        state.side_two.shed_tailing = !state.side_two.shed_tailing;
                        instructions
                            .instruction_list
                            .push(Instruction::ToggleShedTailing(
                                ToggleShedTailingInstruction {
                                    side_ref: SideReference::SideTwo,
                                },
                            ));
                    }
                    #[cfg(not(feature = "doubles"))]
                    {
                        state.side_two.force_switch = !state.side_two.force_switch;
                        instructions
                            .instruction_list
                            .push(Instruction::ToggleSideTwoForceSwitch);
                    }
                    #[cfg(feature = "doubles")]
                    {
                        let slot = state.acting_slot;
                        state
                            .side_two
                            .set_force_switch_slot(slot, !state.side_two.force_switch_slot(slot));
                        instructions.instruction_list.push(
                            Instruction::ToggleForceSwitchSlot(ToggleForceSwitchSlotInstruction {
                                side_ref: SideReference::SideTwo,
                                slot,
                            }),
                        );
                    }

                    if choice.first_move {
                        instructions.instruction_list.push(
                            Instruction::SetSideOneMoveSecondSwitchOutMove(
                                SetSecondMoveSwitchOutMoveInstruction {
                                    new_choice: defender_choice.move_id,
                                    previous_choice: state
                                        .side_one
                                        .switch_out_move_second_saved_move,
                                },
                            ),
                        );
                        state.side_one.switch_out_move_second_saved_move = defender_choice.move_id;
                    } else {
                        instructions.instruction_list.push(
                            Instruction::SetSideOneMoveSecondSwitchOutMove(
                                SetSecondMoveSwitchOutMoveInstruction {
                                    new_choice: Choices::NONE,
                                    previous_choice: state
                                        .side_one
                                        .switch_out_move_second_saved_move,
                                },
                            ),
                        );
                        state.side_one.switch_out_move_second_saved_move = defender_choice.move_id;
                    }
                }
            }
        }
    }

    let secondary_target = state.defender_position(&attacking_side);
    if state
        .get_side_immutable(&secondary_target.side)
        .get_active_slot_immutable(secondary_target.slot)
        .item
        == Items::COVERTCLOAK
    {
        state.reverse_instructions(&instructions.instruction_list);
        final_instructions.push(instructions);
    } else if let Some(secondaries_vec) = &choice.secondaries {
        state.reverse_instructions(&instructions.instruction_list);
        let instructions_vec_after_secondaries = get_instructions_from_secondaries(
            state,
            &choice,
            secondaries_vec,
            &attacking_side,
            instructions,
            hit_sub,
        );
        final_instructions.extend(instructions_vec_after_secondaries);
    } else {
        state.reverse_instructions(&instructions.instruction_list);
        final_instructions.push(instructions);
    }
}

fn after_move_finish(state: &mut State, final_instructions: &mut Vec<StateInstructions>) {
    for state_instructions in final_instructions.iter_mut() {
        state.apply_instructions(&state_instructions.instruction_list);

        // check if anybody has negative boosts and a whiteherb
        // if so, consume the item and set the boosts to 0
        for side_ref in [SideReference::SideOne, SideReference::SideTwo] {
            let side = state.get_side(&side_ref);
            let active_has_whiteherb = side.get_active_immutable().item == Items::WHITEHERB;
            if active_has_whiteherb {
                if side.reset_negative_boosts(side_ref, state_instructions) {
                    let active = side.get_active();
                    active.item = Items::NONE;
                    // FIXME(doubles): slot
                    state_instructions
                        .instruction_list
                        .push(Instruction::ChangeItem(ChangeItemInstruction::new(
                            side_ref,
                            0,
                            Items::WHITEHERB,
                            Items::NONE,
                        )));
                }
            }
        }
        state.reverse_instructions(&state_instructions.instruction_list);
    }
}

#[cfg(not(feature = "doubles"))]
fn handle_both_moves(
    state: &mut State,
    first_move_side_choice: &mut Choice,
    second_move_side_choice: &mut Choice,
    first_move_side_ref: SideReference,
    incoming_instructions: StateInstructions,
    state_instructions_vec: &mut Vec<StateInstructions>,
    branch_on_damage: bool,
) {
    generate_instructions_from_move(
        state,
        first_move_side_choice,
        second_move_side_choice,
        first_move_side_ref,
        incoming_instructions,
        state_instructions_vec,
        branch_on_damage,
    );
    after_move_finish(state, state_instructions_vec);

    let mut i = 0;
    let vec_len = state_instructions_vec.len();
    second_move_side_choice.first_move = false;
    while i < vec_len {
        let state_instruction = state_instructions_vec.remove(0);
        generate_instructions_from_move(
            state,
            &mut second_move_side_choice.clone(), // this clone is needed because the choice may be modified in this loop
            first_move_side_choice,
            first_move_side_ref.get_other_side(),
            state_instruction,
            state_instructions_vec,
            branch_on_damage,
        );
        after_move_finish(state, state_instructions_vec);
        i += 1;
    }
}

fn mega_evolve(state: &mut State, side_ref: SideReference, instructions: &mut StateInstructions) {
    let act_slot = state.actor_slot();
    let _def_pos = state.defender_position(&side_ref);
    let side = state.get_side(&side_ref);
    let active_pkmn = side.get_active();

    // assumes that you can mega-evolve if this function is called
    let mega_evolve_data = active_pkmn
        .id
        .mega_evolve_target(active_pkmn.item)
        .unwrap_or_else(|| {
            panic!(
                "cannot mega evolve {:?} with {:?}",
                active_pkmn.id, active_pkmn.item
            )
        });

    // change id
    instructions
        .instruction_list
        .push(Instruction::FormeChange(FormeChangeInstruction::new(
            side_ref,
            act_slot,
            mega_evolve_data.id as i16 - active_pkmn.id as i16,
        )));
    active_pkmn.id = mega_evolve_data.id;

    // change stats
    active_pkmn.recalculate_stats(&side_ref, instructions);

    // change ability
    if mega_evolve_data.ability != active_pkmn.ability {
        instructions
            .instruction_list
            .push(Instruction::ChangeAbility(ChangeAbilityInstruction::new(
                side_ref,
                act_slot,
                mega_evolve_data.ability as i16 - active_pkmn.ability as i16,
            )));
        active_pkmn.ability = mega_evolve_data.ability;
    }
    // change type
    if mega_evolve_data.types != active_pkmn.types {
        instructions
            .instruction_list
            .push(Instruction::ChangeType(ChangeType::new(
                side_ref,
                act_slot,
                mega_evolve_data.types,
                active_pkmn.types,
            )));
        active_pkmn.types = mega_evolve_data.types;
    }

    // ability on switch in
    ability_on_switch_in(state, &side_ref, instructions);
}

#[cfg(not(feature = "doubles"))]
pub fn generate_instructions_from_move_pair(
    state: &mut State,
    side_one_move: &MoveChoice,
    side_two_move: &MoveChoice,
    branch_on_damage: bool,
) -> Vec<StateInstructions> {
    let mut side_one_choice;
    let mut s1_tera = false;
    let mut s1_mega = false;
    let mut s1_replacing_fainted_pkmn = false;
    match side_one_move {
        MoveChoice::Switch(switch_id) => {
            if state.side_one.get_active().hp == 0 {
                s1_replacing_fainted_pkmn = true;
            }
            side_one_choice = Choice::default();
            side_one_choice.switch_id = *switch_id;
            side_one_choice.category = MoveCategory::Switch;
        }
        MoveChoice::Move { move_index, .. } => {
            side_one_choice = state.side_one.get_active().moves[move_index].choice.clone();
            side_one_choice.move_index = *move_index;
        }
        MoveChoice::MoveTera { move_index, .. } => {
            side_one_choice = state.side_one.get_active().moves[move_index].choice.clone();
            side_one_choice.move_index = *move_index;
            s1_tera = true;
        }
        MoveChoice::MoveMega { move_index, .. } => {
            side_one_choice = state.side_one.get_active().moves[move_index].choice.clone();
            side_one_choice.move_index = *move_index;
            s1_mega = true;
        }
        MoveChoice::None => {
            side_one_choice = Choice::default();
        }
    }

    let mut side_two_choice;
    let mut s2_replacing_fainted_pkmn = false;
    let mut s2_tera = false;
    let mut s2_mega = false;
    match side_two_move {
        MoveChoice::Switch(switch_id) => {
            if state.side_two.get_active().hp == 0 {
                s2_replacing_fainted_pkmn = true;
            }
            side_two_choice = Choice::default();
            side_two_choice.switch_id = *switch_id;
            side_two_choice.category = MoveCategory::Switch;
        }
        MoveChoice::Move { move_index, .. } => {
            side_two_choice = state.side_two.get_active().moves[move_index].choice.clone();
            side_two_choice.move_index = *move_index;
        }
        MoveChoice::MoveTera { move_index, .. } => {
            side_two_choice = state.side_two.get_active().moves[move_index].choice.clone();
            side_two_choice.move_index = *move_index;
            s2_tera = true;
        }
        MoveChoice::MoveMega { move_index, .. } => {
            side_two_choice = state.side_two.get_active().moves[move_index].choice.clone();
            side_two_choice.move_index = *move_index;
            s2_mega = true;
        }
        MoveChoice::None => {
            side_two_choice = Choice::default();
        }
    }

    let mut state_instructions_vec: Vec<StateInstructions> = Vec::with_capacity(4);
    let mut incoming_instructions: StateInstructions = StateInstructions::default();

    // Run terastallization / Mega evolutions
    // Note: only create/apply instructions, don't apply changes
    // generate_instructions_from_move() assumes instructions have not been applied
    // technically, switches should happen _before_ this, but this is fine for now
    if s1_tera {
        state.side_one.get_active().terastallized = true;
        incoming_instructions
            .instruction_list
            .push(Instruction::ToggleTerastallized(
                ToggleTerastallizedInstruction {
                    side_ref: SideReference::SideOne,
                },
            ));
    }
    if s2_tera {
        state.side_two.get_active().terastallized = true;
        incoming_instructions
            .instruction_list
            .push(Instruction::ToggleTerastallized(
                ToggleTerastallizedInstruction {
                    side_ref: SideReference::SideTwo,
                },
            ));
    }
    if s1_mega {
        mega_evolve(state, SideReference::SideOne, &mut incoming_instructions);
    }
    if s2_mega {
        mega_evolve(state, SideReference::SideTwo, &mut incoming_instructions);
    }

    modify_choice_priority(&state, &SideReference::SideOne, &mut side_one_choice);
    modify_choice_priority(&state, &SideReference::SideTwo, &mut side_two_choice);

    // reverse instructions because mega-evolving might've added some
    state.reverse_instructions(&incoming_instructions.instruction_list);

    match moves_first(
        &state,
        &side_one_choice,
        &side_two_choice,
        &mut incoming_instructions,
    ) {
        SideMovesFirst::SideOne => {
            handle_both_moves(
                state,
                &mut side_one_choice,
                &mut side_two_choice,
                SideReference::SideOne,
                incoming_instructions,
                &mut state_instructions_vec,
                branch_on_damage,
            );

            for state_instruction in state_instructions_vec.iter_mut() {
                state.apply_instructions(&state_instruction.instruction_list);
                if !(s1_replacing_fainted_pkmn
                    || s2_replacing_fainted_pkmn
                    || state.side_one.force_switch
                    || state.side_two.force_switch)
                {
                    add_end_of_turn_instructions(state, state_instruction, &SideReference::SideOne);
                }
                state.reverse_instructions(&state_instruction.instruction_list);
            }
        }
        SideMovesFirst::SideTwo => {
            handle_both_moves(
                state,
                &mut side_two_choice,
                &mut side_one_choice,
                SideReference::SideTwo,
                incoming_instructions,
                &mut state_instructions_vec,
                branch_on_damage,
            );
            for state_instruction in state_instructions_vec.iter_mut() {
                state.apply_instructions(&state_instruction.instruction_list);
                if !(s1_replacing_fainted_pkmn
                    || s2_replacing_fainted_pkmn
                    || state.side_one.force_switch
                    || state.side_two.force_switch)
                {
                    add_end_of_turn_instructions(state, state_instruction, &SideReference::SideTwo);
                }
                state.reverse_instructions(&state_instruction.instruction_list);
            }
        }
        SideMovesFirst::SpeedTie => {
            let mut side_one_moves_first_instruction = incoming_instructions.clone();
            incoming_instructions.update_percentage(0.5);
            side_one_moves_first_instruction.update_percentage(0.5);

            // side_one moves first
            handle_both_moves(
                state,
                &mut side_one_choice,
                &mut side_two_choice,
                SideReference::SideOne,
                side_one_moves_first_instruction,
                &mut state_instructions_vec,
                branch_on_damage,
            );
            for state_instruction in state_instructions_vec.iter_mut() {
                state.apply_instructions(&state_instruction.instruction_list);
                if !(s1_replacing_fainted_pkmn
                    || s2_replacing_fainted_pkmn
                    || state.side_one.force_switch
                    || state.side_two.force_switch)
                {
                    add_end_of_turn_instructions(state, state_instruction, &SideReference::SideOne);
                }
                state.reverse_instructions(&state_instruction.instruction_list);
            }

            // side_two moves first
            let mut side_two_moves_first_si = Vec::with_capacity(4);
            handle_both_moves(
                state,
                &mut side_two_choice,
                &mut side_one_choice,
                SideReference::SideTwo,
                incoming_instructions,
                &mut side_two_moves_first_si,
                branch_on_damage,
            );
            for state_instruction in side_two_moves_first_si.iter_mut() {
                state.apply_instructions(&state_instruction.instruction_list);
                if !(s1_replacing_fainted_pkmn
                    || s2_replacing_fainted_pkmn
                    || state.side_one.force_switch
                    || state.side_two.force_switch)
                {
                    add_end_of_turn_instructions(state, state_instruction, &SideReference::SideTwo);
                }
                state.reverse_instructions(&state_instruction.instruction_list);
            }

            // combine both vectors into the final vector
            state_instructions_vec.extend(side_two_moves_first_si);
        }
    }
    state_instructions_vec
}

// ============================================================================
// Doubles turn-resolution core (feature = "doubles").
//
// Generalizes the singles "2 actors / binary order" turn into "up to 4 actors
// ordered". The singles path above is frozen under `cfg(not(doubles))`; nothing
// below runs in a singles build, so the singles bit-for-bit guarantee is by
// construction. For exactly 2 actors this path reproduces the singles ordering
// (priority brackets, Custap, Pursuit/switch asymmetry, Trick Room, speed-tie
// 0.5/0.5 split) via `compare_actors`, which is a slot-parameterized copy of
// `moves_first`.
// ============================================================================

/// One actor in a turn: a battle position plus the fully-built `Choice` it will
/// execute, and the position its move primarily targets (used to find the
/// "defender choice" the way the singles path passes the opposing side's choice).
#[cfg(feature = "doubles")]
#[derive(Clone)]
struct Actor {
    position: BattlePosition,
    choice: Choice,
    target: Option<BattlePosition>,
}

#[cfg(feature = "doubles")]
#[derive(Clone, Copy, PartialEq)]
enum PairOrder {
    First,
    Second,
    Tie,
}

/// Slot-aware effective speed (doubles). Copy of [`get_effective_speed`] reading
/// the active in `slot`; for `slot == 0` it is identical to that function.
#[cfg(feature = "doubles")]
fn get_effective_speed_slot(state: &State, side_reference: SideReference, slot: u8) -> i16 {
    let side = state.get_side_immutable(&side_reference);
    let active_pkmn = side.get_active_slot_immutable(slot);

    let mut boosted_speed = side.calculate_boosted_speed_slot(slot) as f32;

    match state.weather.weather_type {
        Weather::SUN | Weather::HARSHSUN if active_pkmn.ability == Abilities::CHLOROPHYLL => {
            boosted_speed *= 2.0
        }
        Weather::RAIN | Weather::HEAVYRAIN if active_pkmn.ability == Abilities::SWIFTSWIM => {
            boosted_speed *= 2.0
        }
        Weather::SAND if active_pkmn.ability == Abilities::SANDRUSH => boosted_speed *= 2.0,
        Weather::HAIL if active_pkmn.ability == Abilities::SLUSHRUSH => boosted_speed *= 2.0,
        _ => {}
    }

    match active_pkmn.ability {
        Abilities::SURGESURFER if state.terrain.terrain_type == Terrain::ELECTRICTERRAIN => {
            boosted_speed *= 2.0
        }
        Abilities::UNBURDEN
            if active_pkmn
                .volatile_statuses
                .contains(&PokemonVolatileStatus::UNBURDEN) =>
        {
            boosted_speed *= 2.0
        }
        Abilities::QUICKFEET if active_pkmn.status != PokemonStatus::NONE => boosted_speed *= 1.5,
        _ => {}
    }

    if active_pkmn
        .volatile_statuses
        .contains(&PokemonVolatileStatus::SLOWSTART)
    {
        boosted_speed *= 0.5;
    }

    if active_pkmn
        .volatile_statuses
        .contains(&PokemonVolatileStatus::PROTOSYNTHESISSPE)
        || active_pkmn
            .volatile_statuses
            .contains(&PokemonVolatileStatus::QUARKDRIVESPE)
    {
        boosted_speed *= 1.5;
    }

    if side.side_conditions.tailwind > 0 {
        boosted_speed *= 2.0
    }

    match active_pkmn.item {
        Items::IRONBALL => boosted_speed *= 0.5,
        Items::CHOICESCARF => boosted_speed *= 1.5,
        _ => {}
    }

    #[cfg(any(feature = "gen3", feature = "gen4", feature = "gen5", feature = "gen6"))]
    if active_pkmn.status == PokemonStatus::PARALYZE && active_pkmn.ability != Abilities::QUICKFEET
    {
        boosted_speed *= 0.25;
    }

    #[cfg(any(feature = "gen7", feature = "gen8", feature = "gen9"))]
    if active_pkmn.status == PokemonStatus::PARALYZE && active_pkmn.ability != Abilities::QUICKFEET
    {
        boosted_speed *= 0.50;
    }

    boosted_speed as i16
}

/// Pairwise ordering decision for two actors. This is a slot-parameterized copy
/// of [`moves_first`] (the singles orderer) with `SideOne`/`SideTwo` replaced by
/// `a`/`b`. It may push a Custap-berry `ChangeItem` into `incoming_instructions`,
/// exactly as `moves_first` does. For two slot-0 actors it produces identical
/// results to `moves_first`.
#[cfg(feature = "doubles")]
fn compare_actors(
    state: &State,
    a: &Actor,
    b: &Actor,
    incoming_instructions: &mut StateInstructions,
) -> PairOrder {
    let a_effective_speed = get_effective_speed_slot(state, a.position.side, a.position.slot);
    let b_effective_speed = get_effective_speed_slot(state, b.position.side, b.position.slot);

    if a.choice.category == MoveCategory::Switch && b.choice.category == MoveCategory::Switch {
        return if a_effective_speed > b_effective_speed {
            PairOrder::First
        } else if a_effective_speed == b_effective_speed {
            PairOrder::Tie
        } else {
            PairOrder::Second
        };
    } else if a.choice.category == MoveCategory::Switch {
        return if b.choice.move_id != Choices::PURSUIT {
            PairOrder::First
        } else {
            PairOrder::Second
        };
    } else if b.choice.category == MoveCategory::Switch {
        return if a.choice.move_id == Choices::PURSUIT {
            PairOrder::First
        } else {
            PairOrder::Second
        };
    }

    let a_active = state
        .get_side_immutable(&a.position.side)
        .get_active_slot_immutable(a.position.slot);
    let b_active = state
        .get_side_immutable(&b.position.side)
        .get_active_slot_immutable(b.position.slot);
    if a.choice.priority == b.choice.priority {
        if a_active.item == Items::CUSTAPBERRY && a_active.hp < a_active.maxhp / 4 {
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeItem(ChangeItemInstruction::new(
                    a.position.side,
                    a.position.slot,
                    Items::CUSTAPBERRY,
                    Items::NONE,
                )));
            return PairOrder::First;
        } else if b_active.item == Items::CUSTAPBERRY && b_active.hp < b_active.maxhp / 4 {
            incoming_instructions
                .instruction_list
                .push(Instruction::ChangeItem(ChangeItemInstruction::new(
                    b.position.side,
                    b.position.slot,
                    Items::CUSTAPBERRY,
                    Items::NONE,
                )));
            return PairOrder::Second;
        }

        if a_effective_speed == b_effective_speed {
            return PairOrder::Tie;
        }

        match state.trick_room.active {
            true => {
                if a_effective_speed < b_effective_speed {
                    PairOrder::First
                } else {
                    PairOrder::Second
                }
            }
            false => {
                if a_effective_speed > b_effective_speed {
                    PairOrder::First
                } else {
                    PairOrder::Second
                }
            }
        }
    } else if a.choice.priority > b.choice.priority {
        PairOrder::First
    } else {
        PairOrder::Second
    }
}

/// All permutations of a list of indices (only ever called with <= 4 elements).
#[cfg(feature = "doubles")]
fn permutations_of(items: &[usize]) -> Vec<Vec<usize>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for i in 0..items.len() {
        let mut rest: Vec<usize> = Vec::with_capacity(items.len() - 1);
        rest.extend_from_slice(&items[..i]);
        rest.extend_from_slice(&items[i + 1..]);
        for mut perm in permutations_of(&rest) {
            let mut full = Vec::with_capacity(items.len());
            full.push(items[i]);
            full.append(&mut perm);
            out.push(full);
        }
    }
    out
}

/// Order the actors into one or more execution sequences, each with the
/// `incoming` instructions it carries. Speed ties spawn probabilistic branches.
///
/// For exactly 2 actors this reproduces `moves_first`'s three outcomes and the
/// SpeedTie 0.5/0.5 split (s1-first branch emitted before s2-first), so the
/// degenerate doubles turn matches the singles structure.
#[cfg(feature = "doubles")]
fn order_actors(
    state: &State,
    actors: Vec<Actor>,
    incoming: StateInstructions,
) -> Vec<(Vec<Actor>, StateInstructions)> {
    match actors.len() {
        0 => vec![(actors, incoming)],
        1 => vec![(actors, incoming)],
        2 => {
            let mut inc = incoming;
            let a = actors[0].clone();
            let b = actors[1].clone();
            match compare_actors(state, &a, &b, &mut inc) {
                PairOrder::First => vec![(vec![a, b], inc)],
                PairOrder::Second => vec![(vec![b, a], inc)],
                PairOrder::Tie => {
                    let mut a_first = inc.clone();
                    a_first.update_percentage(0.5);
                    inc.update_percentage(0.5);
                    vec![(vec![a.clone(), b.clone()], a_first), (vec![b, a], inc)]
                }
            }
        }
        _ => order_actors_n(state, actors, incoming),
    }
}

/// Ordering for 3-4 actors: switches first, then by priority bracket, then by
/// effective speed (reversed under Trick Room). Equal (switch-class, priority,
/// speed) actors form a tie group whose permutations each spawn an equiprobable
/// branch (bounded: <= 4 actors => at most 24 orderings). Custap special-casing
/// is intentionally not applied here (only in the 2-actor `compare_actors`
/// path); its multi-actor interaction is a documented follow-up.
#[cfg(feature = "doubles")]
fn order_actors_n(
    state: &State,
    actors: Vec<Actor>,
    incoming: StateInstructions,
) -> Vec<(Vec<Actor>, StateInstructions)> {
    let n = actors.len();
    let tr = state.trick_room.active;

    // (is_switch, priority, effective_speed) per actor index.
    let meta: Vec<(bool, i8, i16)> = actors
        .iter()
        .map(|a| {
            (
                a.choice.category == MoveCategory::Switch,
                a.choice.priority,
                get_effective_speed_slot(state, a.position.side, a.position.slot),
            )
        })
        .collect();

    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&i, &j| {
        let (si, pi, spi) = meta[i];
        let (sj, pj, spj) = meta[j];
        // switches first, then higher priority first, then speed
        // (descending normally, ascending under Trick Room)
        sj.cmp(&si)
            .then(pj.cmp(&pi))
            .then(if tr { spi.cmp(&spj) } else { spj.cmp(&spi) })
    });

    // Group consecutive tied indices (same is_switch, priority, speed).
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for &i in &idx {
        match groups.last() {
            Some(g) if meta[*g.last().unwrap()] == meta[i] => {
                groups.last_mut().unwrap().push(i);
            }
            _ => groups.push(vec![i]),
        }
    }

    // Cartesian product of each group's permutations.
    let mut orderings: Vec<Vec<usize>> = vec![Vec::with_capacity(n)];
    for g in &groups {
        let perms = permutations_of(g);
        let mut next: Vec<Vec<usize>> = Vec::with_capacity(orderings.len() * perms.len());
        for base in &orderings {
            for perm in &perms {
                let mut combined = base.clone();
                combined.extend_from_slice(perm);
                next.push(combined);
            }
        }
        orderings = next;
    }

    let total = orderings.len() as f32;
    orderings
        .into_iter()
        .map(|order| {
            let mut inc = incoming.clone();
            if total > 1.0 {
                inc.update_percentage(1.0 / total);
            }
            let ordered: Vec<Actor> = order.into_iter().map(|i| actors[i].clone()).collect();
            (ordered, inc)
        })
        .collect()
}

/// Resolve a single ordered actor sequence, mirroring `handle_both_moves` but
/// for an arbitrary number of actors. Each actor's move is generated over every
/// branch produced so far, then `after_move_finish` runs, then the next actor.
/// A fainted actor no-ops via `generate_instructions_from_move`'s hp==0 early
/// return, so faints need no explicit reordering. (Dynamic re-ordering after
/// mid-turn speed changes among 3+ remaining actors is a documented follow-up;
/// for 2 actors there is nothing to re-order, so this matches singles exactly.)
#[cfg(feature = "doubles")]
fn resolve_ordered_actors(
    state: &mut State,
    ordered: &[Actor],
    incoming: StateInstructions,
    out: &mut Vec<StateInstructions>,
    branch_on_damage: bool,
) {
    // Static map of each position to its chosen Choice, used as defender_choice
    // (the singles path passes the opposing side's choice; here we pass the
    // choice of whoever occupies the move's primary target position).
    let mut work: Vec<StateInstructions> = vec![incoming];
    for (idx, actor) in ordered.iter().enumerate() {
        // The "defender choice" mirrors the singles engine, which always passes
        // the opposing active's choice (used for drag/Pursuit/Sucker Punch
        // interactions). Use the move's explicit target only when it actually
        // points at the opposing side (singles tests leave `target` arbitrary,
        // so trusting it blindly can resolve to the actor's own choice); else
        // fall back to any actor on the opposing side.
        let opp_side = actor.position.side.get_other_side();
        let defender_choice = actor
            .target
            .filter(|t| t.side == opp_side)
            .and_then(|t| ordered.iter().find(|o| o.position == t))
            .or_else(|| ordered.iter().find(|o| o.position.side == opp_side))
            .map(|o| o.choice.clone())
            .unwrap_or_default();

        let mut next: Vec<StateInstructions> = Vec::with_capacity(work.len());
        state.acting_slot = actor.position.slot;
        // Default the move's target to the actor's explicit choice target, or the
        // position directly across if none was given. A spread move overrides
        // this per living target inside `generate_instructions_from_move`.
        state.target_position = actor.target.unwrap_or_else(|| actor.position.opposing());
        for branch in work.drain(..) {
            let mut choice = actor.choice.clone();
            choice.first_move = idx == 0;
            generate_instructions_from_move(
                state,
                &mut choice,
                &defender_choice,
                actor.position.side,
                branch,
                &mut next,
                branch_on_damage,
            );
        }
        after_move_finish(state, &mut next);
        work = next;
    }
    out.extend(work);
}

/// Build the `Choice` for one actor from its `MoveChoice`, mirroring the per-side
/// match in the singles pair path but reading the active in `slot`. Returns the
/// choice, the move's primary target (if any), and the tera/mega/replacing-fainted
/// flags (tera/mega are applied to `state` by the caller).
#[cfg(feature = "doubles")]
fn build_actor_choice(
    state: &mut State,
    side_ref: SideReference,
    slot: u8,
    move_choice: &MoveChoice,
) -> (Choice, Option<BattlePosition>, bool, bool, bool) {
    let mut tera = false;
    let mut mega = false;
    let mut replacing_fainted = false;
    let mut target = None;
    let choice = match move_choice {
        MoveChoice::Switch(switch_id) => {
            if state.get_side(&side_ref).get_active_slot(slot).hp == 0 {
                replacing_fainted = true;
            }
            let mut c = Choice::default();
            c.switch_id = *switch_id;
            c.category = MoveCategory::Switch;
            c
        }
        MoveChoice::Move { move_index, target: t } => {
            let mut c = state.get_side(&side_ref).get_active_slot(slot).moves[move_index]
                .choice
                .clone();
            c.move_index = *move_index;
            target = Some(*t);
            c
        }
        MoveChoice::MoveTera { move_index, target: t } => {
            let mut c = state.get_side(&side_ref).get_active_slot(slot).moves[move_index]
                .choice
                .clone();
            c.move_index = *move_index;
            target = Some(*t);
            tera = true;
            c
        }
        MoveChoice::MoveMega { move_index, target: t } => {
            let mut c = state.get_side(&side_ref).get_active_slot(slot).moves[move_index]
                .choice
                .clone();
            c.move_index = *move_index;
            target = Some(*t);
            mega = true;
            c
        }
        MoveChoice::None => Choice::default(),
    };
    (choice, target, tera, mega, replacing_fainted)
}

/// Doubles turn resolution from combined per-side actions. This is the
/// generalized core; `generate_instructions_from_move_pair` (below) is a thin
/// wrapper that packages one `MoveChoice` per side into a `SideAction`.
#[cfg(feature = "doubles")]
pub fn generate_instructions_from_actions(
    state: &mut State,
    side_one_action: &SideAction,
    side_two_action: &SideAction,
    branch_on_damage: bool,
) -> Vec<StateInstructions> {
    let mut incoming_instructions = StateInstructions::default();
    let mut actors: Vec<Actor> = Vec::with_capacity(ACTIVE_PER_SIDE * 2);
    let mut any_replacing_fainted = false;

    // Build actors: side_one slots in order, then side_two slots in order. A
    // `None` sub-action (empty/fainted/do-nothing slot) contributes no actor.
    for (side_ref, action) in [
        (SideReference::SideOne, side_one_action),
        (SideReference::SideTwo, side_two_action),
    ] {
        for slot in 0..ACTIVE_PER_SIDE as u8 {
            let move_choice = action.actions[slot as usize];
            if matches!(move_choice, MoveChoice::None) {
                continue;
            }
            let (choice, target, tera, mega, replacing_fainted) =
                build_actor_choice(state, side_ref, slot, &move_choice);
            if replacing_fainted {
                any_replacing_fainted = true;
            }
            if tera {
                state.get_side(&side_ref).get_active_slot(slot).terastallized = true;
                incoming_instructions
                    .instruction_list
                    .push(Instruction::ToggleTerastallized(
                        ToggleTerastallizedInstruction { side_ref },
                    ));
            }
            if mega {
                mega_evolve(state, side_ref, &mut incoming_instructions);
            }
            actors.push(Actor {
                position: BattlePosition::new(side_ref, slot),
                choice,
                target,
            });
        }
    }

    for actor in actors.iter_mut() {
        modify_choice_priority(&state, &actor.position.side, &mut actor.choice);
    }

    // Reverse tera/mega instructions: ordering reads the pre-move state, exactly
    // as the singles pair path does before calling `moves_first`.
    state.reverse_instructions(&incoming_instructions.instruction_list);

    let mut final_instructions: Vec<StateInstructions> = Vec::new();
    for (ordered, inc) in order_actors(&state, actors, incoming_instructions) {
        if ordered.is_empty() {
            // No actors at all (both sides did nothing): still run end of turn.
            let mut branch = inc;
            state.apply_instructions(&branch.instruction_list);
            if !(any_replacing_fainted
                || state.side_one.any_force_switch()
                || state.side_two.any_force_switch())
            {
                add_end_of_turn_instructions(state, &mut branch, &SideReference::SideOne);
            }
            state.reverse_instructions(&branch.instruction_list);
            final_instructions.push(branch);
            continue;
        }
        let end_of_turn_side = ordered[0].position.side;
        let mut branch_out: Vec<StateInstructions> = Vec::new();
        resolve_ordered_actors(state, &ordered, inc, &mut branch_out, branch_on_damage);
        combine_duplicate_instructions(&mut branch_out);
        for si in branch_out.iter_mut() {
            state.apply_instructions(&si.instruction_list);
            if !(any_replacing_fainted
                || state.side_one.any_force_switch()
                || state.side_two.any_force_switch())
            {
                add_end_of_turn_instructions(state, si, &end_of_turn_side);
            }
            state.reverse_instructions(&si.instruction_list);
        }
        final_instructions.extend(branch_out);
    }
    combine_duplicate_instructions(&mut final_instructions);
    final_instructions
}

/// Legacy 2-`MoveChoice` entry point, kept so existing callers (io / mcts /
/// search) compile unchanged in a doubles build. Packages one move per side into
/// a single-slot `SideAction` and delegates to `generate_instructions_from_actions`.
#[cfg(feature = "doubles")]
pub fn generate_instructions_from_move_pair(
    state: &mut State,
    side_one_move: &MoveChoice,
    side_two_move: &MoveChoice,
    branch_on_damage: bool,
) -> Vec<StateInstructions> {
    let mut s1 = [MoveChoice::None; ACTIVE_PER_SIDE];
    let mut s2 = [MoveChoice::None; ACTIVE_PER_SIDE];
    s1[0] = *side_one_move;
    s2[0] = *side_two_move;
    generate_instructions_from_actions(
        state,
        &SideAction::new(s1),
        &SideAction::new(s2),
        branch_on_damage,
    )
}

pub fn calculate_damage_rolls(
    mut state: State,
    attacking_side_ref: &SideReference,
    mut choice: Choice,
    mut defending_choice: &Choice,
) -> Option<Vec<i16>> {
    let mut incoming_instructions = StateInstructions::default();

    if choice.flags.charge {
        choice.flags.charge = false;
    }
    if choice.move_id == Choices::FAKEOUT || choice.move_id == Choices::FIRSTIMPRESSION {
        state.get_side(attacking_side_ref).get_active().last_used_move = LastUsedMove::Switch(PokemonIndex::P0);
    }

    let attacker_active = state
        .get_side_immutable(attacking_side_ref)
        .get_active_immutable();
    let defender_active = state
        .get_side_immutable(&attacking_side_ref.get_other_side())
        .get_active_immutable();
    match choice.move_id {
        Choices::SEISMICTOSS => {
            if type_effectiveness_modifier(&PokemonType::NORMAL, &defender_active) == 0.0 {
                return None;
            }
            return Some(vec![attacker_active.level as i16]);
        }
        Choices::NIGHTSHADE => {
            if type_effectiveness_modifier(&PokemonType::GHOST, &defender_active) == 0.0 {
                return None;
            }
            return Some(vec![attacker_active.level as i16]);
        }
        Choices::FINALGAMBIT => {
            if type_effectiveness_modifier(&PokemonType::GHOST, &defender_active) == 0.0 {
                return None;
            }
            return Some(vec![attacker_active.hp]);
        }
        Choices::ENDEAVOR => {
            if type_effectiveness_modifier(&PokemonType::GHOST, &defender_active) == 0.0
                || defender_active.hp <= attacker_active.hp
            {
                return None;
            }
            return Some(vec![defender_active.hp - attacker_active.hp]);
        }
        Choices::PAINSPLIT => {
            if type_effectiveness_modifier(&PokemonType::GHOST, &defender_active) == 0.0
                || defender_active.hp <= attacker_active.hp
            {
                return None;
            }
            return Some(vec![
                defender_active.hp - (attacker_active.hp + defender_active.hp) / 2,
            ]);
        }
        Choices::SUPERFANG
            if type_effectiveness_modifier(&PokemonType::NORMAL, &defender_active) == 0.0 =>
        {
            return None;
        }
        Choices::SUPERFANG | Choices::NATURESMADNESS | Choices::RUINATION => {
            return Some(vec![defender_active.hp / 2]);
        }
        Choices::SUCKERPUNCH | Choices::THUNDERCLAP => {
            defending_choice = MOVES.get(&Choices::TACKLE).unwrap();
        }

        _ => {}
    }

    before_move(
        &mut state,
        &mut choice,
        defending_choice,
        attacking_side_ref,
        &mut incoming_instructions,
    );

    if choice.move_id == Choices::FUTURESIGHT {
        choice = MOVES.get(&Choices::FUTURESIGHT)?.clone();
    }

    let mut return_vec = Vec::with_capacity(4);
    if let Some((damage, crit_damage)) =
        calculate_damage(&state, attacking_side_ref, &choice, DamageRolls::Max)
    {
        return_vec.push(damage);
        return_vec.push(crit_damage);
        Some(return_vec)
    } else {
        None
    }
}

pub fn calculate_both_damage_rolls(
    state: &State,
    mut s1_choice: Choice,
    mut s2_choice: Choice,
    side_one_moves_first: bool,
) -> (Option<Vec<i16>>, Option<Vec<i16>>) {
    if side_one_moves_first {
        s1_choice.first_move = true;
        s2_choice.first_move = false;
    } else {
        s1_choice.first_move = false;
        s2_choice.first_move = true;
    }

    let damages_dealt_s1 = calculate_damage_rolls(
        state.clone(),
        &SideReference::SideOne,
        s1_choice.clone(),
        &s2_choice,
    );
    let damages_dealt_s2 = calculate_damage_rolls(
        state.clone(),
        &SideReference::SideTwo,
        s2_choice,
        &s1_choice,
    );

    (damages_dealt_s1, damages_dealt_s2)
}

#[cfg(test)]
mod tests {
    use super::super::abilities::Abilities;
    use super::super::state::{PokemonVolatileStatus, Terrain, Weather};
    use super::*;
    use crate::choices::{Choices, MOVES};
    use crate::instruction::{
        ApplyVolatileStatusInstruction, BoostInstruction, ChangeItemInstruction,
        ChangeStatusInstruction, ChangeSubsituteHealthInstruction, ChangeTerrain,
        DamageInstruction, EnableMoveInstruction, SwitchInstruction,
    };
    use crate::state::{
        Move, PokemonBoostableStat, PokemonIndex, PokemonMoveIndex, PokemonSideCondition,
        PokemonStatus, SideReference, State,
    };

    // FIXME(doubles): slots in test expectations are all 0 (singles-equivalent)

    #[test]
    fn test_drag_move_as_second_move_exits_early_if_opponent_used_drag_move() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::DRAGONTAIL).unwrap().to_owned();
        choice.first_move = false;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::DRAGONTAIL).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );
        assert_eq!(instructions, vec![StateInstructions::default()])
    }

    #[test]
    fn test_electric_move_does_nothing_versus_ground_type() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::THUNDERBOLT).unwrap().to_owned();
        state.side_two.get_active().types = (PokemonType::GROUND, PokemonType::TYPELESS);
        choice.first_move = false;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );
        assert_eq!(instructions, vec![StateInstructions::default()])
    }

    #[test]
    fn test_grass_type_cannot_have_powder_move_used_against_it() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::SPORE).unwrap().to_owned(); // Spore is a powder move
        state.side_two.get_active().types = (PokemonType::GRASS, PokemonType::TYPELESS);
        choice.first_move = false;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        #[cfg(any(feature = "gen6", feature = "gen7", feature = "gen8", feature = "gen9"))]
        let expected_instructions = vec![StateInstructions::default()];

        #[cfg(any(feature = "gen3", feature = "gen4", feature = "gen5"))]
        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::ChangeStatus(ChangeStatusInstruction {
                side_ref: SideReference::SideTwo,
                pokemon_index: PokemonIndex::P0,
                old_status: PokemonStatus::NONE,
                new_status: PokemonStatus::SLEEP,
            })],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_spikes_sets_first_layer() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::SPIKES).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::ChangeSideCondition(
                ChangeSideConditionInstruction {
                    side_ref: SideReference::SideTwo,
                    side_condition: PokemonSideCondition::Spikes,
                    amount: 1,
                },
            )],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_spikes_layers_cannot_exceed_3() {
        let mut state: State = State::default();
        state.side_two.side_conditions.spikes = 3;
        let mut choice = MOVES.get(&Choices::SPIKES).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_aurora_veil_works_in_hail() {
        let mut state: State = State::default();
        state.weather.weather_type = Weather::HAIL;
        let mut choice = MOVES.get(&Choices::AURORAVEIL).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::ChangeSideCondition(
                ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::AuroraVeil,
                    amount: 5,
                },
            )],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_auroa_veil_fails_outside_hail() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::AURORAVEIL).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_auroa_veil_fails_outside_of_hail() {
        let mut state: State = State::default();
        state.weather.weather_type = Weather::NONE;
        let mut choice = MOVES.get(&Choices::AURORAVEIL).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_stealthrock_cannot_exceed_1_layer() {
        let mut state: State = State::default();
        state.side_two.side_conditions.stealth_rock = 1;
        let mut choice = MOVES.get(&Choices::STEALTHROCK).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_stoneaxe_damage_and_stealthrock_setting() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::STONEAXE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 10.000002,
                instruction_list: vec![],
            },
            StateInstructions {
                percentage: 90.0,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        51,
                    )),
                    Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                        side_ref: SideReference::SideTwo,
                        side_condition: PokemonSideCondition::Stealthrock,
                        amount: 1,
                    }),
                ],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_ceaselessedge_damage_and_stealthrock_setting() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::CEASELESSEDGE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 10.000002,
                instruction_list: vec![],
            },
            StateInstructions {
                percentage: 90.0,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        51,
                    )),
                    Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                        side_ref: SideReference::SideTwo,
                        side_condition: PokemonSideCondition::Spikes,
                        amount: 1,
                    }),
                ],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_100_percent_secondary_volatilestatus() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::CHATTER).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    51,
                )),
                Instruction::ApplyVolatileStatus(ApplyVolatileStatusInstruction::new(
                    SideReference::SideTwo,
                    0,
                    PokemonVolatileStatus::CONFUSION,
                )),
            ],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_possible_secondary_volatilestatus() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::CONFUSION).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 90.0,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    40,
                ))],
            },
            StateInstructions {
                percentage: 10.0,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        40,
                    )),
                    Instruction::ApplyVolatileStatus(ApplyVolatileStatusInstruction::new(
                        SideReference::SideTwo,
                        0,
                        PokemonVolatileStatus::CONFUSION,
                    )),
                ],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_possible_secondary_volatilestatus_with_possible_accuracy() {
        let mut state: State = State::default();
        state.side_two.get_active().hp = 400;
        state.side_two.get_active().maxhp = 400;
        let mut choice = MOVES.get(&Choices::AXEKICK).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 10.000002,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    50, // This move has recoil lol
                ))],
            },
            StateInstructions {
                percentage: 63.0,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    188,
                ))],
            },
            StateInstructions {
                percentage: 27.0000019,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        188,
                    )),
                    Instruction::ApplyVolatileStatus(ApplyVolatileStatusInstruction::new(
                        SideReference::SideTwo,
                        0,
                        PokemonVolatileStatus::CONFUSION,
                    )),
                ],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_basic_volatile_status_applied_to_self() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::AQUARING).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::ApplyVolatileStatus(
                ApplyVolatileStatusInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonVolatileStatus::AQUARING,
                ),
            )],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_basic_volatile_status_applied_to_opponent() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::ATTRACT).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::ApplyVolatileStatus(
                ApplyVolatileStatusInstruction::new(
                    SideReference::SideTwo,
                    0,
                    PokemonVolatileStatus::ATTRACT,
                ),
            )],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_cannot_apply_volatile_status_twice() {
        let mut state: State = State::default();
        state
            .side_two
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::ATTRACT);
        let mut choice = MOVES.get(&Choices::ATTRACT).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_substitute_failing_if_user_has_less_than_25_percent_hp() {
        let mut state: State = State::default();
        state.side_one.get_active().hp = 25;
        let mut choice = MOVES.get(&Choices::SUBSTITUTE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_shedtail_failing_if_user_has_less_than_50_percent_hp() {
        let mut state: State = State::default();
        state.side_one.get_active().hp = 50;
        let mut choice = MOVES.get(&Choices::SHEDTAIL).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_basic_drag_move() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::WHIRLWIND).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 20.0,
                instruction_list: vec![Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideTwo,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                })],
            },
            StateInstructions {
                percentage: 20.0,
                instruction_list: vec![Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideTwo,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P2,
                })],
            },
            StateInstructions {
                percentage: 20.0,
                instruction_list: vec![Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideTwo,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P3,
                })],
            },
            StateInstructions {
                percentage: 20.0,
                instruction_list: vec![Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideTwo,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P4,
                })],
            },
            StateInstructions {
                percentage: 20.0,
                instruction_list: vec![Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideTwo,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P5,
                })],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_basic_drag_move_with_fainted_reserve() {
        let mut state: State = State::default();
        state.side_two.pokemon[PokemonIndex::P1].hp = 0;
        state.side_two.pokemon[PokemonIndex::P3].hp = 0;
        let mut choice = MOVES.get(&Choices::WHIRLWIND).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 33.333336,
                instruction_list: vec![Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideTwo,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P2,
                })],
            },
            StateInstructions {
                percentage: 33.333336,
                instruction_list: vec![Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideTwo,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P4,
                })],
            },
            StateInstructions {
                percentage: 33.333336,
                instruction_list: vec![Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideTwo,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P5,
                })],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_basic_damaging_drag_move_with_fainted_reserve() {
        let mut state: State = State::default();
        state.side_two.pokemon[PokemonIndex::P1].hp = 0;
        state.side_two.pokemon[PokemonIndex::P3].hp = 0;
        let mut choice = MOVES.get(&Choices::DRAGONTAIL).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 10.0000019,
                instruction_list: vec![], // The move missed
            },
            StateInstructions {
                percentage: 30.0,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        48,
                    )),
                    Instruction::Switch(SwitchInstruction {
                        side_ref: SideReference::SideTwo,
                        previous_index: PokemonIndex::P0,
                        next_index: PokemonIndex::P2,
                    }),
                ],
            },
            StateInstructions {
                percentage: 30.0,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        48,
                    )),
                    Instruction::Switch(SwitchInstruction {
                        side_ref: SideReference::SideTwo,
                        previous_index: PokemonIndex::P0,
                        next_index: PokemonIndex::P4,
                    }),
                ],
            },
            StateInstructions {
                percentage: 30.0,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        48,
                    )),
                    Instruction::Switch(SwitchInstruction {
                        side_ref: SideReference::SideTwo,
                        previous_index: PokemonIndex::P0,
                        next_index: PokemonIndex::P5,
                    }),
                ],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_basic_damaging_drag_that_knocks_out_defender() {
        let mut state: State = State::default();
        state.side_two.pokemon[PokemonIndex::P1].hp = 0;
        state.side_two.pokemon[PokemonIndex::P3].hp = 0;
        state.side_two.get_active().hp = 5;
        let mut choice = MOVES.get(&Choices::DRAGONTAIL).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 10.0000019,
                instruction_list: vec![], // The move missed
            },
            StateInstructions {
                percentage: 90.0,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    5,
                ))],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_drag_versus_no_alive_reserved() {
        let mut state: State = State::default();
        state.side_two.pokemon[PokemonIndex::P1].hp = 0;
        state.side_two.pokemon[PokemonIndex::P2].hp = 0;
        state.side_two.pokemon[PokemonIndex::P3].hp = 0;
        state.side_two.pokemon[PokemonIndex::P4].hp = 0;
        state.side_two.pokemon[PokemonIndex::P5].hp = 0;
        let mut choice = MOVES.get(&Choices::WHIRLWIND).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_basic_drag_move_with_fainted_reserve_and_prior_instruction() {
        let mut state: State = State::default();
        state.side_two.pokemon[PokemonIndex::P1].hp = 0;
        state.side_two.pokemon[PokemonIndex::P3].hp = 0;
        let mut choice = MOVES.get(&Choices::WHIRLWIND).unwrap().to_owned();

        let previous_instruction = StateInstructions {
            percentage: 50.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                5,
            ))],
        };

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            previous_instruction,
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 16.666668,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideOne,
                        0,
                        5,
                    )),
                    Instruction::Switch(SwitchInstruction {
                        side_ref: SideReference::SideTwo,
                        previous_index: PokemonIndex::P0,
                        next_index: PokemonIndex::P2,
                    }),
                ],
            },
            StateInstructions {
                percentage: 16.666668,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideOne,
                        0,
                        5,
                    )),
                    Instruction::Switch(SwitchInstruction {
                        side_ref: SideReference::SideTwo,
                        previous_index: PokemonIndex::P0,
                        next_index: PokemonIndex::P4,
                    }),
                ],
            },
            StateInstructions {
                percentage: 16.666668,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideOne,
                        0,
                        5,
                    )),
                    Instruction::Switch(SwitchInstruction {
                        side_ref: SideReference::SideTwo,
                        previous_index: PokemonIndex::P0,
                        next_index: PokemonIndex::P5,
                    }),
                ],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    #[cfg(feature = "gen9")]
    fn test_basic_status_move() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::GLARE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::ChangeStatus(ChangeStatusInstruction {
                side_ref: SideReference::SideTwo,
                pokemon_index: PokemonIndex::P0,
                old_status: PokemonStatus::NONE,
                new_status: PokemonStatus::PARALYZE,
            })],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    #[cfg(feature = "gen9")]
    fn test_status_move_that_can_miss() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::THUNDERWAVE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 10.000002,
                instruction_list: vec![],
            },
            StateInstructions {
                percentage: 90.0,
                instruction_list: vec![Instruction::ChangeStatus(ChangeStatusInstruction {
                    side_ref: SideReference::SideTwo,
                    pokemon_index: PokemonIndex::P0,
                    old_status: PokemonStatus::NONE,
                    new_status: PokemonStatus::PARALYZE,
                })],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_status_move_that_can_miss_but_is_blocked_by_ability() {
        let mut state: State = State::default();
        state.side_two.get_active().ability = Abilities::LIMBER;
        let mut choice = MOVES.get(&Choices::THUNDERWAVE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_flamebody_conditional_burn_on_contact() {
        let mut state: State = State::default();
        state.side_two.get_active().ability = Abilities::FLAMEBODY;
        let mut choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 70.0,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    48,
                ))],
            },
            StateInstructions {
                percentage: 30.0000019,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        48,
                    )),
                    Instruction::ChangeStatus(ChangeStatusInstruction {
                        side_ref: SideReference::SideOne,
                        pokemon_index: PokemonIndex::P0,
                        old_status: PokemonStatus::NONE,
                        new_status: PokemonStatus::BURN,
                    }),
                ],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_protectivepads_stops_flamebody() {
        let mut state: State = State::default();
        state.side_two.get_active().ability = Abilities::FLAMEBODY;
        state.side_one.get_active().item = Items::PROTECTIVEPADS;
        let mut choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideTwo,
                0,
                48,
            ))],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_flamebody_versus_noncontact_move() {
        let mut state: State = State::default();
        state.side_two.get_active().ability = Abilities::FLAMEBODY;
        let mut choice = MOVES.get(&Choices::WATERGUN).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideTwo,
                0,
                32,
            ))],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_flamebody_versus_fire_type() {
        let mut state: State = State::default();
        state.side_one.get_active().types.0 = PokemonType::FIRE;
        state.side_two.get_active().ability = Abilities::FLAMEBODY;
        let mut choice = MOVES.get(&Choices::WATERGUN).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideTwo,
                0,
                32,
            ))],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_move_with_multiple_secondaries() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::FIREFANG).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 5.00000095,
                instruction_list: vec![],
            },
            StateInstructions {
                percentage: 76.9499969,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    51,
                ))],
            },
            StateInstructions {
                percentage: 8.55000019,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        51,
                    )),
                    Instruction::ApplyVolatileStatus(ApplyVolatileStatusInstruction::new(
                        SideReference::SideTwo,
                        0,
                        PokemonVolatileStatus::FLINCH,
                    )),
                ],
            },
            StateInstructions {
                percentage: 8.55000019,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        51,
                    )),
                    Instruction::ChangeStatus(ChangeStatusInstruction {
                        side_ref: SideReference::SideTwo,
                        pokemon_index: PokemonIndex::P0,
                        old_status: PokemonStatus::NONE,
                        new_status: PokemonStatus::BURN,
                    }),
                ],
            },
            StateInstructions {
                percentage: 0.949999988,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        51,
                    )),
                    Instruction::ChangeStatus(ChangeStatusInstruction {
                        side_ref: SideReference::SideTwo,
                        pokemon_index: PokemonIndex::P0,
                        old_status: PokemonStatus::NONE,
                        new_status: PokemonStatus::BURN,
                    }),
                    Instruction::ApplyVolatileStatus(ApplyVolatileStatusInstruction::new(
                        SideReference::SideTwo,
                        0,
                        PokemonVolatileStatus::FLINCH,
                    )),
                ],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_flamebody() {
        let mut state: State = State::default();
        state.side_two.get_active().ability = Abilities::FLAMEBODY;
        let mut choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 70.0,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    48,
                ))],
            },
            StateInstructions {
                percentage: 30.000002,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        48,
                    )),
                    Instruction::ChangeStatus(ChangeStatusInstruction {
                        side_ref: SideReference::SideOne,
                        pokemon_index: PokemonIndex::P0,
                        old_status: PokemonStatus::NONE,
                        new_status: PokemonStatus::BURN,
                    }),
                ],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_flamebody_creating_a_move_with_multiple_secondaries() {
        let mut state: State = State::default();
        state.side_two.get_active().ability = Abilities::FLAMEBODY;
        let mut choice = MOVES.get(&Choices::FIREPUNCH).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 63.0,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    60,
                ))],
            },
            StateInstructions {
                percentage: 27.0000019,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        60,
                    )),
                    Instruction::ChangeStatus(ChangeStatusInstruction {
                        side_ref: SideReference::SideOne,
                        pokemon_index: PokemonIndex::P0,
                        old_status: PokemonStatus::NONE,
                        new_status: PokemonStatus::BURN,
                    }),
                ],
            },
            StateInstructions {
                percentage: 7.0,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        60,
                    )),
                    Instruction::ChangeStatus(ChangeStatusInstruction {
                        side_ref: SideReference::SideTwo,
                        pokemon_index: PokemonIndex::P0,
                        old_status: PokemonStatus::NONE,
                        new_status: PokemonStatus::BURN,
                    }),
                ],
            },
            StateInstructions {
                percentage: 3.0,
                instruction_list: vec![
                    Instruction::Damage(DamageInstruction::new(
                        SideReference::SideTwo,
                        0,
                        60,
                    )),
                    Instruction::ChangeStatus(ChangeStatusInstruction {
                        side_ref: SideReference::SideTwo,
                        pokemon_index: PokemonIndex::P0,
                        old_status: PokemonStatus::NONE,
                        new_status: PokemonStatus::BURN,
                    }),
                    Instruction::ChangeStatus(ChangeStatusInstruction {
                        side_ref: SideReference::SideOne,
                        pokemon_index: PokemonIndex::P0,
                        old_status: PokemonStatus::NONE,
                        new_status: PokemonStatus::BURN,
                    }),
                ],
            },
        ];
        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_substitute_does_not_block_rest() {
        let mut state: State = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::SUBSTITUTE);
        state.side_one.get_active().hp = state.side_one.get_active().maxhp - 1;
        let mut choice = MOVES.get(&Choices::REST).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::ChangeStatus(ChangeStatusInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: PokemonIndex::P0,
                    old_status: PokemonStatus::NONE,
                    new_status: PokemonStatus::SLEEP,
                }),
                Instruction::SetRestTurns(SetSleepTurnsInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: PokemonIndex::P0,
                    new_turns: 3,
                    previous_turns: 0,
                }),
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideOne,
                    0,
                    1,
                )),
            ],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_basic_heal_move() {
        let mut state: State = State::default();
        state.side_one.get_active().hp = 1;
        let mut choice = MOVES.get(&Choices::RECOVER).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Heal(HealInstruction::new(
                SideReference::SideOne,
                0,
                50,
            ))],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_heal_move_generates_no_instruction_at_maxhp() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::RECOVER).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_basic_negative_heal_move() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::EXPLOSION).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    100,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    100,
                )),
            ],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_negative_heal_move_does_not_overkill() {
        let mut state: State = State::default();
        state.side_one.get_active().hp = 1;
        let mut choice = MOVES.get(&Choices::EXPLOSION).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    1,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    100,
                )),
            ],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_heal_move_does_not_overheal() {
        let mut state: State = State::default();
        state.side_one.get_active().hp = 55;
        let mut choice = MOVES.get(&Choices::RECOVER).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Heal(HealInstruction::new(
                SideReference::SideOne,
                0,
                45,
            ))],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_basic_boosting_move() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::SWORDSDANCE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Boost(BoostInstruction::new(
                SideReference::SideOne,
                0,
                PokemonBoostableStat::Attack,
                2,
            ))],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_does_not_overboost() {
        let mut state: State = State::default();
        state.side_one.get_active().attack_boost = 5;
        let mut choice = MOVES.get(&Choices::SWORDSDANCE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Boost(BoostInstruction::new(
                SideReference::SideOne,
                0,
                PokemonBoostableStat::Attack,
                1,
            ))],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_no_instruction_when_boosting_at_max() {
        let mut state: State = State::default();
        state.side_one.get_active().attack_boost = 6;
        let mut choice = MOVES.get(&Choices::SWORDSDANCE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_boost_lowering_that_can_miss() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::KINESIS).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 19.999998,
                instruction_list: vec![],
            },
            StateInstructions {
                percentage: 80.0,
                instruction_list: vec![Instruction::Boost(BoostInstruction::new(
                    SideReference::SideTwo,
                    0,
                    PokemonBoostableStat::Accuracy,
                    -1,
                ))],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_basic_boost_lowering() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::CHARM).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Boost(BoostInstruction::new(
                SideReference::SideTwo,
                0,
                PokemonBoostableStat::Attack,
                -2,
            ))],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_cannot_boost_lower_than_negative_6() {
        let mut state: State = State::default();
        state.side_two.get_active().attack_boost = -5;
        let mut choice = MOVES.get(&Choices::CHARM).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Boost(BoostInstruction::new(
                SideReference::SideTwo,
                0,
                PokemonBoostableStat::Attack,
                -1,
            ))],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_no_boost_when_already_at_minimum() {
        let mut state: State = State::default();
        state.side_two.get_active().attack_boost = -6;
        let mut choice = MOVES.get(&Choices::CHARM).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_clearbody_blocks_stat_lowering() {
        let mut state: State = State::default();
        state.side_two.get_active().ability = Abilities::CLEARBODY;
        let mut choice = MOVES.get(&Choices::CHARM).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_clearbody_does_not_block_self_stat_lowering() {
        let mut state: State = State::default();
        state.side_one.get_active().ability = Abilities::CLEARBODY;
        let mut choice = MOVES.get(&Choices::SHELLSMASH).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Attack,
                    2,
                )),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Defense,
                    -1,
                )),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::SpecialAttack,
                    2,
                )),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::SpecialDefense,
                    -1,
                )),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Speed,
                    2,
                )),
            ],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_defog_does_not_change_terrain_if_terrain_is_none() {
        let mut state: State = State::default();

        let mut choice = MOVES.get(&Choices::DEFOG).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_defog_clears_terrain() {
        let mut state: State = State::default();
        state.terrain.terrain_type = Terrain::ELECTRICTERRAIN;
        state.terrain.turns_remaining = 1;

        let mut choice = MOVES.get(&Choices::DEFOG).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::ChangeTerrain(ChangeTerrain {
                new_terrain: Terrain::NONE,
                new_terrain_turns_remaining: 0,
                previous_terrain: Terrain::ELECTRICTERRAIN,
                previous_terrain_turns_remaining: 1,
            })],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_defog_clears_terrain_and_side_conditions() {
        let mut state: State = State::default();
        state.terrain.terrain_type = Terrain::ELECTRICTERRAIN;
        state.terrain.turns_remaining = 1;
        state.side_one.side_conditions.reflect = 1;
        state.side_two.side_conditions.reflect = 1;

        let mut choice = MOVES.get(&Choices::DEFOG).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::ChangeTerrain(ChangeTerrain {
                    new_terrain: Terrain::NONE,
                    new_terrain_turns_remaining: 0,
                    previous_terrain: Terrain::ELECTRICTERRAIN,
                    previous_terrain_turns_remaining: 1,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::Reflect,
                    amount: -1,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideTwo,
                    side_condition: PokemonSideCondition::Reflect,
                    amount: -1,
                }),
            ],
        }];
        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_tidyup_clears_side_conditions_and_substitutes() {
        let mut state: State = State::default();
        state.terrain.terrain_type = Terrain::ELECTRICTERRAIN;
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::SUBSTITUTE);
        state
            .side_two
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::SUBSTITUTE);
        state.side_one.get_active().substitute_health = 10;
        state.side_two.get_active().substitute_health = 25;
        state.terrain.turns_remaining = 1;
        state.side_one.side_conditions.spikes = 2;
        state.side_two.side_conditions.stealth_rock = 1;

        let mut choice = MOVES.get(&Choices::TIDYUP).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::Spikes,
                    amount: -2,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideTwo,
                    side_condition: PokemonSideCondition::Stealthrock,
                    amount: -1,
                }),
                Instruction::ChangeSubstituteHealth(ChangeSubsituteHealthInstruction::new(
                    SideReference::SideOne,
                    0,
                    -10,
                )),
                Instruction::RemoveVolatileStatus(RemoveVolatileStatusInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonVolatileStatus::SUBSTITUTE,
                )),
                Instruction::ChangeSubstituteHealth(ChangeSubsituteHealthInstruction::new(
                    SideReference::SideTwo,
                    0,
                    -25,
                )),
                Instruction::RemoveVolatileStatus(RemoveVolatileStatusInstruction::new(
                    SideReference::SideTwo,
                    0,
                    PokemonVolatileStatus::SUBSTITUTE,
                )),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Attack,
                    1,
                )),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Speed,
                    1,
                )),
            ],
        }];
        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    #[cfg(any(feature = "gen8", feature = "gen9"))]
    fn test_rapidspin_clears_hazards() {
        let mut state: State = State::default();
        state.side_one.side_conditions.stealth_rock = 1;

        let mut choice = MOVES.get(&Choices::RAPIDSPIN).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    61,
                )),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::Stealthrock,
                    amount: -1,
                }),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Speed,
                    1,
                )),
            ],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_missing_rapidspin_does_not_clear_hazards() {
        let mut state: State = State::default();
        state.side_two.get_active().types = (PokemonType::GHOST, PokemonType::NORMAL);
        state.side_one.side_conditions.stealth_rock = 1;

        let mut choice = MOVES.get(&Choices::RAPIDSPIN).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];
        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_acid_into_steel_type() {
        let mut state: State = State::default();
        state.side_two.get_active().types = (PokemonType::STEEL, PokemonType::NORMAL);

        let mut choice = MOVES.get(&Choices::ACID).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        }];
        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    #[cfg(any(feature = "gen8", feature = "gen9"))]
    fn test_rapidspin_clears_multiple_hazards() {
        let mut state: State = State::default();
        state.side_one.side_conditions.stealth_rock = 1;
        state.side_one.side_conditions.toxic_spikes = 2;
        state.side_one.side_conditions.spikes = 3;
        state.side_one.side_conditions.sticky_web = 1;

        let mut choice = MOVES.get(&Choices::RAPIDSPIN).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    61,
                )),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::Stealthrock,
                    amount: -1,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::Spikes,
                    amount: -3,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::ToxicSpikes,
                    amount: -2,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::StickyWeb,
                    amount: -1,
                }),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Speed,
                    1,
                )),
            ],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    #[cfg(any(feature = "gen8", feature = "gen9"))]
    fn test_rapidspin_does_not_clear_opponent_hazards() {
        let mut state: State = State::default();
        state.side_two.side_conditions.stealth_rock = 1;
        state.side_two.side_conditions.toxic_spikes = 2;
        state.side_two.side_conditions.spikes = 3;
        state.side_two.side_conditions.sticky_web = 1;

        let mut choice = MOVES.get(&Choices::RAPIDSPIN).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    61,
                )),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Speed,
                    1,
                )),
            ],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_courtchange_basic_swap() {
        let mut state: State = State::default();
        state.side_one.side_conditions.stealth_rock = 1;

        let mut choice = MOVES.get(&Choices::COURTCHANGE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::Stealthrock,
                    amount: -1,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideTwo,
                    side_condition: PokemonSideCondition::Stealthrock,
                    amount: 1,
                }),
            ],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_courtchange_complicated_swap() {
        let mut state: State = State::default();
        state.side_one.side_conditions.stealth_rock = 1;
        state.side_two.side_conditions.toxic_spikes = 2;
        state.side_two.side_conditions.spikes = 3;
        state.side_two.side_conditions.sticky_web = 1;

        let mut choice = MOVES.get(&Choices::COURTCHANGE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::Stealthrock,
                    amount: -1,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideTwo,
                    side_condition: PokemonSideCondition::Stealthrock,
                    amount: 1,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideTwo,
                    side_condition: PokemonSideCondition::Spikes,
                    amount: -3,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::Spikes,
                    amount: 3,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideTwo,
                    side_condition: PokemonSideCondition::ToxicSpikes,
                    amount: -2,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::ToxicSpikes,
                    amount: 2,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideTwo,
                    side_condition: PokemonSideCondition::StickyWeb,
                    amount: -1,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::StickyWeb,
                    amount: 1,
                }),
            ],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_stoneaxe_does_not_set_stealthrock_if_already_set() {
        let mut state: State = State::default();
        state.side_two.side_conditions.stealth_rock = 1;
        let mut choice = MOVES.get(&Choices::STONEAXE).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions = vec![
            StateInstructions {
                percentage: 10.000002,
                instruction_list: vec![],
            },
            StateInstructions {
                percentage: 90.0,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    51,
                ))],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_flinched_pokemon_cannot_move() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::FLINCH);

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );
        assert_eq!(instructions, vec![StateInstructions::default()])
    }

    #[test]
    fn test_dead_pokemon_moving_second_does_nothing() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        choice.first_move = false;
        state.side_one.get_active().hp = 0;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );
        assert_eq!(instructions, vec![StateInstructions::default()])
    }

    #[test]
    #[cfg(not(feature = "doubles"))] // EARTHQUAKE is a multi-target spread move in doubles
    fn test_cannot_ohko_versus_study() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::EARTHQUAKE).unwrap().to_owned();
        state.side_two.get_active().ability = Abilities::STURDY;
        state.side_two.get_active().hp = 50;
        state.side_two.get_active().maxhp = 50;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideTwo,
                0,
                49,
            ))],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    #[cfg(not(feature = "doubles"))] // EARTHQUAKE is a multi-target spread move in doubles
    fn test_cannot_ohko_versus_sash() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::EARTHQUAKE).unwrap().to_owned();
        state.side_two.get_active().item = Items::FOCUSSASH;
        state.side_two.get_active().hp = 50;
        state.side_two.get_active().maxhp = 50;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideTwo,
                0,
                49,
            ))],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    #[cfg(not(feature = "doubles"))] // EARTHQUAKE is a multi-target spread move in doubles
    fn test_sturdy_does_not_affect_non_ohko_move() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::EARTHQUAKE).unwrap().to_owned();
        state.side_two.get_active().ability = Abilities::STURDY;
        state.side_two.get_active().hp = 45;
        state.side_two.get_active().maxhp = 50;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideTwo,
                0,
                45,
            ))],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_beastboost_boosts_on_kill() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state.side_one.get_active().ability = Abilities::BEASTBOOST;
        state.side_one.get_active().attack = 500; // highest stat
        state.side_two.get_active().hp = 1;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    1,
                )),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Attack,
                    1,
                )),
            ],
        };
        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_beastboost_boosts_different_stat_on_kill() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state.side_one.get_active().ability = Abilities::BEASTBOOST;
        state.side_one.get_active().defense = 500; // highest stat
        state.side_two.get_active().hp = 1;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    1,
                )),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Defense,
                    1,
                )),
            ],
        };
        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_beastboost_does_not_overboost() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state.side_one.get_active().ability = Abilities::BEASTBOOST;
        state.side_one.get_active().attack = 500; // highest stat
        state.side_one.get_active().attack_boost = 6; // max boosts already
        state.side_two.get_active().hp = 1;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideTwo,
                0,
                1,
            ))],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_beastboost_does_not_boost_without_kill() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state.side_one.get_active().ability = Abilities::BEASTBOOST;
        state.side_one.get_active().attack = 150; // highest stat
        state.side_two.get_active().hp = 100;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideTwo,
                0,
                72,
            ))],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_drain_move_heals() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::ABSORB).unwrap().to_owned();
        state.side_one.get_active().hp = 100;
        state.side_one.get_active().maxhp = 200;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    16,
                )),
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideOne,
                    0,
                    8,
                )),
            ],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_drain_move_does_not_overheal() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::ABSORB).unwrap().to_owned();
        state.side_one.get_active().hp = 100;
        state.side_one.get_active().maxhp = 105;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    16,
                )),
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideOne,
                    0,
                    5,
                )),
            ],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_recoil_damage() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::BRAVEBIRD).unwrap().to_owned();
        state.side_one.get_active().hp = 105;
        state.side_one.get_active().maxhp = 105;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    94,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    31,
                )),
            ],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_recoil_cannot_overkill() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::BRAVEBIRD).unwrap().to_owned();
        state.side_one.get_active().hp = 5;
        state.side_one.get_active().maxhp = 105;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    94,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    5,
                )),
            ],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_drain_and_recoil_together() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::ABSORB).unwrap().to_owned();
        choice.recoil = Some(0.33);
        state.side_one.get_active().hp = 1;
        state.side_one.get_active().maxhp = 105;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    16,
                )),
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideOne,
                    0,
                    8,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    5,
                )),
            ],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_crash_move_missing() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::JUMPKICK).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: Vec<StateInstructions> = vec![
            StateInstructions {
                percentage: 5.000001,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    50,
                ))],
            },
            StateInstructions {
                percentage: 95.0,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    100,
                ))],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_crash_move_missing_versus_ghost_type() {
        let mut state: State = State::default();
        state.side_two.get_active().types.0 = PokemonType::GHOST;
        let mut choice = MOVES.get(&Choices::JUMPKICK).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: Vec<StateInstructions> = vec![StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                50,
            ))],
        }];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    fn test_crash_move_missing_cannot_overkill() {
        let mut state: State = State::default();
        state.get_side(&SideReference::SideOne).get_active().hp = 5;
        let mut choice = MOVES.get(&Choices::JUMPKICK).unwrap().to_owned();

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: Vec<StateInstructions> = vec![
            StateInstructions {
                percentage: 5.000001,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    5,
                ))],
            },
            StateInstructions {
                percentage: 95.0,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    100,
                ))],
            },
        ];

        assert_eq!(instructions, expected_instructions)
    }

    #[test]
    #[cfg(feature = "gen9")]
    fn test_knockoff_removing_item() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::KNOCKOFF).unwrap().to_owned();
        state.get_side(&SideReference::SideTwo).get_active().item = Items::HEAVYDUTYBOOTS;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    76,
                )),
                Instruction::ChangeItem(ChangeItemInstruction::new(
                    SideReference::SideTwo,
                    0,
                    Items::HEAVYDUTYBOOTS,
                    Items::NONE,
                )),
            ],
        };

        assert_eq!(instructions, vec![expected_instructions])
    }

    #[test]
    fn test_blunderpolicy_boost() {
        let mut state: State = State::default();
        let mut choice = MOVES.get(&Choices::CROSSCHOP).unwrap().to_owned();
        state.get_side(&SideReference::SideOne).get_active().item = Items::BLUNDERPOLICY;

        let mut instructions = vec![];
        generate_instructions_from_move(
            &mut state,
            &mut choice,
            &MOVES.get(&Choices::TACKLE).unwrap(),
            SideReference::SideOne,
            StateInstructions::default(),
            &mut instructions,
            false,
        );

        let expected_instructions: Vec<StateInstructions> = vec![
            StateInstructions {
                percentage: 19.999998,
                instruction_list: vec![
                    Instruction::Boost(BoostInstruction::new(
                        SideReference::SideOne,
                        0,
                        PokemonBoostableStat::Speed,
                        2,
                    )),
                    Instruction::ChangeItem(ChangeItemInstruction::new(
                        SideReference::SideOne,
                        0,
                        Items::BLUNDERPOLICY,
                        Items::NONE,
                    )),
                ],
            },
            StateInstructions {
                percentage: 80.0,
                instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    100,
                ))],
            },
        ];

        assert_eq!(instructions, expected_instructions);
    }

    #[test]
    fn test_basic_switch_functionality_with_no_prior_instructions() {
        let mut state: State = State::default();
        let mut choice = Choice {
            ..Default::default()
        };

        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Switch(SwitchInstruction {
                side_ref: SideReference::SideOne,
                previous_index: PokemonIndex::P0,
                next_index: PokemonIndex::P1,
            })],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_basic_switch_with_volatile_statuses() {
        let mut state: State = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::LEECHSEED);
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::RemoveVolatileStatus(RemoveVolatileStatusInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonVolatileStatus::LEECHSEED,
                )),
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_basic_switch_with_toxic_count() {
        let mut state: State = State::default();
        state.side_one.side_conditions.toxic_count = 2;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::ToxicCount,
                    amount: -2,
                }),
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_basic_switch_with_boost() {
        let mut state: State = State::default();
        state.side_one.get_active().attack_boost = 2;
        state.side_one.get_active().speed_boost = 5;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Attack,
                    -2,
                )),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Speed,
                    -5,
                )),
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_basic_switch_with_disabled_move() {
        let mut state: State = State::default();
        state.side_one.get_active().moves.m0 = Move {
            id: Choices::NONE,
            disabled: true,
            pp: 32,
            ..Default::default()
        };
        state.side_one.get_active().moves.m1 = Move {
            id: Choices::NONE,
            disabled: false,
            pp: 32,
            ..Default::default()
        };

        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::EnableMove(EnableMoveInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonMoveIndex::M0,
                )),
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_basic_switch_with_multiple_disabled_moves() {
        let mut state: State = State::default();
        state.side_one.get_active().moves.m0 = Move {
            id: Choices::NONE,
            disabled: true,
            pp: 32,
            ..Default::default()
        };
        state.side_one.get_active().moves.m1 = Move {
            id: Choices::NONE,
            disabled: true,
            pp: 32,
            ..Default::default()
        };
        state.side_one.get_active().moves.m2 = Move {
            id: Choices::NONE,
            disabled: false,
            pp: 32,
            ..Default::default()
        };
        state.side_one.get_active().moves.m3 = Move {
            id: Choices::NONE,
            disabled: true,
            pp: 32,
            ..Default::default()
        };
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::EnableMove(EnableMoveInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonMoveIndex::M0,
                )),
                Instruction::EnableMove(EnableMoveInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonMoveIndex::M1,
                )),
                Instruction::EnableMove(EnableMoveInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonMoveIndex::M3,
                )),
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_basic_switch_functionality_with_a_prior_instruction() {
        let mut state: State = State::default();
        let mut incoming_instructions = StateInstructions::default();
        let mut choice = Choice {
            ..Default::default()
        };

        choice.switch_id = PokemonIndex::P1;
        incoming_instructions
            .instruction_list
            .push(Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                1,
            )));

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    1,
                )),
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
            ],
            ..Default::default()
        };

        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switch_with_regenerator() {
        let mut state: State = State::default();
        state.side_one.get_active().hp -= 10;
        state.side_one.get_active().ability = Abilities::REGENERATOR;
        state.side_one.get_active().base_ability = Abilities::REGENERATOR;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideOne,
                    0,
                    10,
                )),
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switch_with_regenerator_plus_move_enabling() {
        let mut state: State = State::default();
        state.side_one.get_active().moves.m0 = Move {
            id: Choices::NONE,
            disabled: true,
            pp: 32,
            ..Default::default()
        };
        state.side_one.get_active().moves.m1 = Move {
            id: Choices::NONE,
            disabled: true,
            pp: 32,
            ..Default::default()
        };
        state.side_one.get_active().moves.m2 = Move {
            id: Choices::NONE,
            disabled: false,
            pp: 32,
            ..Default::default()
        };
        state.side_one.get_active().moves.m3 = Move {
            id: Choices::NONE,
            disabled: true,
            pp: 32,
            ..Default::default()
        };
        state.side_one.get_active().hp -= 10;
        state.side_one.get_active().ability = Abilities::REGENERATOR;
        state.side_one.get_active().base_ability = Abilities::REGENERATOR;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::EnableMove(EnableMoveInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonMoveIndex::M0,
                )),
                Instruction::EnableMove(EnableMoveInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonMoveIndex::M1,
                )),
                Instruction::EnableMove(EnableMoveInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonMoveIndex::M3,
                )),
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideOne,
                    0,
                    10,
                )),
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switch_with_regenerator_but_no_damage_taken() {
        let mut state: State = State::default();
        state.side_one.get_active().ability = Abilities::REGENERATOR;
        state.side_one.get_active().base_ability = Abilities::REGENERATOR;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Switch(SwitchInstruction {
                side_ref: SideReference::SideOne,
                previous_index: PokemonIndex::P0,
                next_index: PokemonIndex::P1,
            })],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_fainted_pokemon_with_regenerator_does_not_heal() {
        let mut state: State = State::default();
        state.side_one.get_active().ability = Abilities::REGENERATOR;
        state.side_one.get_active().base_ability = Abilities::REGENERATOR;
        state.side_one.get_active().hp = 0;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Switch(SwitchInstruction {
                side_ref: SideReference::SideOne,
                previous_index: PokemonIndex::P0,
                next_index: PokemonIndex::P1,
            })],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_regenerator_only_heals_one_third() {
        let mut state: State = State::default();
        state.side_one.get_active().ability = Abilities::REGENERATOR;
        state.side_one.get_active().base_ability = Abilities::REGENERATOR;
        state.side_one.get_active().hp = 3;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideOne,
                    0,
                    33,
                )),
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_naturalcure() {
        let mut state: State = State::default();
        state.side_one.get_active().ability = Abilities::NATURALCURE;
        state.side_one.get_active().base_ability = Abilities::NATURALCURE;
        state.side_one.get_active().status = PokemonStatus::PARALYZE;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::ChangeStatus(ChangeStatusInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: PokemonIndex::P0,
                    old_status: PokemonStatus::PARALYZE,
                    new_status: PokemonStatus::NONE,
                }),
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_naturalcure_with_no_status() {
        let mut state: State = State::default();
        state.side_one.get_active().ability = Abilities::NATURALCURE;
        state.side_one.get_active().base_ability = Abilities::NATURALCURE;
        state.side_one.get_active().status = PokemonStatus::NONE;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Switch(SwitchInstruction {
                side_ref: SideReference::SideOne,
                previous_index: PokemonIndex::P0,
                next_index: PokemonIndex::P1,
            })],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_stealthrock() {
        let mut state: State = State::default();
        state.side_one.side_conditions.stealth_rock = 1;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    state.side_one.get_active().hp / 8,
                )),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_resisted_stealthrock() {
        let mut state: State = State::default();
        state.side_one.side_conditions.stealth_rock = 1;
        state.side_one.pokemon[PokemonIndex::P1].types = (PokemonType::GROUND, PokemonType::NORMAL);
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    state.side_one.get_active().hp / 16,
                )),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_stealthrock_does_not_overkill() {
        let mut state: State = State::default();
        state.side_one.side_conditions.stealth_rock = 1;
        state.side_one.pokemon[PokemonIndex::P1].hp = 5;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    5,
                )),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_stickyweb() {
        let mut state: State = State::default();
        state.side_one.side_conditions.sticky_web = 1;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Speed,
                    -1,
                )),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_stickyweb_with_heavydutyboots() {
        let mut state: State = State::default();
        state.side_one.side_conditions.sticky_web = 1;
        state.side_one.pokemon[PokemonIndex::P1].item = Items::HEAVYDUTYBOOTS;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Switch(SwitchInstruction {
                side_ref: SideReference::SideOne,
                previous_index: PokemonIndex::P0,
                next_index: PokemonIndex::P1,
            })],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_stickyweb_with_contrary() {
        let mut state: State = State::default();
        state.side_one.side_conditions.sticky_web = 1;
        state.side_one.pokemon[PokemonIndex::P1].ability = Abilities::CONTRARY;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonBoostableStat::Speed,
                    1,
                )),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_single_layer_toxicspikes() {
        let mut state: State = State::default();
        state.side_one.side_conditions.toxic_spikes = 1;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
                Instruction::ChangeStatus(ChangeStatusInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: PokemonIndex::P1,
                    old_status: PokemonStatus::NONE,
                    new_status: PokemonStatus::POISON,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_double_layer_toxicspikes() {
        let mut state: State = State::default();
        state.side_one.side_conditions.toxic_spikes = 2;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
                Instruction::ChangeStatus(ChangeStatusInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: PokemonIndex::P1,
                    old_status: PokemonStatus::NONE,
                    new_status: PokemonStatus::TOXIC,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_double_layer_toxicspikes_as_flying_type() {
        let mut state: State = State::default();
        state.side_one.side_conditions.toxic_spikes = 2;
        state.side_one.pokemon[PokemonIndex::P1].types.0 = PokemonType::FLYING;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Switch(SwitchInstruction {
                side_ref: SideReference::SideOne,
                previous_index: PokemonIndex::P0,
                next_index: PokemonIndex::P1,
            })],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_double_layer_toxicspikes_as_poison_and_flying_type() {
        let mut state: State = State::default();
        state.side_one.side_conditions.toxic_spikes = 2;
        state.side_one.pokemon[PokemonIndex::P1].types.0 = PokemonType::FLYING;
        state.side_one.pokemon[PokemonIndex::P1].types.1 = PokemonType::POISON;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Switch(SwitchInstruction {
                side_ref: SideReference::SideOne,
                previous_index: PokemonIndex::P0,
                next_index: PokemonIndex::P1,
            })],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_in_with_intimidate() {
        let mut state: State = State::default();
        state.side_one.pokemon[PokemonIndex::P1].ability = Abilities::INTIMIDATE;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
                Instruction::Boost(BoostInstruction::new(
                    SideReference::SideTwo,
                    0,
                    PokemonBoostableStat::Attack,
                    -1,
                )),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_in_with_intimidate_when_opponent_is_already_lowest_atk_boost() {
        let mut state: State = State::default();
        state.side_one.pokemon[PokemonIndex::P1].ability = Abilities::INTIMIDATE;
        state.side_two.get_active().attack_boost = -6;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Switch(SwitchInstruction {
                side_ref: SideReference::SideOne,
                previous_index: PokemonIndex::P0,
                next_index: PokemonIndex::P1,
            })],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_in_with_intimidate_versus_clearbody() {
        let mut state: State = State::default();
        state.side_one.pokemon[PokemonIndex::P1].ability = Abilities::INTIMIDATE;
        state.side_two.get_active().ability = Abilities::CLEARBODY;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Switch(SwitchInstruction {
                side_ref: SideReference::SideOne,
                previous_index: PokemonIndex::P0,
                next_index: PokemonIndex::P1,
            })],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_double_layer_toxicspikes_as_poison_type() {
        let mut state: State = State::default();
        state.side_one.pokemon[PokemonIndex::P1].types.0 = PokemonType::POISON;
        state.side_one.side_conditions.toxic_spikes = 2;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::ToxicSpikes,
                    amount: -2,
                }),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_stealthrock_and_spikes_does_not_overkill() {
        let mut state: State = State::default();
        state.side_one.side_conditions.stealth_rock = 1;
        state.side_one.side_conditions.spikes = 1;
        state.side_one.pokemon[PokemonIndex::P1].hp = 15;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    12,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    3,
                )),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_switching_into_stealthrock_and_multiple_layers_of_spikes_does_not_overkill() {
        let mut state: State = State::default();
        state.side_one.side_conditions.stealth_rock = 1;
        state.side_one.side_conditions.spikes = 3;
        state.side_one.pokemon[PokemonIndex::P1].hp = 25;
        let mut choice = Choice {
            ..Default::default()
        };
        choice.switch_id = PokemonIndex::P1;

        let expected_instructions: StateInstructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Switch(SwitchInstruction {
                    side_ref: SideReference::SideOne,
                    previous_index: PokemonIndex::P0,
                    next_index: PokemonIndex::P1,
                }),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    12,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    13,
                )),
            ],
            ..Default::default()
        };

        let mut incoming_instructions = StateInstructions::default();
        generate_instructions_from_switch(
            &mut state,
            choice.switch_id,
            SideReference::SideOne,
            &mut incoming_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_healthy_pokemon_with_no_prior_instructions() {
        let mut state = State::default();
        let mut incoming_instructions = StateInstructions::default();

        let expected_instructions = StateInstructions::default();

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            &mut vec![],
        );

        assert_eq!(expected_instructions, incoming_instructions);
    }

    #[test]
    fn test_rest_turns_at_3_with_no_prior_instructions() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::SLEEP;
        state.side_one.get_active().rest_turns = 3;
        let mut incoming_instructions = StateInstructions::default();

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::DecrementRestTurns(
                DecrementRestTurnsInstruction {
                    side_ref: SideReference::SideOne,
                },
            )],
        };

        let expected_frozen_instructions: &mut Vec<StateInstructions> = &mut vec![];

        let frozen_instructions = &mut vec![];
        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_rest_turns_at_2_with_no_prior_instructions() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::SLEEP;
        state.side_one.get_active().rest_turns = 2;
        let mut incoming_instructions = StateInstructions::default();

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::DecrementRestTurns(
                DecrementRestTurnsInstruction {
                    side_ref: SideReference::SideOne,
                },
            )],
        };

        let expected_frozen_instructions: &mut Vec<StateInstructions> = &mut vec![];

        let frozen_instructions = &mut vec![];

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_paralyzed_pokemon_with_no_prior_instructions() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::PARALYZE;
        let mut incoming_instructions = StateInstructions::default();

        let expected_instructions = StateInstructions {
            percentage: 75.0,
            instruction_list: vec![],
        };

        let expected_frozen_instructions = &mut vec![StateInstructions {
            percentage: 25.0,
            instruction_list: vec![],
        }];

        let frozen_instructions = &mut vec![];

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_confused_pokemon_with_no_prior_instructions() {
        let mut state = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::CONFUSION);
        let mut incoming_instructions = StateInstructions::default();

        let expected_instructions = StateInstructions {
            percentage: 100.0 * (1.0 - HIT_SELF_IN_CONFUSION_CHANCE),
            instruction_list: vec![],
        };

        let expected_frozen_instructions = &mut vec![StateInstructions {
            percentage: 100.0 * (HIT_SELF_IN_CONFUSION_CHANCE),
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                35,
            ))],
        }];

        let frozen_instructions = &mut vec![];

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_confused_pokemon_with_prior_instruction() {
        let mut state = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::CONFUSION);
        let mut incoming_instructions = StateInstructions::default();
        incoming_instructions.instruction_list = vec![Instruction::Damage(DamageInstruction::new(
            SideReference::SideOne,
            0,
            1,
        ))];

        let expected_instructions = StateInstructions {
            percentage: 100.0 * (1.0 - HIT_SELF_IN_CONFUSION_CHANCE),
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                1,
            ))],
        };

        let expected_frozen_instructions = &mut vec![StateInstructions {
            percentage: 100.0 * HIT_SELF_IN_CONFUSION_CHANCE,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    1,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    35,
                )),
            ],
        }];

        let frozen_instructions = &mut vec![];

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_confused_pokemon_with_prior_instruction_does_not_overkill() {
        let mut state = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::CONFUSION);
        let mut incoming_instructions = StateInstructions::default();
        state.side_one.get_active().hp = 2;
        incoming_instructions.instruction_list = vec![Instruction::Damage(DamageInstruction::new(
            SideReference::SideOne,
            0,
            1,
        ))];

        let expected_instructions = StateInstructions {
            percentage: 100.0 * (1.0 - HIT_SELF_IN_CONFUSION_CHANCE),
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                1,
            ))],
        };

        let expected_frozen_instructions = &mut vec![StateInstructions {
            percentage: 100.0 * HIT_SELF_IN_CONFUSION_CHANCE,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    1,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    2,
                )),
            ],
        }];

        let frozen_instructions = &mut vec![];

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_frozen_pokemon_with_no_prior_instructions() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::FREEZE;
        let mut incoming_instructions = StateInstructions::default();

        let expected_instructions = StateInstructions {
            percentage: 20.0,
            instruction_list: vec![Instruction::ChangeStatus(ChangeStatusInstruction {
                side_ref: SideReference::SideOne,
                pokemon_index: state.side_one.active_indices[0],
                old_status: PokemonStatus::FREEZE,
                new_status: PokemonStatus::NONE,
            })],
        };

        let expected_frozen_instructions = &mut vec![StateInstructions {
            percentage: 80.0,
            instruction_list: vec![],
        }];

        let frozen_instructions = &mut vec![];

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_asleep_pokemon_with_no_prior_instructions() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::SLEEP;
        state.side_one.get_active().sleep_turns = MAX_SLEEP_TURNS;
        let mut incoming_instructions = StateInstructions::default();

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::ChangeStatus(ChangeStatusInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: state.side_one.active_indices[0],
                    old_status: PokemonStatus::SLEEP,
                    new_status: PokemonStatus::NONE,
                }),
                Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: PokemonIndex::P0,
                    new_turns: 0,
                    previous_turns: MAX_SLEEP_TURNS,
                }),
            ],
        };

        let expected_frozen_instructions: &mut Vec<StateInstructions> = &mut vec![];

        let frozen_instructions = &mut vec![];

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_asleep_waking_up_and_confused() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::SLEEP;
        state.side_one.get_active().sleep_turns = MAX_SLEEP_TURNS;
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::CONFUSION);
        let mut incoming_instructions = StateInstructions::default();

        let expected_instructions = StateInstructions {
            percentage: 100.0 * (1.0 - HIT_SELF_IN_CONFUSION_CHANCE),
            instruction_list: vec![
                Instruction::ChangeStatus(ChangeStatusInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: state.side_one.active_indices[0],
                    old_status: PokemonStatus::SLEEP,
                    new_status: PokemonStatus::NONE,
                }),
                Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: PokemonIndex::P0,
                    new_turns: 0,
                    previous_turns: MAX_SLEEP_TURNS,
                }),
            ],
        };

        let expected_frozen_instructions = &mut vec![StateInstructions {
            percentage: 100.0 * HIT_SELF_IN_CONFUSION_CHANCE,
            instruction_list: vec![
                Instruction::ChangeStatus(ChangeStatusInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: state.side_one.active_indices[0],
                    old_status: PokemonStatus::SLEEP,
                    new_status: PokemonStatus::NONE,
                }),
                Instruction::SetSleepTurns(SetSleepTurnsInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: PokemonIndex::P0,
                    new_turns: 0,
                    previous_turns: MAX_SLEEP_TURNS,
                }),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    35,
                )),
            ],
        }];

        let frozen_instructions = &mut vec![];

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_asleep_pokemon_waking_up_with_1_rest_turn() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::SLEEP;
        state.side_one.get_active().rest_turns = 1;
        let mut incoming_instructions = StateInstructions::default();

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::ChangeStatus(ChangeStatusInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: state.side_one.active_indices[0],
                    old_status: PokemonStatus::SLEEP,
                    new_status: PokemonStatus::NONE,
                }),
                Instruction::DecrementRestTurns(DecrementRestTurnsInstruction {
                    side_ref: SideReference::SideOne,
                }),
            ],
        };

        let expected_frozen_instructions: &mut Vec<StateInstructions> = &mut vec![];
        let frozen_instructions = &mut vec![];

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_asleep_pokemon_staying_asleep_with_two_rest_turns() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::SLEEP;
        state.side_one.get_active().rest_turns = 1;
        let mut incoming_instructions = StateInstructions::default();

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::ChangeStatus(ChangeStatusInstruction {
                    side_ref: SideReference::SideOne,
                    pokemon_index: state.side_one.active_indices[0],
                    old_status: PokemonStatus::SLEEP,
                    new_status: PokemonStatus::NONE,
                }),
                Instruction::DecrementRestTurns(DecrementRestTurnsInstruction {
                    side_ref: SideReference::SideOne,
                }),
            ],
        };

        let expected_frozen_instructions: &mut Vec<StateInstructions> = &mut vec![];
        let frozen_instructions = &mut vec![];

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_paralyzed_pokemon_preserves_prior_instructions() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::PARALYZE;
        let mut incoming_instructions = StateInstructions::default();
        incoming_instructions.instruction_list = vec![Instruction::Damage(DamageInstruction::new(
            SideReference::SideOne,
            0,
            1,
        ))];

        let expected_instructions = StateInstructions {
            percentage: 75.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                1,
            ))],
        };

        let expected_frozen_instructions = &mut vec![StateInstructions {
            percentage: 25.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                1,
            ))],
        }];

        let frozen_instructions = &mut vec![];

        generate_instructions_from_existing_status_conditions(
            &mut state,
            &SideReference::SideOne,
            &Choice::default(),
            &mut incoming_instructions,
            frozen_instructions,
        );

        assert_eq!(expected_instructions, incoming_instructions);
        assert_eq!(expected_frozen_instructions, frozen_instructions);
    }

    #[test]
    fn test_basic_side_two_moves_first() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state.side_one.get_active().speed = 100;
        state.side_two.get_active().speed = 101;

        assert_eq!(
            SideMovesFirst::SideTwo,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_custap_berry_when_less_than_25_percent_activates() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state.side_one.get_active().item = Items::CUSTAPBERRY;
        state.side_one.get_active().hp = 24;
        state.side_one.get_active().speed = 100;
        state.side_two.get_active().speed = 101;

        assert_eq!(
            SideMovesFirst::SideOne,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_quarkdrivespe_boost_works() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::QUARKDRIVESPE);
        state.side_one.get_active().hp = 24;
        state.side_one.get_active().speed = 100;
        state.side_two.get_active().speed = 101;

        assert_eq!(
            SideMovesFirst::SideOne,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_protosynthesisspe_boost_works() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::PROTOSYNTHESISSPE);
        state.side_one.get_active().hp = 24;
        state.side_one.get_active().speed = 100;
        state.side_two.get_active().speed = 101;

        assert_eq!(
            SideMovesFirst::SideOne,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_custap_berry_when_greater_than_25_percent_does_not_activate() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state.side_one.get_active().item = Items::CUSTAPBERRY;
        state.side_one.get_active().speed = 100;
        state.side_two.get_active().speed = 101;

        assert_eq!(
            SideMovesFirst::SideTwo,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_custap_berry_does_not_matter_when_opponent_uses_increased_priority_move() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::QUICKATTACK).unwrap().to_owned();
        state.side_one.get_active().item = Items::CUSTAPBERRY;
        state.side_one.get_active().hp = 24;
        state.side_one.get_active().speed = 100;
        state.side_two.get_active().speed = 101;

        assert_eq!(
            SideMovesFirst::SideTwo,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_slowstart_halves_effective_speed() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state.side_one.get_active().speed = 100;
        state.side_two.get_active().speed = 101;
        state
            .side_two
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::SLOWSTART);

        assert_eq!(
            SideMovesFirst::SideOne,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_basic_side_one_moves_first() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state.side_one.get_active().speed = 101;
        state.side_two.get_active().speed = 100;

        assert_eq!(
            SideMovesFirst::SideOne,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_paralysis_reduces_effective_speed() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();

        state.side_one.get_active().status = PokemonStatus::PARALYZE;
        state.side_one.get_active().speed = 101;
        state.side_two.get_active().speed = 100;

        assert_eq!(
            SideMovesFirst::SideTwo,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    #[cfg(any(feature = "gen7", feature = "gen8", feature = "gen9"))]
    fn test_later_gen_speed_cutting_in_half() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::PARALYZE;
        state.side_one.get_active().speed = 100;

        assert_eq!(50, get_effective_speed(&state, &SideReference::SideOne))
    }

    #[test]
    #[cfg(any(feature = "gen3", feature = "gen4", feature = "gen5", feature = "gen6"))]
    fn test_earlier_gen_speed_cutting_by_75_percent() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::PARALYZE;
        state.side_one.get_active().speed = 100;

        assert_eq!(25, get_effective_speed(&state, &SideReference::SideOne))
    }

    #[test]
    fn test_choicescarf_multiplying_speed() {
        let mut state = State::default();
        state.side_one.get_active().speed = 100;
        state.side_one.get_active().item = Items::CHOICESCARF;

        assert_eq!(150, get_effective_speed(&state, &SideReference::SideOne))
    }

    #[test]
    fn test_iron_ball_halving_speed() {
        let mut state = State::default();
        state.side_one.get_active().speed = 100;
        state.side_one.get_active().item = Items::IRONBALL;

        assert_eq!(50, get_effective_speed(&state, &SideReference::SideOne))
    }

    #[test]
    fn test_speed_tie_goes_to_side_two() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state.side_one.get_active().speed = 100;
        state.side_two.get_active().speed = 100;

        assert_eq!(
            SideMovesFirst::SpeedTie,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_higher_priority_ignores_speed_diff() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::QUICKATTACK).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        state.side_one.get_active().speed = 100;
        state.side_two.get_active().speed = 101;

        assert_eq!(
            SideMovesFirst::SideOne,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_side_two_higher_priority_ignores_speed_diff() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::TACKLE).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::QUICKATTACK).unwrap().to_owned();
        state.side_one.get_active().speed = 101;
        state.side_two.get_active().speed = 100;

        assert_eq!(
            SideMovesFirst::SideTwo,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_both_higher_priority_defaults_back_to_speed() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::QUICKATTACK).unwrap().to_owned();
        let side_two_choice = MOVES.get(&Choices::QUICKATTACK).unwrap().to_owned();
        state.side_one.get_active().speed = 101;
        state.side_two.get_active().speed = 100;

        assert_eq!(
            SideMovesFirst::SideOne,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_switch_always_goes_first() {
        let mut state = State::default();
        let mut side_one_choice = MOVES.get(&Choices::SPLASH).unwrap().to_owned();
        side_one_choice.category = MoveCategory::Switch;
        let side_two_choice = MOVES.get(&Choices::QUICKATTACK).unwrap().to_owned();
        state.side_one.get_active().speed = 99;
        state.side_two.get_active().speed = 100;

        assert_eq!(
            SideMovesFirst::SideOne,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_double_switch_checks_higher_speed() {
        let mut state = State::default();
        let mut side_one_choice = MOVES.get(&Choices::SPLASH).unwrap().to_owned();
        side_one_choice.category = MoveCategory::Switch;
        let mut side_two_choice = MOVES.get(&Choices::SPLASH).unwrap().to_owned();
        side_two_choice.category = MoveCategory::Switch;

        state.side_one.get_active().speed = 99;
        state.side_two.get_active().speed = 100;

        assert_eq!(
            SideMovesFirst::SideTwo,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_pursuit_goes_before_switch() {
        let mut state = State::default();
        let side_one_choice = MOVES.get(&Choices::PURSUIT).unwrap().to_owned();
        let mut side_two_choice = MOVES.get(&Choices::SPLASH).unwrap().to_owned();
        side_two_choice.category = MoveCategory::Switch;

        state.side_one.get_active().speed = 50;
        state.side_two.get_active().speed = 100;

        assert_eq!(
            SideMovesFirst::SideOne,
            moves_first(
                &state,
                &side_one_choice,
                &side_two_choice,
                &mut StateInstructions::default()
            )
        )
    }

    #[test]
    fn test_end_of_turn_hail_damage() {
        let mut state = State::default();
        state.weather.weather_type = Weather::HAIL;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    6,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    6,
                )),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_end_of_turn_hail_damage_against_ice_type() {
        let mut state = State::default();
        state.weather.weather_type = Weather::HAIL;
        state.side_two.get_active().types.0 = PokemonType::ICE;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                // no damage to side_two
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    6,
                )),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_end_of_turn_sand_damage() {
        let mut state = State::default();
        state.weather.weather_type = Weather::SAND;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    6,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    6,
                )),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_end_of_turn_sand_damage_against_ground_type() {
        let mut state = State::default();
        state.weather.weather_type = Weather::SAND;
        state.side_two.get_active().types.0 = PokemonType::GROUND;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,

            // no damage to side_two
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                6,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_hail_does_not_overkill() {
        let mut state = State::default();
        state.weather.weather_type = Weather::HAIL;
        state.side_one.get_active().hp = 3;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    3,
                )),
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideTwo,
                    0,
                    6,
                )),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_fainted_pkmn_does_not_take_hail_dmg() {
        let mut state = State::default();
        state.weather.weather_type = Weather::HAIL;
        state.side_one.get_active().hp = 0;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideTwo,
                0,
                6,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    #[cfg(not(feature = "gen4"))]
    fn test_wished_pokemon_gets_healed() {
        let mut state = State::default();
        state.side_one.wish = (1, 5);
        state.side_one.get_active().hp = 50;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideOne,
                    0,
                    5,
                )),
                Instruction::DecrementWish(DecrementWishInstruction {
                    side_ref: SideReference::SideOne,
                }),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_wish_does_not_overheal() {
        let mut state = State::default();
        state.side_one.wish = (1, 50);
        state.side_one.get_active().hp = 95;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideOne,
                    0,
                    5,
                )),
                Instruction::DecrementWish(DecrementWishInstruction {
                    side_ref: SideReference::SideOne,
                }),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_wish_does_nothing_when_maxhp() {
        let mut state = State::default();
        state.side_one.wish = (1, 50);
        state.side_one.get_active().hp = 100;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::DecrementWish(DecrementWishInstruction {
                side_ref: SideReference::SideOne,
            })],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_wish_does_nothing_when_fainted() {
        let mut state = State::default();
        state.side_one.wish = (1, 50);
        state.side_one.get_active().hp = 0;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::DecrementWish(DecrementWishInstruction {
                side_ref: SideReference::SideOne,
            })],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_wish_at_2_does_not_heal() {
        let mut state = State::default();
        state.side_one.wish = (2, 50);
        state.side_one.get_active().hp = 95;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::DecrementWish(DecrementWishInstruction {
                side_ref: SideReference::SideOne,
            })],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_leftovers_heals_at_end_of_turn() {
        let mut state = State::default();
        state.side_one.get_active().hp = 50;
        state.side_one.get_active().item = Items::LEFTOVERS;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Heal(HealInstruction::new(
                SideReference::SideOne,
                0,
                6,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_leftovers_does_not_overheal() {
        let mut state = State::default();
        state.side_one.get_active().hp = 99;
        state.side_one.get_active().item = Items::LEFTOVERS;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Heal(HealInstruction::new(
                SideReference::SideOne,
                0,
                1,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_leftovers_generates_no_instruction_at_maxhp() {
        let mut state = State::default();
        state.side_one.get_active().hp = 100;
        state.side_one.get_active().item = Items::LEFTOVERS;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_leftovers_generates_no_instruction_when_fainted() {
        let mut state = State::default();
        state.side_one.get_active().hp = 0;
        state.side_one.get_active().item = Items::LEFTOVERS;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_blacksludge_heal_as_poison_type() {
        let mut state = State::default();
        state.side_one.get_active().hp = 50;
        state.side_one.get_active().item = Items::BLACKSLUDGE;
        state.side_one.get_active().types.0 = PokemonType::POISON;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Heal(HealInstruction::new(
                SideReference::SideOne,
                0,
                6,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_blacksludge_damage_as_non_poison_type() {
        let mut state = State::default();
        state.side_one.get_active().hp = 50;
        state.side_one.get_active().item = Items::BLACKSLUDGE;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                6,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_blacksludge_does_not_overheal() {
        let mut state = State::default();
        state.side_one.get_active().hp = 99;
        state.side_one.get_active().item = Items::BLACKSLUDGE;
        state.side_one.get_active().types.0 = PokemonType::POISON;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Heal(HealInstruction::new(
                SideReference::SideOne,
                0,
                1,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_flameorb_end_of_turn_burn() {
        let mut state = State::default();
        state.side_one.get_active().item = Items::FLAMEORB;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::ChangeStatus(ChangeStatusInstruction {
                side_ref: SideReference::SideOne,
                pokemon_index: PokemonIndex::P0,
                old_status: PokemonStatus::NONE,
                new_status: PokemonStatus::BURN,
            })],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_fire_type_cannot_be_burned_by_flameorb() {
        let mut state = State::default();
        state.side_one.get_active().item = Items::FLAMEORB;
        state.side_one.get_active().types.0 = PokemonType::FIRE;
        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_toxicorb_applies_status() {
        let mut state = State::default();
        state.side_one.get_active().item = Items::TOXICORB;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::ChangeStatus(ChangeStatusInstruction {
                side_ref: SideReference::SideOne,
                pokemon_index: PokemonIndex::P0,
                old_status: PokemonStatus::NONE,
                new_status: PokemonStatus::TOXIC,
            })],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_toxicorb_does_not_apply_to_poison_type() {
        let mut state = State::default();
        state.side_one.get_active().item = Items::TOXICORB;
        state.side_one.get_active().types.0 = PokemonType::POISON;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_poisonheal_heals_at_end_of_turn() {
        let mut state = State::default();
        state.side_one.get_active().ability = Abilities::POISONHEAL;
        state.side_one.get_active().status = PokemonStatus::POISON;
        state.side_one.get_active().hp = 50;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Heal(HealInstruction::new(
                SideReference::SideOne,
                0,
                12,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_poisonheal_while_toxiced_still_increases_toxic_count() {
        let mut state = State::default();
        state.side_one.get_active().ability = Abilities::POISONHEAL;
        state.side_one.get_active().status = PokemonStatus::TOXIC;
        state.side_one.get_active().hp = 50;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::ToxicCount,
                    amount: 1,
                }),
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideOne,
                    0,
                    12,
                )),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_poisonheal_does_not_overheal() {
        let mut state = State::default();
        state.side_one.get_active().ability = Abilities::POISONHEAL;
        state.side_one.get_active().status = PokemonStatus::POISON;
        state.side_one.get_active().hp = 99;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Heal(HealInstruction::new(
                SideReference::SideOne,
                0,
                1,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_poisonheal_does_nothing_at_maxhp() {
        let mut state = State::default();
        state.side_one.get_active().ability = Abilities::POISONHEAL;
        state.side_one.get_active().status = PokemonStatus::POISON;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_speedboost() {
        let mut state = State::default();
        state.side_one.get_active().ability = Abilities::SPEEDBOOST;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Boost(BoostInstruction::new(
                SideReference::SideOne,
                0,
                PokemonBoostableStat::Speed,
                1,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_speedboost_does_not_boost_beyond_6() {
        let mut state = State::default();
        state.side_one.get_active().ability = Abilities::SPEEDBOOST;
        state.side_one.get_active().speed_boost = 6;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_end_of_turn_poison_damage() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::POISON;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                12,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_poison_damage_does_not_overkill() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::POISON;
        state.side_one.get_active().hp = 5;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                5,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    #[cfg(any(feature = "gen9", feature = "gen8", feature = "gen7"))]
    fn test_end_of_turn_burn_damage() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::BURN;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                6,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    #[cfg(any(feature = "gen3", feature = "gen4", feature = "gen5", feature = "gen6"))]
    fn test_early_generation_burn_one_eigth() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::BURN;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                12,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_burn_damage_does_not_overkill() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::BURN;
        state.side_one.get_active().hp = 5;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                5,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_burn_damage_ignored_if_has_magicguard() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::BURN;
        state.side_one.get_active().ability = Abilities::MAGICGUARD;
        state.side_one.get_active().hp = 5;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_first_toxic_damage() {
        let mut state = State::default();
        state.side_one.get_active().status = PokemonStatus::TOXIC;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    6,
                )),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::ToxicCount,
                    amount: 1,
                }),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_leechseed_sap() {
        let mut state = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::LEECHSEED);
        state.side_one.get_active().hp = 50;
        state.side_two.get_active().hp = 50;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    12,
                )),
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideTwo,
                    0,
                    12,
                )),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_leechseed_sap_does_not_heal_if_receiving_side_is_maxhp() {
        let mut state = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::LEECHSEED);
        state.side_one.get_active().hp = 50;
        state.side_two.get_active().hp = 100;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                12,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_leechseed_sap_does_not_overkill() {
        let mut state = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::LEECHSEED);
        state.side_one.get_active().hp = 5;
        state.side_two.get_active().hp = 50;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    5,
                )),
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideTwo,
                    0,
                    5,
                )),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_leechseed_sap_does_not_overheal() {
        let mut state = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::LEECHSEED);
        state.side_one.get_active().hp = 50;
        state.side_two.get_active().hp = 95;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::Damage(DamageInstruction::new(
                    SideReference::SideOne,
                    0,
                    12,
                )),
                Instruction::Heal(HealInstruction::new(
                    SideReference::SideTwo,
                    0,
                    5,
                )),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_protect_volatile_being_removed() {
        let mut state = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::PROTECT);

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![
                Instruction::RemoveVolatileStatus(RemoveVolatileStatusInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonVolatileStatus::PROTECT,
                )),
                Instruction::ChangeSideCondition(ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::Protect,
                    amount: 1,
                }),
            ],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_protect_side_condition_being_removed() {
        let mut state = State::default();
        state.side_one.side_conditions.protect = 2;

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::ChangeSideCondition(
                ChangeSideConditionInstruction {
                    side_ref: SideReference::SideOne,
                    side_condition: PokemonSideCondition::Protect,
                    amount: -2,
                },
            )],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_roost_vs_removal() {
        let mut state = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::ROOST);

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::RemoveVolatileStatus(
                RemoveVolatileStatusInstruction::new(
                    SideReference::SideOne,
                    0,
                    PokemonVolatileStatus::ROOST,
                ),
            )],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_partiallytrapped_damage() {
        let mut state = State::default();
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::PARTIALLYTRAPPED);

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        #[cfg(any(feature = "gen3", feature = "gen4", feature = "gen5"))]
        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                6,
            ))],
        };

        #[cfg(any(feature = "gen6", feature = "gen7", feature = "gen8", feature = "gen9"))]
        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                12,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_saltcure_on_water_type_damage() {
        let mut state = State::default();
        state.side_one.get_active().types.0 = PokemonType::WATER;
        state
            .side_one
            .get_active().volatile_statuses
            .insert(PokemonVolatileStatus::SALTCURE);

        let mut incoming_instructions = StateInstructions::default();
        add_end_of_turn_instructions(
            &mut state,
            &mut incoming_instructions,
            &SideReference::SideOne,
        );

        let expected_instructions = StateInstructions {
            percentage: 100.0,
            instruction_list: vec![Instruction::Damage(DamageInstruction::new(
                SideReference::SideOne,
                0,
                25,
            ))],
        };

        assert_eq!(expected_instructions, incoming_instructions)
    }

    #[test]
    fn test_chance_to_wake_up_with_no_turns_asleep_is_0() {
        assert_eq!(0.0, chance_to_wake_up(0));
    }

    #[test]
    #[cfg(any(feature = "gen4"))]
    fn test_gen4_25_percent_to_wake_after_1_sleep_turn() {
        assert_eq!(0.25, chance_to_wake_up(1));
    }

    #[test]
    #[cfg(any(feature = "gen4"))]
    fn test_gen4_100_percent_to_wake_after_4_sleep_turn() {
        assert_eq!(1.0, chance_to_wake_up(4));
    }
}
