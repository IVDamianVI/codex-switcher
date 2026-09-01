# Contributing

Contributions are welcome through issues and pull requests.

## Development setup

Install a Rust toolchain compatible with the version declared in `Cargo.toml`
and make sure Codex CLI is available when testing account integration manually.

Before submitting a pull request, run:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Add tests for behavior changes. Keep changes focused and update the README when
the command-line interface or storage behavior changes.

## Sensitive data

Never commit real authentication files, tokens, API keys, e-mail addresses,
account identifiers, local absolute paths, or captured app-server responses from
a real account. Use synthetic fixtures in tests and documentation.

Report vulnerabilities according to [SECURITY.md](SECURITY.md), not through a
public issue.
