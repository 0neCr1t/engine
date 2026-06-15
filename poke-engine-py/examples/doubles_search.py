"""Run a search and generate instructions on a doubles state.

Requires a *doubles* build of the bindings, e.g.::

    cd poke-engine-py && maturin develop --features="poke-engine/gen9,doubles"

In a doubles build the per-side options and the chosen ``move_choice`` are
combined per-slot actions rendered as ``slot0;slot1`` (each sub-action may carry
a ``,<slot>`` target). They feed straight back into ``generate_instructions``.
"""

from poke_engine import monte_carlo_tree_search, generate_instructions

from example_state_doubles import state


result = monte_carlo_tree_search(state, duration_ms=1000)
print(f"Total Iterations: {result.total_visits}")
print("side one:", [(i.move_choice, round(i.total_score, 1), i.visits) for i in result.side_one])
print("side two:", [(i.move_choice, round(i.total_score, 1), i.visits) for i in result.side_two])

# A combined side action: slot 0 uses ember on the right-hand foe (slot 1), slot
# 1 uses helping hand on its ally. Side two's slot 0 protects while slot 1
# earthquakes (a spread move that hits both foes and its own ally).
instructions = generate_instructions(
    state,
    "ember,1;helpinghand",
    "protect;earthquake",
)
for i in instructions:
    print()
    print(i.percentage)
    for ins in i.instruction_list:
        print(f"\t{ins}")
