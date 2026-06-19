"""Doubles-build pytest suite.

These tests only make sense against a *doubles* build of the bindings (built
with the ``doubles`` cargo feature). They probe the build once at import time and
skip the whole module on a singles build, so the file is harmless when collected
by the regular (singles) ``pytest`` run.

Build the doubles module with::

    make pytest_doubles
"""

import pytest

from poke_engine import (
    State,
    Side,
    Move,
    Pokemon,
    monte_carlo_tree_search,
    iterative_deepening_expectiminimax,
    generate_instructions,
)


def _mon(id, type0, moves):
    return Pokemon(
        id=id,
        level=100,
        types=(type0, "typeless"),
        hp=100,
        maxhp=100,
        attack=100,
        defense=100,
        special_attack=100,
        special_defense=100,
        speed=100,
        status="none",
        moves=[Move(id=m, pp=32) for m in moves],
    )


def _doubles_state():
    return State(
        side_one=Side(
            active_indices=["0", "1"],
            pokemon=[
                _mon("charmander", "fire", ["ember", "tackle", "protect"]),
                _mon("squirtle", "water", ["watergun", "tackle", "helpinghand"]),
                _mon("bulbasaur", "grass", ["vinewhip", "tackle", "leer"]),
            ],
        ),
        side_two=Side(
            active_indices=["0", "1"],
            pokemon=[
                _mon("pikachu", "electric", ["thunderbolt", "tackle", "protect"]),
                _mon("geodude", "rock", ["rockslide", "earthquake", "tackle"]),
                _mon("pidgey", "flying", ["gust", "tackle", "quickattack"]),
            ],
        ),
    )


def _build_is_doubles():
    """True iff the loaded module was built with the doubles feature.

    In a doubles build a `;`-joined combined action parses; in a singles build it
    is not a valid single move and `generate_instructions` raises ValueError.
    """
    try:
        generate_instructions(_doubles_state(), "tackle;tackle", "tackle;tackle")
        return True
    except Exception:
        return False


pytestmark = pytest.mark.skipif(
    not _build_is_doubles(), reason="requires a doubles build of poke_engine"
)


def test_active_indices_has_two_slots():
    state = _doubles_state()
    assert state.side_one.active_indices == ["0", "1"]
    assert state.side_two.active_indices == ["0", "1"]


def test_serialization_round_trip():
    # Put distinct per-active state on each side-one active so the round trip has
    # something non-default to preserve in BOTH slots (not just slot 0).
    state = State(
        side_one=Side(
            active_indices=["0", "1"],
            pokemon=[
                Pokemon(
                    id="charmander",
                    hp=100,
                    maxhp=100,
                    moves=[Move(id="ember", pp=32)],
                    attack_boost=2,
                ),
                Pokemon(
                    id="squirtle",
                    hp=100,
                    maxhp=100,
                    moves=[Move(id="watergun", pp=32)],
                    speed_boost=-1,
                    volatile_statuses={"taunt"},
                ),
            ],
        ),
        side_two=Side(
            active_indices=["0", "1"],
            pokemon=[
                _mon("pikachu", "electric", ["thunderbolt"]),
                _mon("geodude", "rock", ["rockslide"]),
            ],
        ),
    )

    serialized = state.to_string()
    again = State.from_string(serialized).to_string()
    assert serialized == again


def test_per_active_state_lives_on_pokemon():
    # Per-active state is carried on each Pokemon (here on slot 1's active), and
    # survives a serialize -> deserialize round trip.
    state = State(
        side_one=Side(
            active_indices=["0", "1"],
            pokemon=[
                _mon("charmander", "fire", ["ember"]),
                Pokemon(
                    id="squirtle",
                    hp=100,
                    maxhp=100,
                    moves=[Move(id="watergun", pp=32)],
                    attack_boost=3,
                ),
            ],
        ),
        side_two=Side(
            active_indices=["0", "1"],
            pokemon=[
                _mon("pikachu", "electric", ["thunderbolt"]),
                _mon("geodude", "rock", ["rockslide"]),
            ],
        ),
    )
    round_tripped = State.from_string(state.to_string())
    assert round_tripped.side_one.pokemon[1].attack_boost == 3


def test_mcts_returns_combined_actions():
    state = _doubles_state()
    result = monte_carlo_tree_search(state, 50)
    assert result.total_visits > 0
    # Every option is a combined per-slot action: two sub-actions joined by ";".
    for r in result.side_one + result.side_two:
        assert ";" in r.move_choice


def test_iterative_deepening_runs():
    iterative_deepening_expectiminimax(_doubles_state(), 50)


def test_generate_instructions_with_targets():
    state = _doubles_state()
    # slot0 embers the right-hand foe (slot 1) + slot1 helping-hands its ally;
    # the foes protect / earthquake (a spread move).
    instructions = generate_instructions(
        state, "ember,1;helpinghand", "protect;earthquake"
    )
    assert len(instructions) > 0
    total = sum(i.percentage for i in instructions)
    assert abs(total - 100.0) < 1.0


def test_invalid_combined_action_raises():
    with pytest.raises(ValueError):
        generate_instructions(_doubles_state(), "not_a_move;tackle", "tackle;tackle")
