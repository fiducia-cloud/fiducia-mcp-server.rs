from itertools import product


def may_mutate_lease(authenticated, holder_match, epoch_current, write_tool):
    return authenticated and holder_match and epoch_current and write_tool

for state in product((False, True), repeat=4):
    allowed = may_mutate_lease(*state)
    if allowed:
        assert all(state)

assert not may_mutate_lease(True, False, True, True)
assert not may_mutate_lease(True, True, False, True)
assert may_mutate_lease(True, True, True, True)
print('formal_wave5_model: ok')
