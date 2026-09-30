#!/usr/bin/env python3
from dataclasses import dataclass
from collections import deque

@dataclass(frozen=True)
class State:
    tenant_ctx: bool = False
    authorized: bool = False
    lease_held: bool = False
    current_fence: int = 0
    presented_fence: int = 0
    irreversible_done: bool = False


def next_states(s: State):
    if s.irreversible_done:
        return [s]
    out = []
    if not s.tenant_ctx:
        out.append(State(True, s.authorized, s.lease_held, s.current_fence, s.presented_fence, False))
    if not s.authorized:
        out.append(State(s.tenant_ctx, True, s.lease_held, s.current_fence, s.presented_fence, False))
    if not s.lease_held and s.current_fence < 2:
        nf = s.current_fence + 1
        out.append(State(s.tenant_ctx, s.authorized, True, nf, nf, False))
    if s.lease_held:
        out.append(State(s.tenant_ctx, s.authorized, False, s.current_fence, s.presented_fence, False))
    for token in range(s.current_fence + 1):
        if token != s.presented_fence:
            out.append(State(s.tenant_ctx, s.authorized, s.lease_held, s.current_fence, token, False))
    if s.tenant_ctx and s.authorized and s.lease_held and s.presented_fence == s.current_fence:
        out.append(State(True, True, True, s.current_fence, s.presented_fence, True))
    return out


def check(s: State):
    assert 0 <= s.presented_fence <= s.current_fence
    if s.irreversible_done:
        assert s.tenant_ctx and s.authorized, "irreversible action without tenant authorization"
        assert s.lease_held, "irreversible action without lease ownership"
        assert s.presented_fence == s.current_fence, "irreversible action accepted stale fencing token"


def main():
    start = State(); q = deque([start]); seen = {start}; edges = 0
    while q:
        s = q.popleft(); check(s)
        for n in next_states(s):
            edges += 1; check(n)
            if n not in seen:
                seen.add(n); q.append(n)
    assert any(s.irreversible_done for s in seen)
    assert any(s.current_fence == 2 and s.presented_fence == 1 and not s.irreversible_done for s in seen)
    print(f"fenced MCP action model: {len(seen)} states, {edges} transitions")

if __name__ == "__main__":
    main()
