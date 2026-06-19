use super::abilities::Abilities;
use super::items::Items;
use crate::choices::{Choices, MoveCategory};
#[cfg(feature = "doubles")]
use crate::choices::MoveTarget;
use crate::define_enum_with_from_str;
use crate::instruction::BoostInstruction;
use crate::instruction::{
    ChangeSideConditionInstruction, ChangeStatInstruction, ChangeType,
    ChangeVolatileStatusDurationInstruction, Instruction, RemoveVolatileStatusInstruction,
    StateInstructions,
};
use crate::pokemon::PokemonName;
#[cfg(feature = "doubles")]
use crate::state::ACTIVE_PER_SIDE;
use crate::state::VolatileStatusBitset;
use crate::state::{
    BattlePosition, LastUsedMove, Move, Pokemon, PokemonBoostableStat, PokemonIndex,
    PokemonMoveIndex, PokemonSideCondition, PokemonStatus, PokemonType, Side, SideReference, State,
};
use core::panic;

fn common_pkmn_stat_calc(stat: u16, ev: u16, level: u16) -> u16 {
    // 31 IV always used
    ((2 * stat + 31 + (ev / 4)) * level) / 100
}

fn multiply_boost(boost_num: i8, stat_value: i16) -> i16 {
    match boost_num {
        -6 => stat_value * 2 / 8,
        -5 => stat_value * 2 / 7,
        -4 => stat_value * 2 / 6,
        -3 => stat_value * 2 / 5,
        -2 => stat_value * 2 / 4,
        -1 => stat_value * 2 / 3,
        0 => stat_value,
        1 => stat_value * 3 / 2,
        2 => stat_value * 4 / 2,
        3 => stat_value * 5 / 2,
        4 => stat_value * 6 / 2,
        5 => stat_value * 7 / 2,
        6 => stat_value * 8 / 2,
        _ => panic!("Invalid boost number: {}", boost_num),
    }
}

/// A chosen action for one active Pokemon.
///
/// The move variants now carry the `BattlePosition` they are aimed at. In singles
/// there is only ever one possible target (the opposing active, slot 0), so the
/// target is filled automatically by `from_string` / `get_all_options` and is not
/// yet consumed by the engine — singles instruction generation still resolves the
/// target from the attacking side. The richer per-slot targeting it enables is for
/// the upcoming doubles work.
#[derive(Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum MoveChoice {
    MoveTera {
        move_index: PokemonMoveIndex,
        target: BattlePosition,
    },
    MoveMega {
        move_index: PokemonMoveIndex,
        target: BattlePosition,
    },
    Move {
        move_index: PokemonMoveIndex,
        target: BattlePosition,
    },
    Switch(PokemonIndex),
    None,
}

impl MoveChoice {
    pub fn new_move(move_index: PokemonMoveIndex, target: BattlePosition) -> MoveChoice {
        MoveChoice::Move { move_index, target }
    }
    pub fn new_move_tera(move_index: PokemonMoveIndex, target: BattlePosition) -> MoveChoice {
        MoveChoice::MoveTera { move_index, target }
    }
    pub fn new_move_mega(move_index: PokemonMoveIndex, target: BattlePosition) -> MoveChoice {
        MoveChoice::MoveMega { move_index, target }
    }

    pub fn to_string(&self, side: &Side) -> String {
        match self {
            MoveChoice::MoveTera { move_index, .. } => {
                format!("{}-tera", side.get_active_immutable().moves[move_index].id).to_lowercase()
            }
            MoveChoice::MoveMega { move_index, .. } => {
                format!("{}-mega", side.get_active_immutable().moves[move_index].id).to_lowercase()
            }
            MoveChoice::Move { move_index, .. } => {
                format!("{}", side.get_active_immutable().moves[move_index].id).to_lowercase()
            }
            MoveChoice::Switch(index) => format!("{}", side.pokemon[*index].id).to_lowercase(),
            MoveChoice::None => "No Move".to_string(),
        }
    }

    /// Parse a move/switch choice for `side`, whose battle side is `side_reference`.
    ///
    /// An optional `,<slot>` suffix selects the targeted opposing slot (e.g.
    /// `"closecombat,2"`). In singles there is only one opposing slot, so the suffix
    /// is accepted but the target always resolves to the opposing active (slot 0).
    ///
    /// The move name is resolved against the active in slot 0; for the per-slot
    /// parsing used by doubles see [`MoveChoice::from_string_slot`].
    pub fn from_string(s: &str, side: &Side, side_reference: SideReference) -> Option<MoveChoice> {
        MoveChoice::from_string_slot(s, side, side_reference, 0)
    }

    /// Like [`MoveChoice::from_string`], but resolves the move name against the
    /// active Pokemon in `acting_slot` (rather than always slot 0). This is what
    /// lets a doubles combined action name slot 1's own moves. In singles
    /// `acting_slot` is always 0, so this is identical to `from_string`.
    pub fn from_string_slot(
        s: &str,
        side: &Side,
        side_reference: SideReference,
        acting_slot: u8,
    ) -> Option<MoveChoice> {
        let s = s.to_lowercase();
        if s == "none" {
            return Some(MoveChoice::None);
        }

        // Split off an optional `,<slot>` target suffix. In singles the only valid
        // target is the opposing active, so the parsed slot is ignored; in doubles
        // it selects which opposing slot the move aims at (defaulting to slot 0).
        let (s, _target_slot) = match s.split_once(',') {
            Some((name, slot)) => (name.to_string(), slot.parse::<u8>().ok()),
            None => (s, None),
        };
        #[cfg(not(feature = "doubles"))]
        let target = BattlePosition::new(side_reference.get_other_side(), 0);
        #[cfg(feature = "doubles")]
        let target =
            BattlePosition::new(side_reference.get_other_side(), _target_slot.unwrap_or(0));

        let mut pkmn_iter = side.pokemon.into_iter();
        while let Some(pkmn) = pkmn_iter.next() {
            // A switch target must be a benched Pokemon: not currently active in
            // any slot. In singles `active_indices` has one entry, so this matches
            // the original `!= active_indices[0]` exactly.
            if pkmn.id.to_string().to_lowercase() == s
                && !side.active_indices.contains(&pkmn_iter.pokemon_index)
            {
                return Some(MoveChoice::Switch(pkmn_iter.pokemon_index));
            }
        }

        // check if s endswith `-tera`
        // if it does, find the move with the name and return MoveChoice::MoveTera
        // if it doesn't, find the move with the name and return MoveChoice::Move
        let mut move_iter = side
            .get_active_slot_immutable(acting_slot)
            .moves
            .into_iter();
        let mut move_name = s;
        if move_name.ends_with("-tera") {
            move_name = move_name[..move_name.len() - 5].to_string();
            while let Some(mv) = move_iter.next() {
                if format!("{:?}", mv.id).to_lowercase() == move_name {
                    return Some(MoveChoice::MoveTera {
                        move_index: move_iter.pokemon_move_index,
                        target,
                    });
                }
            }
        } else if move_name.ends_with("-mega") {
            move_name = move_name[..move_name.len() - 5].to_string();
            while let Some(mv) = move_iter.next() {
                if format!("{:?}", mv.id).to_lowercase() == move_name {
                    return Some(MoveChoice::MoveMega {
                        move_index: move_iter.pokemon_move_index,
                        target,
                    });
                }
            }
        } else {
            while let Some(mv) = move_iter.next() {
                if format!("{:?}", mv.id).to_lowercase() == move_name {
                    return Some(MoveChoice::Move {
                        move_index: move_iter.pokemon_move_index,
                        target,
                    });
                }
            }
        }

        None
    }
}

/// A combined action for one side in doubles: one `MoveChoice` per active slot.
///
/// `actions[slot]` is the sub-action chosen for that slot. An empty/fainted slot
/// with no pending choice is represented by `MoveChoice::None`. This is the unit
/// that doubles search and (in a later phase) turn resolution consume: a side's
/// legal options are the cartesian product of its slots' sub-actions, minus the
/// combinations that break cross-slot legality (e.g. both slots switching to the
/// same benched Pokemon).
///
/// Only defined under the `doubles` feature; in singles a side's action is a bare
/// `MoveChoice` and `get_all_options` keeps its original signature.
#[cfg(feature = "doubles")]
#[derive(Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub struct SideAction {
    pub actions: [MoveChoice; crate::state::ACTIVE_PER_SIDE],
}

#[cfg(feature = "doubles")]
impl SideAction {
    pub fn new(actions: [MoveChoice; crate::state::ACTIVE_PER_SIDE]) -> SideAction {
        SideAction { actions }
    }

