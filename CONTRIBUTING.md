# Contributing

## Submitting a Pull Request

A typical workflow is:

1. Create a topic branch.
2. Add tests for your change.
3. Add the new code.
4. Run `make lint test` to ensure it passes all the checks.
5. Add, commit and push your changes.
6. Submit a pull request.

## Testing

Backend tests are plain Rust `#[test]` / `#[tokio::test]` functions: unit tests live next to the code in
`#[cfg(test)]` modules, integration tests under each crate's `tests/` directory (the web server's run
against a fake NNTP server). Frontend tests run with `bun test`.

Give tests descriptive names that state the behaviour, and make failure messages explain the cause.
Run `make test` for both suites and `make lint` for rustfmt, clippy (`-D warnings`) and svelte-check.


## Updating the Homebrew package

The formula in `Formula/postie.rb` pins the published `postie-rust-<tag>-<target>.tar.gz`
archives and their SHA256 digests: macOS arm64, Linux x86_64 and Linux arm64 (there is
no Intel macOS build, so the formula requires arm64 on macOS). After publishing a stable
release, regenerate it with Python 3 and the GitHub CLI:

```bash
# Use the tag of the release being packaged.
gh api repos/kipsilabs/postie/releases/tags/vX.Y.Z > /tmp/postie-release.json
python3 scripts/update_homebrew.py < /tmp/postie-release.json
python3 -m unittest discover -s scripts -p 'test_update_homebrew.py'
```

The updater requires all three archives and valid GitHub SHA256 asset digests;
it leaves the formula unchanged if validation fails. Review and commit the
updated formula in a pull request. Homebrew users receive it through `brew update`.
The Homebrew workflow runs updater tests and a real install/test on macOS and Linux.
