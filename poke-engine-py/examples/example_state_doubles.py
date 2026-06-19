"""A 2v2 doubles example state.

Requires a *doubles* build of the bindings, e.g.::

    cd poke-engine-py && maturin develop --features="poke-engine/gen9,doubles"

The only structural differences from a singles state are:

* ``active_indices`` lists both active slots (here ``["0", "1"]``), so the first
  two Pokemon of each side start on the field.
* A side's move decision is a combined per-slot action: the per-slot sub-actions
  joined with ``;``, each optionally suffixed with ``,<slot>`` to pick which
  opposing slot it targets (e.g. ``"closecombat,1;protect"``).
"""

from poke_engine import (
    State,
    Side,
    Move,
    Pokemon,
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


state = State(
    side_one=Side(
        # Both leads (slots 0 and 1) start active.
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