    /// Parse a combined side action from the CLI: the per-slot sub-actions
    /// separated by `;`, in slot order. Each sub-action is parsed by
    /// [`MoveChoice::from_string`] (so it may carry an optional `,<slot>` target
    /// suffix); an empty or `none` sub-action becomes [`MoveChoice::None`]. Fewer
    /// `;`-parts than slots leaves the trailing slots as `None`; more parts than
    /// slots is rejected.
    pub fn from_string(
        s: &str,
        side: &Side,
        side_reference: SideReference,
    ) -> Option<SideAction> {
        let mut actions = [MoveChoice::None; crate::state::ACTIVE_PER_SIDE];
        for (i, part) in s.split(';').enumerate() {
            if i >= crate::state::ACTIVE_PER_SIDE {
                return None;
            }
            let part = part.trim();
            if part.is_empty() || part.eq_ignore_ascii_case("none") {
                actions[i] = MoveChoice::None;
                continue;
            }
            // Resolve each slot's sub-action against that slot's own active, so a
            // move that only slot 1 knows still parses.
            actions[i] = MoveChoice::from_string_slot(part, side, side_reference, i as u8)?;
        }
        Some(SideAction::new(actions))
    }
}

define_enum_with_from_str! {
    #[repr(u8)]
    #[derive(PartialEq, Eq, Hash, Debug, Copy, Clone)]
    PokemonVolatileStatus {
        NONE,
        AQUARING,
        ATTRACT,
        AUTOTOMIZE,
        BANEFULBUNKER,
        BIDE,
        BOUNCE,
        BURNINGBULWARK,
        CHARGE,
        CONFUSION,
        CURSE,
        DEFENSECURL,
        DESTINYBOND,
        DIG,
        DISABLE,
        DIVE,
        ELECTRIFY,
        ELECTROSHOT,
        EMBARGO,
        ENCORE,
        ENDURE,
        FLASHFIRE,
        FLINCH,
        FLY,
        FOCUSENERGY,
        FOLLOWME,
        FORESIGHT,
        FREEZESHOCK,
        GASTROACID,
        GEOMANCY,
        GLAIVERUSH,
        GRUDGE,
        HEALBLOCK,
        HELPINGHAND,
        ICEBURN,
        IMPRISON,
        INGRAIN,
        KINGSSHIELD,
        LASERFOCUS,
        LEECHSEED,
        LIGHTSCREEN,
        LOCKEDMOVE,
        MAGICCOAT,
        MAGNETRISE,
        MAXGUARD,
        METEORBEAM,
        MINIMIZE,
        MIRACLEEYE,
        MUSTRECHARGE,
        NIGHTMARE,
        NORETREAT,
        OCTOLOCK,
        PARTIALLYTRAPPED,
        PERISH4,
        PERISH3,
        PERISH2,
        PERISH1,
        PHANTOMFORCE,
        POWDER,
        POWERSHIFT,
        POWERTRICK,
        PROTECT,
        PROTOSYNTHESISATK,
        PROTOSYNTHESISDEF,
        PROTOSYNTHESISSPA,
        PROTOSYNTHESISSPD,
        PROTOSYNTHESISSPE,
        QUARKDRIVEATK,
        QUARKDRIVEDEF,
        QUARKDRIVESPA,
        QUARKDRIVESPD,
        QUARKDRIVESPE,
        RAGE,
        RAGEPOWDER,
        RAZORWIND,
        REFLECT,
        ROOST,
        SALTCURE,
        SHADOWFORCE,
        SKULLBASH,
        SKYATTACK,
        SKYDROP,
        SILKTRAP,
        SLOWSTART,
        SMACKDOWN,
        SNATCH,
        SOLARBEAM,
        SOLARBLADE,
        SPARKLINGARIA,
        SPIKYSHIELD,
        SPOTLIGHT,
        STOCKPILE,
        SUBSTITUTE,
        SYRUPBOMB,
        TARSHOT,
        TAUNT,
        TELEKINESIS,
        THROATCHOP,
        TRUANT,
        TORMENT,
        TYPECHANGE,
        UNBURDEN,
        UPROAR,
        YAWN,
    },
    default = NONE
}

define_enum_with_from_str! {
    #[repr(u8)]
    #[derive(Debug, PartialEq, Copy, Clone)]
    Weather {
        NONE,
        SUN,
        RAIN,
        SAND,
        HAIL,
        SNOW,
        HARSHSUN,
        HEAVYRAIN,
    }
}

define_enum_with_from_str! {
    #[repr(u8)]
    #[derive(Debug, PartialEq, Copy, Clone)]
    Terrain {
        NONE,
        ELECTRICTERRAIN,
        PSYCHICTERRAIN,
        MISTYTERRAIN,
        GRASSYTERRAIN,
    }
}

impl Pokemon {
    pub fn can_mega_evolve(&self) -> bool {
        // this assumes that if you have the correct mega stone, you can always mega evolve
        // even if another pkmn on the team already mega evolved
        // it is incorrect but practically most teams aren't going to have multiple mega stones
        if let Some(_mega_evolve_data) = self.id.mega_evolve_target(self.item) {
            true
        } else {
            false
        }
    }

    pub fn recalculate_stats(
        &mut self,
        side_ref: &SideReference,
        instructions: &mut StateInstructions,
    ) {
        // recalculate stats from base-stats and push any changes made to the StateInstructions
        let stats = self.calculate_stats_from_base_stats();
        // NOTE(doubles): stat recalculation targets slot 0, consistent with the
        // slot-0 switch/transform mechanic (per-slot deferred).
        if stats.1 != self.attack {
            let ins = Instruction::ChangeAttack(ChangeStatInstruction::new(
                *side_ref,
                0,
                stats.1 - self.attack,
            ));
            self.attack = stats.1;
            instructions.instruction_list.push(ins);
        }
        if stats.2 != self.defense {
            let ins = Instruction::ChangeDefense(ChangeStatInstruction::new(
                *side_ref,
                0,
                stats.2 - self.defense,
            ));
            self.defense = stats.2;
            instructions.instruction_list.push(ins);
        }
        if stats.3 != self.special_attack {
            let ins = Instruction::ChangeSpecialAttack(ChangeStatInstruction::new(
                *side_ref,
                0,
                stats.3 - self.special_attack,
            ));
            self.special_attack = stats.3;
            instructions.instruction_list.push(ins);
        }
        if stats.4 != self.special_defense {
            let ins = Instruction::ChangeSpecialDefense(ChangeStatInstruction::new(
                *side_ref,
                0,
                stats.4 - self.special_defense,
            ));
            self.special_defense = stats.4;
            instructions.instruction_list.push(ins);
        }
        if stats.5 != self.speed {
            let ins = Instruction::ChangeSpeed(ChangeStatInstruction::new(
                *side_ref,
                0,
                stats.5 - self.speed,
            ));
            self.speed = stats.5;
            instructions.instruction_list.push(ins);
        }
    }
    pub fn calculate_stats_from_base_stats(&self) -> (i16, i16, i16, i16, i16, i16) {
        let base_stats = self.id.base_stats();
        (
            (common_pkmn_stat_calc(base_stats.0 as u16, self.evs.0 as u16, self.level as u16)
                + self.level as u16
                + 10) as i16,
            (common_pkmn_stat_calc(base_stats.1 as u16, self.evs.1 as u16, self.level as u16) + 5)
                as i16,
            (common_pkmn_stat_calc(base_stats.2 as u16, self.evs.2 as u16, self.level as u16) + 5)
                as i16,
            (common_pkmn_stat_calc(base_stats.3 as u16, self.evs.3 as u16, self.level as u16) + 5)
                as i16,
            (common_pkmn_stat_calc(base_stats.4 as u16, self.evs.4 as u16, self.level as u16) + 5)
                as i16,
            (common_pkmn_stat_calc(base_stats.5 as u16, self.evs.5 as u16, self.level as u16) + 5)
                as i16,
        )
    }
    /// Whether `move_index` (the move `mv`) is a selectable action this turn,
    /// given the lock-in / restriction state. Shared by the singles
    /// (`add_available_moves`) and doubles (`add_available_moves_doubles`) option
    /// enumeration so both apply identical legality rules.
    fn move_is_selectable(
        &self,
        move_index: PokemonMoveIndex,
        mv: &Move,
        last_used_move: &LastUsedMove,
        encored: bool,
        taunted: bool,
    ) -> bool {
        if mv.disabled || mv.pp <= 0 {
            return false;
        }
        match last_used_move {
            LastUsedMove::Move(last_used_move) => {
                if encored && last_used_move != &move_index {
                    return false;
                } else if (self.moves[last_used_move].id == Choices::BLOODMOON
                    || self.moves[last_used_move].id == Choices::GIGATONHAMMER)
                    && &move_index == last_used_move
                {
                    return false;
                }
            }
            _ => {
                // there are some situations where you switched out and got encored into
                // a move from a different pokemon because you also have that move.
                // just assume nothing is locked in this case
            }
        }
        if (self.item == Items::ASSAULTVEST || taunted)
            && mv.choice.category == MoveCategory::Status
        {
            return false;
        }
        true
    }

