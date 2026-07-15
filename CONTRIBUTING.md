# Contributing to Consilium

Thanks for helping make multi-agent coding easier to use.

## Before you start

- Search existing issues before opening a new one.
- For a bug, include the smallest reproducible example you can share.
- For a large feature or architecture change, open an issue first so we can
  agree on the direction before either of us spends time implementing it.

Never include provider credentials, access tokens, private prompts, or sensitive
repository contents in an issue or test fixture.

## Local setup

The core is a Rust workspace and the interface is a Vite/React application.

```sh
git clone https://github.com/TemurTurayev/consilium.git
cd consilium
cargo test --workspace
cd ui
npm install
npm test
```

Useful checks before submitting a pull request:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd ui && npm run typecheck && npm test
```

Tests that contact real providers or spend quota should never run implicitly.
Keep the default test suite deterministic and local.

## Pull requests

- Keep each pull request focused on one problem.
- Explain the user-visible change and how you verified it.
- Add or update tests when behavior changes.
- Update documentation when a command, configuration field, or limitation
  changes.
- Do not commit generated build directories, credentials, or local transcripts.

By contributing, you agree that your contribution is licensed under the
project's [MIT License](LICENSE).
