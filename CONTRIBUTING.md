# Contributing to omaCortex

Thanks for helping. omaCortex is a local activity recorder: issues and pull
requests are welcome, and nothing in a patch should send activity off the
machine.

## Before you start

- Search the open issues, including closed ones. A bug or idea may already
  be filed.
- Use the bug report or feature request template. A failing `omacortex doctor`
  line is more useful than a paraphrase; you can redact home-directory paths.
- Do not include secrets, tokens, or private window titles in an issue, a
  commit, or a pull request.

## Setup

You need Rust (the version in `rust-version` in `Cargo.toml`, or newer) and
Node.js 22 for the panel and extension tests.

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
node --test tests/model.test.js tests/extension.test.mjs
```

Continuous integration runs those commands on every pull request. `cargo fmt`
and clippy are required: warnings are denied.

`install.sh --build` compiles the release binary. `install.sh --download`
fetches the GitHub release that matches `version` in `Cargo.toml` and checks
its sha256. Use `--download` when you want to try the published binary rather
than a local build.

## Pull requests

- Keep a change focused. A documentation fix and a behavior change belong in
  separate requests when they can land independently.
- Describe what changed and how you checked it.
- Do not commit `target/`, `libexec/`, or `dist/`.
- Match the existing style. New Rust stays formatted, and a `match` on an
  enum stays exhaustive.

If you change the version, update all three of these together. CI checks that
they agree:

- `Cargo.toml`
- `manifest.json`
- `extension/manifest.json`

The plugin id `io.github.anonb3ll.omacortex` is the install directory, the
shell IPC name, and the browser native-messaging host name. Renaming it
breaks installed plugins and connected browsers, so leave it as it is unless
a change is explicitly about that rename.

## Releases

Releases are cut by tagging `vX.Y.Z` with the same version as `Cargo.toml`.
That builds the x86_64 and aarch64 archives. Do not push a tag from a pull
request.

## License

By contributing, you agree that your changes are licensed under the MIT
license in `LICENSE`.