    pub fn add_available_moves(
        &self,
        vec: &mut Vec<MoveChoice>,
        last_used_move: &LastUsedMove,
        encored: bool,
        taunted: bool,
        can_tera: bool,
        target: BattlePosition,
    ) {
        let mut iter = self.moves.into_iter();
        while let Some(p) = iter.next() {
            if self.move_is_selectable(iter.pokemon_move_index, p, last_used_move, encored, taunted)
            {
                vec.push(MoveChoice::Move {
                    move_index: iter.pokemon_move_index,
                    target,
                });
                if can_tera {
                    vec.push(MoveChoice::MoveTera {
                        move_index: iter.pokemon_move_index,
                        target,
                    });
                }
                if self.can_mega_evolve() {
                    vec.push(MoveChoice::MoveMega {
                        move_index: iter.pokemon_move_index,
                        target,
                    });
                }
            }
        }
    }

    /// Doubles variant of [`Pokemon::add_available_moves`].
    ///
    /// For each selectable move it asks `targets_for` (given the move's
    /// [`MoveTarget`]) for the list of [`BattlePosition`]s that should each spawn a
    /// distinct option, and pushes one `MoveChoice` (plus Tera/Mega variants) per
    /// target. Single-target foe moves therefore fan out over the living foes,
    /// while spread / side / self moves yield a single option at a nominal
    /// position. A move whose `targets_for` returns no positions (e.g. an
    /// ally-only move with no living ally) is skipped.
    #[cfg(feature = "doubles")]
    pub fn add_available_moves_doubles<F>(
        &self,
        vec: &mut Vec<MoveChoice>,
        last_used_move: &LastUsedMove,
        encored: bool,
        taunted: bool,
        can_tera: bool,
        mut targets_for: F,
    ) where
        F: FnMut(&MoveTarget) -> Vec<BattlePosition>,
    {
        let mut iter = self.moves.into_iter();
        while let Some(p) = iter.next() {
            if self.move_is_selectable(iter.pokemon_move_index, p, last_used_move, encored, taunted)
            {
                for target in targets_for(&p.choice.target) {
                    vec.push(MoveChoice::Move {
                        move_index: iter.pokemon_move_index,
                        target,
                    });
                    if can_tera {
                        vec.push(MoveChoice::MoveTera {
                            move_index: iter.pokemon_move_index,
                            target,
                        });
                    }
                    if self.can_mega_evolve() {
                        vec.push(MoveChoice::MoveMega {
                            move_index: iter.pokemon_move_index,
                            target,
                        });
                    }
                }
            }
        }
    }

    pub fn add_move_from_choice(
        &self,
        vec: &mut Vec<MoveChoice>,
        choice: Choices,
        target: BattlePosition,
    ) {
        let mut iter = self.moves.into_iter();
        while let Some(p) = iter.next() {
            if p.id == choice {
                vec.push(MoveChoice::Move {
                    move_index: iter.pokemon_move_index,
                    target,
                });
            }
        }
    }

    #[cfg(feature = "terastallization")]
    pub fn has_type(&self, pkmn_type: &PokemonType) -> bool {
        if self.terastallized {
            pkmn_type == &self.tera_type
        } else {
            pkmn_type == &self.types.0 || pkmn_type == &self.types.1
        }
    }

    #[cfg(not(feature = "terastallization"))]
    pub fn has_type(&self, pkmn_type: &PokemonType) -> bool {
        pkmn_type == &self.types.0 || pkmn_type == &self.types.1
    }

    pub fn item_is_permanent(&self) -> bool {
        match self.item {
            Items::LUSTROUSGLOBE => self.id == PokemonName::PALKIAORIGIN,
            Items::GRISEOUSCORE => self.id == PokemonName::GIRATINAORIGIN,
            Items::ADAMANTCRYSTAL => self.id == PokemonName::DIALGAORIGIN,
            Items::RUSTEDSWORD => {
                self.id == PokemonName::ZACIANCROWNED || self.id == PokemonName::ZACIAN
            }
            Items::RUSTEDSHIELD => {
                self.id == PokemonName::ZAMAZENTACROWNED || self.id == PokemonName::ZAMAZENTA
            }
            Items::SPLASHPLATE => self.id == PokemonName::ARCEUSWATER,
            Items::TOXICPLATE => self.id == PokemonName::ARCEUSPOISON,
            Items::EARTHPLATE => self.id == PokemonName::ARCEUSGROUND,
            Items::STONEPLATE => self.id == PokemonName::ARCEUSROCK,
            Items::INSECTPLATE => self.id == PokemonName::ARCEUSBUG,
            Items::SPOOKYPLATE => self.id == PokemonName::ARCEUSGHOST,
            Items::IRONPLATE => self.id == PokemonName::ARCEUSSTEEL,
            Items::FLAMEPLATE => self.id == PokemonName::ARCEUSFIRE,
            Items::MEADOWPLATE => self.id == PokemonName::ARCEUSGRASS,
            Items::ZAPPLATE => self.id == PokemonName::ARCEUSELECTRIC,
            Items::MINDPLATE => self.id == PokemonName::ARCEUSPSYCHIC,
            Items::ICICLEPLATE => self.id == PokemonName::ARCEUSICE,
            Items::DRACOPLATE => self.id == PokemonName::ARCEUSDRAGON,
            Items::DREADPLATE => self.id == PokemonName::ARCEUSDARK,
            Items::FISTPLATE => self.id == PokemonName::ARCEUSFIGHTING,
            Items::BLANKPLATE => self.id == PokemonName::ARCEUS,
            Items::SKYPLATE => self.id == PokemonName::ARCEUSFLYING,
            Items::PIXIEPLATE => self.id == PokemonName::ARCEUSFAIRY,
            Items::BUGMEMORY => self.id == PokemonName::SILVALLYBUG,
            Items::FIGHTINGMEMORY => self.id == PokemonName::SILVALLYFIGHTING,
            Items::GHOSTMEMORY => self.id == PokemonName::SILVALLYGHOST,
            Items::PSYCHICMEMORY => self.id == PokemonName::SILVALLYPSYCHIC,
            Items::FLYINGMEMORY => self.id == PokemonName::SILVALLYFLYING,
            Items::STEELMEMORY => self.id == PokemonName::SILVALLYSTEEL,
            Items::ICEMEMORY => self.id == PokemonName::SILVALLYICE,
            Items::POISONMEMORY => self.id == PokemonName::SILVALLYPOISON,
            Items::FIREMEMORY => self.id == PokemonName::SILVALLYFIRE,
            Items::DRAGONMEMORY => self.id == PokemonName::SILVALLYDRAGON,
            Items::GROUNDMEMORY => self.id == PokemonName::SILVALLYGROUND,
            Items::WATERMEMORY => self.id == PokemonName::SILVALLYWATER,
            Items::DARKMEMORY => self.id == PokemonName::SILVALLYDARK,
            Items::ROCKMEMORY => self.id == PokemonName::SILVALLYROCK,
            Items::GRASSMEMORY => self.id == PokemonName::SILVALLYGRASS,
            Items::FAIRYMEMORY => self.id == PokemonName::SILVALLYFAIRY,
            Items::ELECTRICMEMORY => self.id == PokemonName::SILVALLYELECTRIC,
            Items::CORNERSTONEMASK => {
                self.id == PokemonName::OGERPONCORNERSTONE
                    || self.id == PokemonName::OGERPONCORNERSTONETERA
            }
            Items::HEARTHFLAMEMASK => {
                self.id == PokemonName::OGERPONHEARTHFLAME
                    || self.id == PokemonName::OGERPONHEARTHFLAMETERA
            }
            Items::WELLSPRINGMASK => {
                self.id == PokemonName::OGERPONWELLSPRING
                    || self.id == PokemonName::OGERPONWELLSPRINGTERA
            }
            _ => false,
        }
    }

    pub fn item_can_be_removed(&self) -> bool {
        if self.ability == Abilities::STICKYHOLD {
            return false;
        }
        !self.item_is_permanent()
    }

    pub fn is_grounded(&self) -> bool {
        if self.item == Items::IRONBALL {
            return true;
        }
        if self.has_type(&PokemonType::FLYING)
            || self.ability == Abilities::LEVITATE
            || self.item == Items::AIRBALLOON
        {
            return false;
        }
        true
    }

    pub fn volatile_status_can_be_applied(
        &self,
        volatile_status: &PokemonVolatileStatus,
        active_volatiles: &VolatileStatusBitset,
        first_move: bool,
    ) -> bool {
        if active_volatiles.contains(volatile_status) || self.hp == 0 {
            return false;
        }
        match volatile_status {
            PokemonVolatileStatus::LEECHSEED => {
                if self.has_type(&PokemonType::GRASS)
                    || active_volatiles.contains(&PokemonVolatileStatus::SUBSTITUTE)
                {
                    return false;
                }
                true
            }
            PokemonVolatileStatus::CONFUSION => {
                if active_volatiles.contains(&PokemonVolatileStatus::SUBSTITUTE) {
                    return false;
                }
                true
            }
            PokemonVolatileStatus::SUBSTITUTE => self.hp > self.maxhp / 4,
            PokemonVolatileStatus::FLINCH => {
                if !first_move || [Abilities::INNERFOCUS].contains(&self.ability) {
                    return false;
                }
                true
            }
            PokemonVolatileStatus::PROTECT => first_move,
            PokemonVolatileStatus::TAUNT
            | PokemonVolatileStatus::TORMENT
            | PokemonVolatileStatus::ENCORE
            | PokemonVolatileStatus::DISABLE
            | PokemonVolatileStatus::HEALBLOCK
            | PokemonVolatileStatus::ATTRACT => self.ability != Abilities::AROMAVEIL,
            _ => true,
        }
    }

