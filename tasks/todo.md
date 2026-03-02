# Todo

- [x] Confirm Phase 0 deletion safety via code reference audit
- [x] Remove dead `src/strategy/orchestrator.rs`
- [x] Remove dead `EventEdgePlatformAgent` implementation and exports
- [x] Verify build and targeted tests pass after cleanup
- [x] Update review notes with retained non-deletable legacy paths

## Review

- Planned execution target: GitHub issue #35 (Phase 0 dead code cleanup).
- Removed items in this phase:
  - `src/strategy/orchestrator.rs` (not in module graph, no runtime call sites)
  - `src/platform/agents/event_edge_agent.rs` and related exports
- Validation:
  - `cargo build`
  - `cargo test strategy::manager::tests -- --nocapture`
  - `cargo test coordinator::bootstrap::tests -- --nocapture`
- Explicitly retained (out of current safe-delete scope):
  - `OrderPlatform` / `platform.rs` (still used by RL/legacy paths)
  - `DomainAgent` trait family (still used by legacy platform/router and public exports)
  - `NbaComebackAgent` (still used by CLI strategy path)

Residual risks:
- Dead-code cleanup is partial by design; remaining legacy components still increase architectural surface area until later phases.
