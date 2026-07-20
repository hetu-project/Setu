# Contributing to Setu

Thanks for your interest in contributing to **Setu** — the causally-driven distributed ledger for AI-agent economies. This guide covers how to set up your environment, the workflow we follow, and what we look for in a pull request. Whether it's a typo fix, a bug report, or a new subsystem, contributions are welcome.

## Code of Conduct

Please keep interactions respectful, constructive, and inclusive. Assume good intent, keep discussion technical, and help newcomers. If you run into behavior that violates this spirit, reach out to the maintainers on [Discord](https://discord.gg/hetu).

## Ways to Contribute

| Action | How |
|--------|-----|
| 🐛 Report a bug | Open an issue using the **Bug report** template |
| 💡 Request a feature | Open an issue using the **Feature request** template |
| 🌱 First contribution | Look for issues labeled [`good-first-issue`](https://github.com/AdvaitaLabs/Setu/labels/good-first-issue) |
| 📖 Improve docs | Docs and code comments PRs are very welcome |
| 🔧 Fix or build something | Submit a pull request (see the workflow below) |
| 💬 Ask questions | Join the [Discord](https://discord.gg/hetu) |

For anything larger than a small fix, please open (or comment on) an issue first so we can align on the approach before you invest significant time.

## Development Environment

### Prerequisites

- **Rust** 1.75+ (2021 edition) — install via [rustup](https://rustup.rs/)
- **RocksDB** — used for persistent object storage
- **Docker** — optional, for containerized multi-validator deployment

### Build & Test

```bash
git clone https://github.com/AdvaitaLabs/Setu.git
cd Setu
cargo build --release
cargo test --all
```

For end-to-end and multi-validator runs, see the helper scripts in [`scripts/`](scripts/) (e.g. `start_nodes.sh`, `test_e2e.sh`, `multi_validator_test.sh`) and **Path 2 · Run Your Own Node** in the [README](README.md).

### Project Layout

Setu is a Cargo workspace. A few key members:

- `types/`, `crates/setu-core/`, `crates/setu-protocol/` — shared types, core library, protocol messages
- `storage/` — RocksDB-based object storage
- `consensus/` — consensus engine
- `crates/setu-enclave/` — TEE enclave abstraction (Mock + Nitro)
- `crates/setu-move-vm/`, `crates/setu-runtime/` — execution engines
- `setu-validator/`, `setu-solver/`, `setu-rpc/`, `setu-cli/` — node binaries and tooling
- `api/` — HTTP API layer

See **Project Structure** in the [README](README.md) for the full map.

## Development Workflow

1. **Pick or open an issue.** For non-trivial work, discuss it in an issue first. New contributors: start with [`good-first-issue`](https://github.com/AdvaitaLabs/Setu/labels/good-first-issue).
2. **Fork** the repository and create a topic branch: `fix/...`, `feat/...`, or `docs/...`.
3. **Keep changes focused** — one logical change per pull request. Avoid unrelated refactors in the same PR.
4. **Before you push**, make sure the following all pass locally:

   ```bash
   cargo fmt --all
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test --all
   ```

5. **Write clear commit messages.** We use [Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`, etc.
6. **Open a pull request** against `main`. Describe *what* changed, *why*, and *how you tested it*, and link the issue it resolves (e.g. `Fixes #123`).

### ⚠️ Consensus-Critical Changes

Setu validators must remain byte-for-byte deterministic across nodes. Serialization is intentionally pinned (`bcs = "=0.1.6"`). **Do not** bump BCS or change on-chain encodings, hashing, or state layout without a coordinated state migration — any encoding change can split consensus. If your change touches serialization, hashing, or persisted state, call it out explicitly in the PR description.

## Pull Request Checklist

- [ ] `cargo build --release` succeeds
- [ ] `cargo test --all` passes
- [ ] `cargo fmt --all` applied (no diff)
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` is clean
- [ ] The change is focused and the linked issue is referenced
- [ ] Docs/comments updated if behavior or public APIs changed
- [ ] Consensus-critical changes (encoding/hashing/state) are flagged, if any

## Reporting Security Issues

Please **do not** open a public issue for security vulnerabilities. Instead, contact the maintainers privately (e.g. via a direct message to the team on [Discord](https://discord.gg/hetu)) so the issue can be assessed and fixed before public disclosure.

## License

By contributing to Setu, you agree that your contributions will be licensed under the same terms as this project. See the [LICENSE](LICENSE) file for details.

## Questions?

Join us on [Discord](https://discord.gg/hetu) or read the technical docs at [docs.hetu.org](https://docs.hetu.org). We read every message.