    pub fn immune_to_stats_lowered_by_opponent(
        &self,
        stat: &PokemonBoostableStat,
        volatiles: &VolatileStatusBitset,
    ) -> bool {
        if [
            Abilities::CLEARBODY,
            Abilities::WHITESMOKE,
            Abilities::FULLMETALBODY,
        ]
        .contains(&self.ability)
            || ([Items::CLEARAMULET].contains(&self.item))
        {
            return true;
        }

        if volatiles.contains(&PokemonVolatileStatus::SUBSTITUTE) {
            return true;
        }

        if stat == &PokemonBoostableStat::Attack && self.ability == Abilities::HYPERCUTTER {
            return true;
        } else if stat == &PokemonBoostableStat::Accuracy && self.ability == Abilities::KEENEYE {
            return true;
        }

        false
    }
}

impl Side {
    pub fn reset_negative_boosts(
        &mut self,
        side_ref: SideReference,
        instructions: &mut StateInstructions,
    ) -> bool {
        // NOTE(doubles): operates on slot 0 (per-slot reset deferred with switches).
        let mut changed = false;
        if self.get_active().attack_boost < 0 {
            let amount = -self.get_active().attack_boost;
            instructions
                .instruction_list
                .push(Instruction::Boost(BoostInstruction::new(
                    side_ref,
                    0,
                    PokemonBoostableStat::Attack,
                    amount,
                )));
            self.get_active().attack_boost = 0;
            changed = true;
        }
        if self.get_active().defense_boost < 0 {
            let amount = -self.get_active().defense_boost;
            instructions
                .instruction_list
                .push(Instruction::Boost(BoostInstruction::new(
                    side_ref,
                    0,
                    PokemonBoostableStat::Defense,
                    amount,
                )));
            self.get_active().defense_boost = 0;
            changed = true;
        }
        if self.get_active().special_attack_boost < 0 {
            let amount = -self.get_active().special_attack_boost;
            instructions
                .instruction_list
                .push(Instruction::Boost(BoostInstruction::new(
                    side_ref,
                    0,
                    PokemonBoostableStat::SpecialAttack,
                    amount,
                )));
            self.get_active().special_attack_boost = 0;
            changed = true;
        }
        if self.get_active().special_defense_boost < 0 {
            let amount = -self.get_active().special_defense_boost;
            instructions
                .instruction_list
                .push(Instruction::Boost(BoostInstruction::new(
                    side_ref,
                    0,
                    PokemonBoostableStat::SpecialDefense,
                    amount,
                )));
            self.get_active().special_defense_boost = 0;
            changed = true;
        }
        if self.get_active().speed_boost < 0 {
            let amount = -self.get_active().speed_boost;
            instructions
                .instruction_list
                .push(Instruction::Boost(BoostInstruction::new(
                    side_ref,
                    0,
                    PokemonBoostableStat::Speed,
                    amount,
                )));
            self.get_active().speed_boost = 0;
            changed = true;
        }
        if self.get_active().accuracy_boost < 0 {
            let amount = -self.get_active().accuracy_boost;
            instructions
                .instruction_list
                .push(Instruction::Boost(BoostInstruction::new(
                    side_ref,
                    0,
                    PokemonBoostableStat::Accuracy,
                    amount,
                )));
            self.get_active().accuracy_boost = 0;
            changed = true;
        }
        if self.get_active().evasion_boost < 0 {
            let amount = -self.get_active().evasion_boost;
            instructions
                .instruction_list
                .push(Instruction::Boost(BoostInstruction::new(
                    side_ref,
                    0,
                    PokemonBoostableStat::Evasion,
                    amount,
                )));
            self.get_active().evasion_boost = 0;
            changed = true;
        }
        changed
    }
    pub fn active_is_charging_move(&self) -> Option<PokemonMoveIndex> {
        const CHARGE_VOLATILES: &[(PokemonVolatileStatus, Choices)] = &[
            (PokemonVolatileStatus::BOUNCE, Choices::BOUNCE),
            (PokemonVolatileStatus::DIG, Choices::DIG),
            (PokemonVolatileStatus::DIVE, Choices::DIVE),
            (PokemonVolatileStatus::FLY, Choices::FLY),
            (PokemonVolatileStatus::FREEZESHOCK, Choices::FREEZESHOCK),
            (PokemonVolatileStatus::GEOMANCY, Choices::GEOMANCY),
            (PokemonVolatileStatus::ICEBURN, Choices::ICEBURN),
            (PokemonVolatileStatus::METEORBEAM, Choices::METEORBEAM),
            (PokemonVolatileStatus::ELECTROSHOT, Choices::ELECTROSHOT),
            (PokemonVolatileStatus::PHANTOMFORCE, Choices::PHANTOMFORCE),
            (PokemonVolatileStatus::RAZORWIND, Choices::RAZORWIND),
            (PokemonVolatileStatus::SHADOWFORCE, Choices::SHADOWFORCE),
            (PokemonVolatileStatus::SKULLBASH, Choices::SKULLBASH),
            (PokemonVolatileStatus::SKYATTACK, Choices::SKYATTACK),
            (PokemonVolatileStatus::SKYDROP, Choices::SKYDROP),
            (PokemonVolatileStatus::SOLARBEAM, Choices::SOLARBEAM),
            (PokemonVolatileStatus::SOLARBLADE, Choices::SOLARBLADE),
        ];

        let vs = &self.get_active_immutable().volatile_statuses;

        for (volatile, choice_id) in CHARGE_VOLATILES {
            if vs.contains(volatile) {
                let mut iter = self.get_active_immutable().moves.into_iter();
                while let Some(mv) = iter.next() {
                    if mv.id == *choice_id {
                        return Some(iter.pokemon_move_index);
                    }
                }
            }
        }
        None
    }

    pub fn calculate_highest_stat(&self) -> PokemonBoostableStat {
        let mut highest_stat = PokemonBoostableStat::Attack;
        let mut highest_stat_value = self.calculate_boosted_stat(PokemonBoostableStat::Attack);
        for stat in [
            PokemonBoostableStat::Defense,
            PokemonBoostableStat::SpecialAttack,
            PokemonBoostableStat::SpecialDefense,
            PokemonBoostableStat::Speed,
        ] {
            let stat_value = self.calculate_boosted_stat(stat);
            if stat_value > highest_stat_value {
                highest_stat = stat;
                highest_stat_value = stat_value;
            }
        }
        highest_stat
    }
    pub fn get_boost_from_boost_enum(&self, boost_enum: &PokemonBoostableStat) -> i8 {
        match boost_enum {
            PokemonBoostableStat::Attack => self.get_active_immutable().attack_boost,
            PokemonBoostableStat::Defense => self.get_active_immutable().defense_boost,
            PokemonBoostableStat::SpecialAttack => self.get_active_immutable().special_attack_boost,
            PokemonBoostableStat::SpecialDefense => self.get_active_immutable().special_defense_boost,
            PokemonBoostableStat::Speed => self.get_active_immutable().speed_boost,
            PokemonBoostableStat::Evasion => self.get_active_immutable().evasion_boost,
            PokemonBoostableStat::Accuracy => self.get_active_immutable().accuracy_boost,
        }
    }

    pub fn calculate_boosted_stat(&self, stat: PokemonBoostableStat) -> i16 {
        self.calculate_boosted_stat_slot(0, stat)
    }

    /// Slot-aware boosted stat. In singles `slot` is always 0, so
    /// `calculate_boosted_stat(stat)` (which passes 0) is unchanged.
    ///
    /// In Gen4, Simple doubles the effective boost without it visually being
    /// doubled (capped at an effective value of 6).
    pub fn calculate_boosted_stat_slot(&self, slot: u8, stat: PokemonBoostableStat) -> i16 {
        let active = self.get_active_slot_immutable(slot);
        match stat {
            PokemonBoostableStat::Attack => {
                #[cfg(feature = "gen4")]
                let boost = if active.ability == Abilities::SIMPLE {
                    (active.attack_boost * 2).min(6).max(-6)
                } else {
                    active.attack_boost
                };

                #[cfg(not(feature = "gen4"))]
                let boost = active.attack_boost;

                multiply_boost(boost, active.attack)
            }
            PokemonBoostableStat::Defense => {
                #[cfg(feature = "gen4")]
                let boost = if active.ability == Abilities::SIMPLE {
                    (active.defense_boost * 2).min(6).max(-6)
                } else {
                    active.defense_boost
                };
                #[cfg(not(feature = "gen4"))]
                let boost = active.defense_boost;

                multiply_boost(boost, active.defense)
            }
            PokemonBoostableStat::SpecialAttack => {
                #[cfg(feature = "gen4")]
                let boost = if active.ability == Abilities::SIMPLE {
                    (active.special_attack_boost * 2).min(6).max(-6)
                } else {
                    active.special_attack_boost
                };
                #[cfg(not(feature = "gen4"))]
                let boost = active.special_attack_boost;

                multiply_boost(boost, active.special_attack)
            }
            PokemonBoostableStat::SpecialDefense => {
                #[cfg(feature = "gen4")]
                let boost = if active.ability == Abilities::SIMPLE {
                    (active.special_defense_boost * 2).min(6).max(-6)
                } else {
                    active.special_defense_boost
                };
                #[cfg(not(feature = "gen4"))]
                let boost = active.special_defense_boost;

                multiply_boost(boost, active.special_defense)
            }
            PokemonBoostableStat::Speed => {
                #[cfg(feature = "gen4")]
                let boost = if active.ability == Abilities::SIMPLE {
                    (active.speed_boost * 2).min(6).max(-6)
                } else {
                    active.speed_boost
                };
                #[cfg(not(feature = "gen4"))]
                let boost = active.speed_boost;

                multiply_boost(boost, active.speed)
            }
            _ => {
                panic!("Not implemented")
            }
        }
    }

