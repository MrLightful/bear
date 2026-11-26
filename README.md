# Bear

Bear is a native macOS app for working with coding agents, built with Rust and GPUI. Keep conversations, agent activity, and Git checkouts together as you take a task from a prompt to reviewed changes.

Bear uses [Huggler](https://github.com/MrLightful/huggler) as its orchestration engine.

- Start tasks in an existing checkout, a new Git worktree, or a scratch directory.
- Follow agent progress, answer questions, and queue follow-ups.
- Review files and diffs, use the built-in terminal, and create GitHub pull requests.
- Configure providers and models to suit your workflow.

## Getting started

Install Git, Xcode, Rust/Cargo 1.94.1 or later, and a signed-in Codex CLI. From the repository root, run:

```sh
cargo run --locked --manifest-path Cargo.toml --target-dir target/desktop
```

In Bear, choose **Create**, select a project or **Start without a project**, and enter your task. Configure additional providers under **Settings → Providers**.

## Documentation

- [Desktop workflow and provider setup](docs/desktop-coding.md)
- [Verification and packaging](docs/verification.md)
- [Product overview](PRODUCT.md)
- [Design guidelines](DESIGN.md)

## License

[MIT](LICENSE)
