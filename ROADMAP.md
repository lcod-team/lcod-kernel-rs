# Roadmap — Rust Kernel (lcod-kernel-rs)

## M0 — Core runtime
- [x] Charger et valider `lcp.toml` (TOML strict + JSON Schema).
- [x] Enregistrer contrats, implémentations et flows dans la registry embarquée.
- [x] CLI minimale (`cargo run --bin run_compose`) capable d’exécuter un compose avec bindings hôte.

## M1 — Composition & tests
- [x] Opérateurs de flow (`flow/if@1`, `flow/foreach@1`, `flow/break@1`, `flow/continue@1`, `flow/throw@1`).
- [x] Support des slots imbriqués (`ctx.run_slot`, `ctx.replace_run_slot_handler`) et nettoyage de scope.
- [x] Couverture `cargo test` + miroirs des fixtures spec (`tests/flow_blocks.rs`, `cargo run --bin test_specs`).
- [ ] Compléter `flow/parallel@1` et `flow/try@1` (propagation structurée des erreurs).

## M2 — Tooling & CI
- [ ] Publier un workflow CI rustfmt/clippy.
- [x] Tests de parité `tooling/script@1` (QuickJS sandbox, timeouts, `run_slot`).

## M3 — Runtime parity

Goal: atteindre la parité fonctionnelle avec la référence Node.

Delivered:
- [x] Infrastructure contracts (`core/fs`, `core/http`, `core/git`, `core/hash`, `core/parse`, `core/stream`) via `register_core`.
- [x] Resolver CLI (`cargo run --bin run_compose -- --resolver`) + helpers workspace (canonicalisation des IDs).
- [x] Tooling partagé (`tooling/test_checker@1`, `tooling/script@1`) et conformance diff (piloté par `node scripts/run-conformance.mjs`).

Next:
- [ ] M3-04b Finaliser les bindings avancés (git/http, packaging des manifests) et documenter `docs/runtime-rust.md`.
- [ ] M3-06 Registry scope chaining: implémenter le support natif de `tooling/registry/scope@1` (push/pop registry) avec tests intégrés + conformance.

## M4 — Observabilité & logging
- [ ] Intégrer `lcod://tooling/log@1` une fois la spec fixée (sérialisation structured log + passerelles host).
- [ ] Exposer un mode trace (`--trace`) sur `run_compose` pour inspecter les mutations de scope/slots.

## M5 — Packaging & distribution
- [ ] Publier un crate/binaire `lcod-kernel-rs-cli`.
- [ ] Implémenter `--assemble/--ship/--build` (aligné sur la spec packaging).
- [x] Conserver la normalisation `tooling/compose/normalize@1` alignée sur la spec.

## M6 — Service demo
- [x] HTTP demo (`env/http_host@0.1.0`, `project/http_app@0.1.0`) : parité avec Node + tests.