    /// Slot-aware boosted Speed (doubles only). Mirrors the `Speed` arm of
    /// [`Side::calculate_boosted_stat`] but reads the active in `slot`. In
    /// singles `slot` is always 0, so it equals `calculate_boosted_stat(Speed)`.
    #[cfg(feature = "doubles")]
    pub fn calculate_boosted_speed_slot(&self, slot: u8) -> i16 {
        let active = self.get_active_slot_immutable(slot);
        #[cfg(feature = "gen4")]
        let boost = if active.ability == Abilities::SIMPLE {
            (active.speed_boost * 2).min(6).max(-6)
        } else {
            active.speed_boost
        };
        #[cfg(not(feature = "gen4"))]
        let boost = active.speed_boost;

        multiply_boost(boost, active.speed)
    }

    pub fn has_alive_non_rested_sleeping_pkmn(&self) -> bool {
        for p in self.pokemon.into_iter() {
            if p.status == PokemonStatus::SLEEP && p.hp > 0 && p.rest_turns == 0 {
                return true;
            }
        }
        false
    }

    #[cfg(not(feature = "terastallization"))]
    pub fn can_use_tera(&self) -> bool {
        false
    }

    #[cfg(feature = "terastallization")]
    pub fn can_use_tera(&self) -> bool {
        for p in self.pokemon.into_iter() {
            if p.terastallized {
                return false;
            }
        }
        true
    }

    pub fn add_switches(&self, vec: &mut Vec<MoveChoice>) {
        let mut iter = self.pokemon.into_iter();
        while let Some(p) = iter.next() {
            if p.hp > 0 && iter.pokemon_index != self.active_indices[0] {
                vec.push(MoveChoice::Switch(iter.pokemon_index));
            }
        }
        if vec.len() == 0 {
            vec.push(MoveChoice::None);
        }
    }

    /// Doubles variant of [`Side::add_switches`]: a benched Pokemon is a legal
    /// switch target for a slot when it is alive and not already active in *any*
    /// slot. Unlike the singles version this never pushes `MoveChoice::None` when
    /// empty — the per-slot fallback and cross-slot legality are handled by the
    /// combined-option builder.
    #[cfg(feature = "doubles")]
    pub fn add_switches_doubles(&self, vec: &mut Vec<MoveChoice>) {
        let mut iter = self.pokemon.into_iter();
        while let Some(p) = iter.next() {
            if p.hp > 0 && !self.active_indices.contains(&iter.pokemon_index) {
                vec.push(MoveChoice::Switch(iter.pokemon_index));
            }
        }
    }

    /// Slot-aware version of [`Side::active_is_charging_move`] for doubles.
    #[cfg(feature = "doubles")]
    pub fn active_is_charging_move_slot(&self, slot: u8) -> Option<PokemonMoveIndex> {
        const CHARGE_VOLATILES: &[(PokemonVolatileStatus, Choices)] = &[
            (PokemonVolatileStatus::BOUNCE, Choices::BOUNCE),
            (PokemonVolatileStatus::DIG, Choices::DIG),
            (PokemonVolatileStatus::DIVE, Choices::DIVE),
            (PokemonVolatileStatus::FLY, Choices::FLY),
            (PokemonVolatileStatus::FREEZESHOCK, Choices::FREEZESHOCK),
            (PokemonVolatileStatus::GEOMANCY, Choices::GEOMANCY),
            (PokemonVolatileStatus::ICEBURN, Choices::ICEBURN),
            (PokemonVolatileStatus::METEORBEAM, Choices::METEORBEAM),
            (PokemonVolatileStatus::ELECTROSHOT, Choices::ELECTROSHOT),
            (PokemonVolatileStatus::PHANTOMFORCE, Choices::PHANTOMFORCE),
            (PokemonVolatileStatus::RAZORWIND, Choices::RAZORWIND),
            (PokemonVolatileStatus::SHADOWFORCE, Choices::SHADOWFORCE),
            (PokemonVolatileStatus::SKULLBASH, Choices::SKULLBASH),
            (PokemonVolatileStatus::SKYATTACK, Choices::SKYATTACK),
            (PokemonVolatileStatus::SKYDROP, Choices::SKYDROP),
            (PokemonVolatileStatus::SOLARBEAM, Choices::SOLARBEAM),
            (PokemonVolatileStatus::SOLARBLADE, Choices::SOLARBLADE),
        ];
        let active = self.get_active_slot_immutable(slot);
        let vs = &active.volatile_statuses;
        for (volatile, choice_id) in CHARGE_VOLATILES {
            if vs.contains(volatile) {
                let mut iter = active.moves.into_iter();
                while let Some(mv) = iter.next() {
                    if mv.id == *choice_id {
                        return Some(iter.pokemon_move_index);
                    }
                }
            }
        }
        None
    }

    pub fn trapped(&self, opponent_active: &Pokemon) -> bool {
        self.trapped_slot(0, opponent_active)
    }

    /// Whether the active in `slot` is trapped by `opponent_active`. The singles
    /// `trapped` delegates here with `slot == 0`; doubles checks each living foe.
    pub fn trapped_slot(&self, slot: u8, opponent_active: &Pokemon) -> bool {
        let active_pkmn = self.get_active_slot_immutable(slot);
        if active_pkmn
            .volatile_statuses
            .contains(&PokemonVolatileStatus::LOCKEDMOVE)
            || active_pkmn
                .volatile_statuses
                .contains(&PokemonVolatileStatus::NORETREAT)
        {
            return true;
        }
        if active_pkmn.item == Items::SHEDSHELL || active_pkmn.has_type(&PokemonType::GHOST) {
            return false;
        } else if active_pkmn
            .volatile_statuses
            .contains(&PokemonVolatileStatus::PARTIALLYTRAPPED)
        {
            return true;
        } else if opponent_active.ability == Abilities::SHADOWTAG {
            return true;
        } else if opponent_active.ability == Abilities::ARENATRAP && active_pkmn.is_grounded() {
            return true;
        } else if opponent_active.ability == Abilities::MAGNETPULL
            && active_pkmn.has_type(&PokemonType::STEEL)
        {
            return true;
        }
        false
    }

    pub fn num_fainted_pkmn(&self) -> i8 {
        let mut count = 0;
        for p in self.pokemon.into_iter() {
            if p.hp == 0 {
                count += 1;
            }
        }
        count
    }
}

