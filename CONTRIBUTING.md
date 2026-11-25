# Contributing

Preserve the reference app and its settings. Implement changes in this repository
and test them with a separate profile.

Run formatting, Clippy and the Rust tests before submitting changes. Changes to
terminal input, pane focus or lifecycle also need native desktop validation.
Report executed checks and their limitations; do not claim feature parity or
performance improvements from language choice alone.

Keep terminal semantics in Alacritty, PTY ownership in `terminal.rs`, document
saving and recovery in `files.rs`, and workspace serialization in `model.rs`.

## Checks before you push

`make ci` runs `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
markdownlint, `cargo test --locked` and `cargo build --release --locked`. Run it before pushing.
Optional: `pre-commit install` runs fast checks on each commit (`.pre-commit-config.yaml`).

## Branches, commits and pull requests

- Branch prefixes: `feat/`, `fix/`, `docs/`, `chore/`, `ci/`, `test/`.
- PR titles follow Conventional Commits (`feat: ...`, `fix: ...`, `chore: ...`). One purpose per PR.
- Commit messages say what changed and why, in the imperative mood.
- Update `CHANGELOG.md` under Unreleased when behavior changes.
- Architecture decisions are recorded in [docs/adr](docs/adr/README.md).
