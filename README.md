# git-auto-committer

Requires Rust 1.99 or newer and Git on PATH.

```sh
cargo build --locked --release
./target/release/git-auto-committer --repo-path /path/to/repo --total 2500 --push-every 1000
```

Creates commits in batches (default 1000), pushing the current branch to `origin`
after every 1000 new commits and pushing any remainder when finished. `--total 0`
(the default) runs until Ctrl+C. Ctrl+C stops creation and pushes pending commits.
Git must have a local author identity and working remote authentication configured.

Options:

- `--push-every N`: positive number of commits between pushes (default 1000).
- `--push-remote NAME`: remote to receive the current branch (default `origin`).
- `--push-retries N`: additional attempts for recognized transient network failures (default 3).
- `--push-retry-delay SECONDS`: fixed delay between retries (default 2).
- `--batch-size N`: commits per file snapshot (default 1000).
- `--batch-delay SECONDS`: pause between batches (default 1).

Files are staged into a tree once per batch; changes made during a batch are picked
up at the next batch. Each commit in a batch reuses its tree and author signature.
Push uses native Git with its existing credential helpers, SSH and proxy settings.
It never force-pushes. Permanent failures stop immediately; exhausted retries exit
with an error, preserving local commits for manual recovery (`git push origin HEAD`).
The push counter counts commits created by the current process.

Validation: `cargo test --locked` and `cargo clippy --locked --all-targets -- -D warnings`.
Integration tests use temporary local repositories; they do not push to GitHub.