impl State {
    pub fn root_get_all_options(&self) -> (Vec<MoveChoice>, Vec<MoveChoice>) {
        if self.team_preview {
            let mut s1_options = Vec::with_capacity(6);
            let mut s2_options = Vec::with_capacity(6);

            let mut pkmn_iter = self.side_one.pokemon.into_iter();
            while let Some(_) = pkmn_iter.next() {
                if self.side_one.pokemon[pkmn_iter.pokemon_index].hp > 0 {
                    s1_options.push(MoveChoice::Switch(pkmn_iter.pokemon_index));
                }
            }
            let mut pkmn_iter = self.side_two.pokemon.into_iter();
            while let Some(_) = pkmn_iter.next() {
                if self.side_two.pokemon[pkmn_iter.pokemon_index].hp > 0 {
                    s2_options.push(MoveChoice::Switch(pkmn_iter.pokemon_index));
                }
            }
            return (s1_options, s2_options);
        }

        let (mut s1_options, mut s2_options) = self.get_all_options();

        if self.side_one.force_trapped {
            s1_options.retain(|x| match x {
                MoveChoice::Move { .. }
                | MoveChoice::MoveTera { .. }
                | MoveChoice::MoveMega { .. } => true,
                MoveChoice::Switch(_) => false,
                MoveChoice::None => true,
            });
        }
        if self.side_one.slow_uturn_move {
            s1_options.clear();
            let encored = self
                .side_one
                .get_active_immutable().volatile_statuses
                .contains(&PokemonVolatileStatus::ENCORE);
            let taunted = self
                .side_one
                .get_active_immutable().volatile_statuses
                .contains(&PokemonVolatileStatus::TAUNT);
            self.side_one.get_active_immutable().add_available_moves(
                &mut s1_options,
                &self.side_one.get_active_immutable().last_used_move,
                encored,
                taunted,
                self.side_one.can_use_tera(),
                BattlePosition::new(SideReference::SideTwo, 0),
            );
        }

        if self.side_two.force_trapped {
            s2_options.retain(|x| match x {
                MoveChoice::Move { .. }
                | MoveChoice::MoveTera { .. }
                | MoveChoice::MoveMega { .. } => true,
                MoveChoice::Switch(_) => false,
                MoveChoice::None => true,
            });
        }
        if self.side_two.slow_uturn_move {
            s2_options.clear();
            let encored = self
                .side_two
                .get_active_immutable().volatile_statuses
                .contains(&PokemonVolatileStatus::ENCORE);
            let taunted = self
                .side_two
                .get_active_immutable().volatile_statuses
                .contains(&PokemonVolatileStatus::TAUNT);
            self.side_two.get_active_immutable().add_available_moves(
                &mut s2_options,
                &self.side_two.get_active_immutable().last_used_move,
                encored,
                taunted,
                self.side_two.can_use_tera(),
                BattlePosition::new(SideReference::SideOne, 0),
            );
        }

        if s1_options.len() == 0 {
            s1_options.push(MoveChoice::None);
        }
        if s2_options.len() == 0 {
            s2_options.push(MoveChoice::None);
        }

        (s1_options, s2_options)
    }

    pub fn get_all_options(&self) -> (Vec<MoveChoice>, Vec<MoveChoice>) {
        let mut side_one_options: Vec<MoveChoice> = Vec::with_capacity(9);
        let mut side_two_options: Vec<MoveChoice> = Vec::with_capacity(9);

        let side_one_active = self.side_one.get_active_immutable();
        let side_two_active = self.side_two.get_active_immutable();

        if self.side_one.force_switch {
            self.side_one.add_switches(&mut side_one_options);
            if self.side_two.switch_out_move_second_saved_move == Choices::NONE {
                side_two_options.push(MoveChoice::None);
            } else {
                self.side_two.get_active_immutable().add_move_from_choice(
                    &mut side_two_options,
                    self.side_two.switch_out_move_second_saved_move,
                    BattlePosition::new(SideReference::SideOne, 0),
                );
            }
            return (side_one_options, side_two_options);
        }

        if self.side_two.force_switch {
            self.side_two.add_switches(&mut side_two_options);
            if self.side_one.switch_out_move_second_saved_move == Choices::NONE {
                side_one_options.push(MoveChoice::None);
            } else {
                self.side_one.get_active_immutable().add_move_from_choice(
                    &mut side_one_options,
                    self.side_one.switch_out_move_second_saved_move,
                    BattlePosition::new(SideReference::SideTwo, 0),
                );
            }
            return (side_one_options, side_two_options);
        }

        let side_one_force_switch = self.side_one.get_active_immutable().hp <= 0;
        let side_two_force_switch = self.side_two.get_active_immutable().hp <= 0;

        if side_one_force_switch && side_two_force_switch {
            self.side_one.add_switches(&mut side_one_options);
            self.side_two.add_switches(&mut side_two_options);
            return (side_one_options, side_two_options);
        }
        if side_one_force_switch {
            self.side_one.add_switches(&mut side_one_options);
            side_two_options.push(MoveChoice::None);
            return (side_one_options, side_two_options);
        }
        if side_two_force_switch {
            side_one_options.push(MoveChoice::None);
            self.side_two.add_switches(&mut side_two_options);
            return (side_one_options, side_two_options);
        }

        if self
            .side_one
            .get_active_immutable().volatile_statuses
            .contains(&PokemonVolatileStatus::MUSTRECHARGE)
        {
            side_one_options.push(MoveChoice::None);
        } else if let Some(mv_index) = self.side_one.active_is_charging_move() {
            side_one_options.push(MoveChoice::Move {
                move_index: mv_index,
                target: BattlePosition::new(SideReference::SideTwo, 0),
            });
        } else {
            let encored = self
                .side_one
                .get_active_immutable().volatile_statuses
                .contains(&PokemonVolatileStatus::ENCORE);
            let taunted = self
                .side_one
                .get_active_immutable().volatile_statuses
                .contains(&PokemonVolatileStatus::TAUNT);
            self.side_one.get_active_immutable().add_available_moves(
                &mut side_one_options,
                &self.side_one.get_active_immutable().last_used_move,
                encored,
                taunted,
                self.side_one.can_use_tera(),
                BattlePosition::new(SideReference::SideTwo, 0),
            );
            if !self.side_one.trapped(side_two_active) {
                self.side_one.add_switches(&mut side_one_options);
            }
        }

        if self
            .side_two
            .get_active_immutable().volatile_statuses
            .contains(&PokemonVolatileStatus::MUSTRECHARGE)
        {
            side_two_options.push(MoveChoice::None);
        } else if let Some(mv_index) = self.side_two.active_is_charging_move() {
            side_two_options.push(MoveChoice::Move {
                move_index: mv_index,
                target: BattlePosition::new(SideReference::SideOne, 0),
            });
        } else {
            let encored = self
                .side_two
                .get_active_immutable().volatile_statuses
                .contains(&PokemonVolatileStatus::ENCORE);
            let taunted = self
                .side_two
                .get_active_immutable().volatile_statuses
                .contains(&PokemonVolatileStatus::TAUNT);
            self.side_two.get_active_immutable().add_available_moves(
                &mut side_two_options,
                &self.side_two.get_active_immutable().last_used_move,
                encored,
                taunted,
                self.side_two.can_use_tera(),
                BattlePosition::new(SideReference::SideOne, 0),
            );
            if !self.side_two.trapped(side_one_active) {
                self.side_two.add_switches(&mut side_two_options);
            }
        }

        if side_one_options.len() == 0 {
            side_one_options.push(MoveChoice::None);
        }
        if side_two_options.len() == 0 {
            side_two_options.push(MoveChoice::None);
        }

        (side_one_options, side_two_options)
    }

    /// Entry point for enumerating doubles options at the root of a turn.
    ///
    /// Living positions a spread move with `target`, used from `attacker_side`'s
    /// acting slot (`self.acting_slot`), actually hits. `BothFoes`/`AllAdjacentFoes`
    /// hit the living foes; `AllAdjacent`/`AllOthers` additionally hit the living
    /// ally. Used both to expand the move over its targets and to decide the 0.75x
    /// spread reduction (which applies only when more than one target is hit).
    #[cfg(feature = "doubles")]
    pub fn spread_target_positions(
        &self,
        attacker_side: SideReference,
        target: MoveTarget,
    ) -> Vec<BattlePosition> {
        let foe_side = attacker_side.get_other_side();
        let includes_allies = matches!(target, MoveTarget::AllAdjacent | MoveTarget::AllOthers);
        let mut out = Vec::with_capacity(ACTIVE_PER_SIDE * 2);
        for slot in 0..ACTIVE_PER_SIDE as u8 {
            if self
                .get_side_immutable(&foe_side)
                .get_active_slot_immutable(slot)
                .hp
                > 0
            {
                out.push(BattlePosition::new(foe_side, slot));
            }
        }
        if includes_allies {
            for slot in 0..ACTIVE_PER_SIDE as u8 {
                if slot != self.acting_slot
                    && self
                        .get_side_immutable(&attacker_side)
                        .get_active_slot_immutable(slot)
                        .hp
                        > 0
                {
                    out.push(BattlePosition::new(attacker_side, slot));
                }
            }
        }
        out
    }

    /// Number of living targets a spread move would hit (see
    /// [`State::spread_target_positions`]).
    #[cfg(feature = "doubles")]
    pub fn living_spread_target_count(
        &self,
        attacker_side: SideReference,
        target: MoveTarget,
    ) -> usize {
        self.spread_target_positions(attacker_side, target).len()
    }

    /// Handles team preview (each side picks `ACTIVE_PER_SIDE` distinct leads) and
    /// otherwise delegates to [`State::get_all_options_doubles`]. The mid-turn
    /// refinements the singles `root_get_all_options` applies (force_trapped,
    /// slow_uturn pivot continuation) belong to doubles turn resolution and are
    /// deferred to a later phase; this phase only enumerates options.
    #[cfg(feature = "doubles")]
    pub fn root_get_all_options_doubles(&self) -> (Vec<SideAction>, Vec<SideAction>) {
        if self.team_preview {
            return (
                self.team_preview_options(SideReference::SideOne),
                self.team_preview_options(SideReference::SideTwo),
            );
        }
        self.get_all_options_doubles()
    }

