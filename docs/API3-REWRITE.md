# API-3 rewrite notes

Hoard v0.8.0-dev.1 `api3-r8` is a clean API-3 application/workspace boundary. The mature RC remains the behavior/UI specification.

Shared ownership moves to Stable Build Framework v0.5.1:
- application identity/path primitives through `stable-app-core`;
- common palette, opacity, metrics and glyph language through `stable-ui`;
- generic Cargo compilation and candidate lifecycle through framework API 3;
- independent validation through adaptive-validator 1.0.0 / validator API 1.

Hoard retains file-manager-specific layout, operations, native Wayland input/rendering, clipboard/DnD, updater, deployment/default-handler safety and runtime regression proofs.

The committed Cargo.lock was generated as the explicit initial dependency-update event for this rewrite and pins the shared cores to framework tag v0.5.1 / commit `da2d26861512905640a5097325f65f2c75e08dd1`. Routine host-gate builds are locked and do not update dependencies.
