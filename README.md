# Hoard API 3 rewrite

This tree is the first development candidate of the Hoard API-3 rewrite. It is **not** an RC.

The mature Hoard v0.8 RC line is the behavioral and UI specification. This source rebuilds Hoard as a project function on Stable Build Framework v0.5.1 / API 3 with `stable-app-core` and `stable-ui`.

## Build / validate / install

```bash
./framework/host-gate
```

The host gate is the only operator entrypoint. Routine builds use the committed `Cargo.lock` and `--locked`; framework/dependency upgrades are explicit development events.

## Architecture

- `crates/hoard-core`: file-manager state, filesystem operations, cache, settings, history, search, previews, updater, diagnostics and identity adapter.
- `crates/hoard-function`: Hoard UI/function behavior and stable-ui adapter.
- `apps/hoard`: direct SCTK/calloop Wayland shell and Vulkan-only egui-wgpu renderer.
- `validator/`: Hoard-specific API-1 validator hooks.
- `framework/`: minimal API-3 project contract and deploy/rollback function hooks.

See `docs/API3-RC-PARITY.md` for the parity gate.