    /// Doubles equivalent of [`State::get_all_options`].
    ///
    /// Returns, for each side, the list of legal combined actions: the cartesian
    /// product of every active slot's [`MoveChoice`]s, pruned of combinations that
    /// violate cross-slot legality (currently: two slots switching to the same
    /// benched Pokemon).
    #[cfg(feature = "doubles")]
    pub fn get_all_options_doubles(&self) -> (Vec<SideAction>, Vec<SideAction>) {
        // A faint/forced-out replacement is its own decision point: the side(s)
        // with empty slots send replacements while the other side waits (it does
        // not get a normal turn). Mirrors the singles force_switch short-circuit.
        let s1_replacing = self.replacement_slots(SideReference::SideOne).is_some();
        let s2_replacing = self.replacement_slots(SideReference::SideTwo).is_some();
        if s1_replacing || s2_replacing {
            let waiting = || vec![SideAction::new([MoveChoice::None; ACTIVE_PER_SIDE])];
            let s1 = if s1_replacing {
                self.side_options_doubles(SideReference::SideOne)
            } else {
                waiting()
            };
            let s2 = if s2_replacing {
                self.side_options_doubles(SideReference::SideTwo)
            } else {
                waiting()
            };
            return (s1, s2);
        }
        (
            self.side_options_doubles(SideReference::SideOne),
            self.side_options_doubles(SideReference::SideTwo),
        )
    }

    /// The legal combined actions for one side: cartesian product of its slots'
    /// per-slot options, with illegal combinations pruned.
    ///
    /// When the side is in *replacement mode* — one or more slots fainted (or were
    /// forced out by a pivot move) and a benched Pokemon is available — the only
    /// legal actions are to send replacements into the empty slots. Several faints
    /// on a side in one turn are replaced together: each emptied slot picks a
    /// distinct living bench mon, the surviving slot does nothing. If there are
    /// fewer bench mons than empty slots, as many as possible are replaced.
    #[cfg(feature = "doubles")]
    fn side_options_doubles(&self, side_ref: SideReference) -> Vec<SideAction> {
        if let Some(needing) = self.replacement_slots(side_ref) {
            return self.replacement_options_doubles(side_ref, &needing);
        }
        let mut per_slot: Vec<Vec<MoveChoice>> = Vec::with_capacity(ACTIVE_PER_SIDE);
        for slot in 0..ACTIVE_PER_SIDE as u8 {
            per_slot.push(self.slot_options_doubles(side_ref, slot));
        }
        Self::combine_slot_options(&per_slot)
    }

    /// The slots on `side_ref` that must be replaced this decision point (fainted
    /// or pivot-forced), or `None` if the side is in a normal turn. Returns `None`
    /// when no replacement is possible (no living bench), so play falls through to
    /// the normal turn enumeration (the empty slot simply contributes `None`).
    #[cfg(feature = "doubles")]
    fn replacement_slots(&self, side_ref: SideReference) -> Option<Vec<u8>> {
        let side = self.get_side_immutable(&side_ref);
        let needing: Vec<u8> = (0..ACTIVE_PER_SIDE as u8)
            .filter(|&slot| {
                side.force_switch_slot(slot) || side.get_active_slot_immutable(slot).hp <= 0
            })
            .collect();
        if needing.is_empty() {
            return None;
        }
        // Only a replacement decision point if at least one bench mon is available.
        let mut bench = Vec::new();
        side.add_switches_doubles(&mut bench);
        if bench.is_empty() {
            None
        } else {
            Some(needing)
        }
    }

    /// Enumerate the replacement actions: each `needing` slot picks a distinct
    /// living bench mon (surviving slots act with `None`). When bench mons are
    /// scarce, exactly `min(needing, available)` slots are filled — you replace as
    /// many as you can, never declining a replacement you could make.
    #[cfg(feature = "doubles")]
    fn replacement_options_doubles(&self, side_ref: SideReference, needing: &[u8]) -> Vec<SideAction> {
        let side = self.get_side_immutable(&side_ref);
        let mut switches = Vec::new();
        side.add_switches_doubles(&mut switches);
        let available = switches.len();
        let fill = needing.len().min(available);

        // Per-slot option lists: a needing slot may take any switch or (as a
        // fallback under scarcity) None; other slots only do nothing.
        let mut per_slot: Vec<Vec<MoveChoice>> = Vec::with_capacity(ACTIVE_PER_SIDE);
        for slot in 0..ACTIVE_PER_SIDE as u8 {
            if needing.contains(&slot) {
                let mut opts = switches.clone();
                opts.push(MoveChoice::None);
                per_slot.push(opts);
            } else {
                per_slot.push(vec![MoveChoice::None]);
            }
        }

        // Keep combos that replace exactly `fill` slots with distinct mons.
        Self::combine_slot_options(&per_slot)
            .into_iter()
            .filter(|sa| {
                sa.actions
                    .iter()
                    .filter(|a| matches!(a, MoveChoice::Switch(_)))
                    .count()
                    == fill
            })
            .collect()
    }

    /// Every `MoveChoice` available to a single active slot: each selectable move
    /// fanned out over its legal targets, plus switches. An empty (fainted, not
    /// yet replaced) slot yields a single `MoveChoice::None`.
    #[cfg(feature = "doubles")]
    fn slot_options_doubles(&self, side_ref: SideReference, slot: u8) -> Vec<MoveChoice> {
        let side = self.get_side_immutable(&side_ref);
        let active = side.get_active_slot_immutable(slot);
        let mut options: Vec<MoveChoice> = Vec::new();

        // Empty slot: a fainted active with no replacement yet contributes only a
        // "do nothing" sub-action so the combination still has an entry per slot.
        if active.hp <= 0 {
            options.push(MoveChoice::None);
            return options;
        }

        let user_pos = BattlePosition::new(side_ref, slot);

        if active
            .volatile_statuses
            .contains(&PokemonVolatileStatus::MUSTRECHARGE)
        {
            options.push(MoveChoice::None);
            return options;
        }

        if let Some(mv_index) = side.active_is_charging_move_slot(slot) {
            let move_target = active.moves[&mv_index].choice.target.clone();
            let target = self
                .legal_targets(user_pos, &move_target)
                .into_iter()
                .next()
                .unwrap_or_else(|| BattlePosition::new(side_ref.get_other_side(), 0));
            options.push(MoveChoice::Move {
                move_index: mv_index,
                target,
            });
            return options;
        }

        let encored = active
            .volatile_statuses
            .contains(&PokemonVolatileStatus::ENCORE);
        let taunted = active
            .volatile_statuses
            .contains(&PokemonVolatileStatus::TAUNT);
        active.add_available_moves_doubles(
            &mut options,
            &active.last_used_move,
            encored,
            taunted,
            side.can_use_tera(),
            |move_target| self.legal_targets(user_pos, move_target),
        );
        if !self.slot_trapped(side_ref, slot) {
            side.add_switches_doubles(&mut options);
        }

        if options.is_empty() {
            options.push(MoveChoice::None);
        }
        options
    }

    /// The living positions on the side opposing `user_side`.
    #[cfg(feature = "doubles")]
    fn alive_foe_positions(&self, user_side: SideReference) -> Vec<BattlePosition> {
        let foe_side = user_side.get_other_side();
        let foe = self.get_side_immutable(&foe_side);
        let mut out = Vec::with_capacity(ACTIVE_PER_SIDE);
        for slot in 0..ACTIVE_PER_SIDE as u8 {
            if foe.get_active_slot_immutable(slot).hp > 0 {
                out.push(BattlePosition::new(foe_side, slot));
            }
        }
        out
    }

    /// The [`BattlePosition`]s that should each spawn a distinct option for a move
    /// with `move_target` used from `user_pos`.
    ///
    /// Single-target foe moves fan out over every living foe (the player picks
    /// which). Self / own-side / spread / side / random moves are not a per-target
    /// choice and yield a single nominal position. An ally-target move with no
    /// living ally yields no positions, making the move unselectable.
    #[cfg(feature = "doubles")]
    fn legal_targets(
        &self,
        user_pos: BattlePosition,
        move_target: &MoveTarget,
    ) -> Vec<BattlePosition> {
        let user_side = user_pos.side;
        let foe_side = user_side.get_other_side();
        let alive_foes = self.alive_foe_positions(user_side);
        let first_foe = alive_foes
            .first()
            .copied()
            .unwrap_or_else(|| BattlePosition::new(foe_side, 0));

        match move_target {
            // Single foe: the player chooses which living foe to hit.
            MoveTarget::Opponent => {
                if alive_foes.is_empty() {
                    vec![first_foe]
                } else {
                    alive_foes
                }
            }
            // Adjacent ally: unusable if the ally slot is empty/fainted.
            MoveTarget::Ally => {
                let ally_slot = if user_pos.slot == 0 { 1 } else { 0 };
                if self
                    .get_side_immutable(&user_side)
                    .get_active_slot_immutable(ally_slot)
                    .hp
                    > 0
                {
                    vec![BattlePosition::new(user_side, ally_slot)]
                } else {
                    vec![]
                }
            }
            // Self / own side as a whole: nominal target is the user's own position.
            MoveTarget::User | MoveTarget::UserSide => vec![user_pos],
            // Opposing side as a whole (hazards): nominal slot-0 of the foe side.
            MoveTarget::FoeSide => vec![BattlePosition::new(foe_side, 0)],
            // Spread / random / everyone: not a per-target choice -> one option.
            MoveTarget::BothFoes
            | MoveTarget::AllAdjacentFoes
            | MoveTarget::RandomFoe
            | MoveTarget::AllAdjacent
            | MoveTarget::AllOthers => vec![first_foe],
        }
    }

    /// Whether the active in `slot` is trapped by any living foe.
    #[cfg(feature = "doubles")]
    fn slot_trapped(&self, side_ref: SideReference, slot: u8) -> bool {
        let side = self.get_side_immutable(&side_ref);
        let foe = self.get_side_immutable(&side_ref.get_other_side());
        for fslot in 0..ACTIVE_PER_SIDE as u8 {
            let foe_active = foe.get_active_slot_immutable(fslot);
            if foe_active.hp > 0 && side.trapped_slot(slot, foe_active) {
                return true;
            }
        }
        false
    }

    /// Doubles team-preview options for one side: choose `ACTIVE_PER_SIDE` distinct
    /// living Pokemon as leads (cartesian product, distinct-switch pruning applied).
    #[cfg(feature = "doubles")]
    fn team_preview_options(&self, side_ref: SideReference) -> Vec<SideAction> {
        let side = self.get_side_immutable(&side_ref);
        let mut leads: Vec<MoveChoice> = Vec::new();
        let mut iter = side.pokemon.into_iter();
        while let Some(p) = iter.next() {
            if p.hp > 0 {
                leads.push(MoveChoice::Switch(iter.pokemon_index));
            }
        }
        let per_slot: Vec<Vec<MoveChoice>> = (0..ACTIVE_PER_SIDE).map(|_| leads.clone()).collect();
        Self::combine_slot_options(&per_slot)
    }

    /// Cartesian product of per-slot option lists into combined [`SideAction`]s,
    /// pruning combinations where two slots switch to the same benched Pokemon.
    #[cfg(feature = "doubles")]
    fn combine_slot_options(per_slot: &[Vec<MoveChoice>]) -> Vec<SideAction> {
        let mut combos: Vec<Vec<MoveChoice>> = vec![Vec::with_capacity(ACTIVE_PER_SIDE)];
        for slot_opts in per_slot {
            let mut next = Vec::with_capacity(combos.len() * slot_opts.len());
            for partial in &combos {
                for opt in slot_opts {
                    let mut extended = partial.clone();
                    extended.push(*opt);
                    next.push(extended);
                }
            }
            combos = next;
        }

        combos
            .into_iter()
            .filter(|combo| !Self::has_duplicate_switch(combo))
            .map(|combo| {
                let mut actions = [MoveChoice::None; ACTIVE_PER_SIDE];
                for (i, mc) in combo.into_iter().enumerate() {
                    actions[i] = mc;
                }
                SideAction::new(actions)
            })
            .collect()
    }

    /// Whether a combined action has two slots switching to the same Pokemon.
    #[cfg(feature = "doubles")]
    fn has_duplicate_switch(combo: &[MoveChoice]) -> bool {
        let mut switched: Vec<PokemonIndex> = Vec::new();
        for mc in combo {
            if let MoveChoice::Switch(idx) = mc {
                if switched.contains(idx) {
                    return true;
                }
                switched.push(*idx);
            }
        }
        false
    }

    pub fn reset_toxic_count(
        &mut self,
        side_ref: &SideReference,
        vec_to_add_to: &mut Vec<Instruction>,
    ) {
        let side = self.get_side(side_ref);
        if side.side_conditions.toxic_count > 0 {
            vec_to_add_to.push(Instruction::ChangeSideCondition(
                ChangeSideConditionInstruction {
                    side_ref: *side_ref,
                    side_condition: PokemonSideCondition::ToxicCount,
                    amount: -1 * side.side_conditions.toxic_count,
                },
            ));
            side.side_conditions.toxic_count = 0;
        }
    }

    pub fn remove_volatile_statuses_on_switch(
        &mut self,
        side_ref: &SideReference,
        instructions: &mut Vec<Instruction>,
        baton_passing: bool,
        shed_tailing: bool,
    ) {
        let side = self.get_side(side_ref);

        // Take ownership of the current set to avoid borrow conflicts
        // since we may need to modify the side in the loop
        let mut volatile_statuses = std::mem::take(&mut side.get_active().volatile_statuses);

        volatile_statuses.retain(&mut |pkmn_volatile_status| {
            let should_retain = match pkmn_volatile_status {
                PokemonVolatileStatus::SUBSTITUTE => baton_passing || shed_tailing,
                PokemonVolatileStatus::LEECHSEED => baton_passing,
                PokemonVolatileStatus::TYPECHANGE => {
                    let active = side.get_active();
                    if active.base_types != active.types {
                        let (new_types, old_types) = (active.base_types, active.types);
                        instructions.push(Instruction::ChangeType(ChangeType::new(
                            *side_ref, 0, new_types, old_types,
                        )));
                        active.types = active.base_types;
                    }
                    false
                }
                // While you can't switch out of a locked move you can be forced out in other ways
                PokemonVolatileStatus::LOCKEDMOVE => {
                    let amount = -1 * side.get_active().volatile_status_durations.lockedmove;
                    instructions.push(Instruction::ChangeVolatileStatusDuration(
                        ChangeVolatileStatusDurationInstruction::new(
                            *side_ref,
                            0,
                            *pkmn_volatile_status,
                            amount,
                        ),
                    ));
                    side.get_active().volatile_status_durations.lockedmove = 0;
                    false
                }
                PokemonVolatileStatus::YAWN => {
                    let amount = -1 * side.get_active().volatile_status_durations.yawn;
                    instructions.push(Instruction::ChangeVolatileStatusDuration(
                        ChangeVolatileStatusDurationInstruction::new(
                            *side_ref,
                            0,
                            *pkmn_volatile_status,
                            amount,
                        ),
                    ));
                    side.get_active().volatile_status_durations.yawn = 0;
                    false
                }
                PokemonVolatileStatus::TAUNT => {
                    let amount = -1 * side.get_active().volatile_status_durations.taunt;
                    instructions.push(Instruction::ChangeVolatileStatusDuration(
                        ChangeVolatileStatusDurationInstruction::new(
                            *side_ref,
                            0,
                            *pkmn_volatile_status,
                            amount,
                        ),
                    ));
                    side.get_active().volatile_status_durations.taunt = 0;
                    false
                }
                _ => false,
            };

            if !should_retain {
                instructions.push(Instruction::RemoveVolatileStatus(
                    RemoveVolatileStatusInstruction::new(*side_ref, 0, *pkmn_volatile_status),
                ));
            }
            should_retain
        });

        // Clean up by re-setting the volatile statuses
        side.get_active().volatile_statuses = volatile_statuses;
    }

    pub fn terrain_is_active(&self, terrain: &Terrain) -> bool {
        &self.terrain.terrain_type == terrain && self.terrain.turns_remaining > 0
    }

    pub fn get_terrain(&self) -> Terrain {
        if self.terrain.turns_remaining > 0 {
            self.terrain.terrain_type
        } else {
            Terrain::NONE
        }
    }

    pub fn weather_is_active(&self, weather: &Weather) -> bool {
        let s1_active = self.side_one.get_active_immutable();
        let s2_active = self.side_two.get_active_immutable();
        &self.weather.weather_type == weather
            && s1_active.ability != Abilities::AIRLOCK
            && s1_active.ability != Abilities::CLOUDNINE
            && s2_active.ability != Abilities::AIRLOCK
            && s2_active.ability != Abilities::CLOUDNINE
    }

    fn _state_contains_any_move(&self, moves: &[Choices]) -> bool {
        for s in [&self.side_one, &self.side_two] {
            for pkmn in s.pokemon.into_iter() {
                for mv in pkmn.moves.into_iter() {
                    if moves.contains(&mv.id) {
                        return true;
                    }
                }
            }
        }

        false
    }

    pub fn set_damage_dealt_flag(&mut self) {
        if self._state_contains_any_move(&[
            Choices::COUNTER,
            Choices::MIRRORCOAT,
            Choices::METALBURST,
            Choices::COMEUPPANCE,
            Choices::FOCUSPUNCH,
            Choices::AVALANCHE,
        ]) {
            self.use_damage_dealt = true
        }
    }

    pub fn set_last_used_move_flag(&mut self) {
        if self._state_contains_any_move(&[
            Choices::ENCORE,
            Choices::FAKEOUT,
            Choices::FIRSTIMPRESSION,
            Choices::BLOODMOON,
            Choices::GIGATONHAMMER,
        ]) {
            self.use_last_used_move = true
        }
    }

    pub fn set_conditional_mechanics(&mut self) {
        /*
        These mechanics are not always relevant but when they are it
        is important that they are enabled. Enabling them all the time would
        suffer about a 20% performance hit.
        */
        self.set_damage_dealt_flag();
        self.set_last_used_move_flag();
    }
}
