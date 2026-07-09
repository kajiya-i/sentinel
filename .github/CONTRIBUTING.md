# Contributing to Sentinel

Thank you for your interest in contributing! No contribution is too small — bug reports,
documentation improvements, and typo fixes are all equally welcome.

If you're unsure where to start, feel free to open an issue and ask.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Prerequisites](#prerequisites)
- [Ways to Contribute](#ways-to-contribute)
- [Issue Labels](#issue-labels)
- [Reporting Bugs](#reporting-bugs)
- [Feature Requests](#feature-requests)
- [Pull Requests](#pull-requests)
- [Commit Messages & DCO](#commit-messages--dco)
- [Code Style](#code-style)
- [Testing](#testing)
- [Documentation](#documentation)
- [Project Docs](#project-docs)
- [License](#license)

---

## Code of Conduct

Please read and follow our [Code of Conduct](CODE_OF_CONDUCT.md).

---

## Prerequisites

**Rust toolchain** (latest stable; edition 2024):

```sh
rustup toolchain install stable
rustup component add rustfmt clippy
```

**Chrome / Chromium** — Sentinel drives a headless browser via chromiumoxide. Install a
recent Chrome/Chromium; Sentinel auto-detects it, or pass `--chrome-path`.

**AI provider key** — running against the real API needs `ANTHROPIC_API_KEY` in the
environment. Tests do **not** hit the real API (AI calls are mocked; browser tests run
against local fixtures and skip when no browser is available), so you can build and test
without a key.

---

## Ways to Contribute

- **Bug reports** — a check produces a wrong verdict, a crash, or flaky behavior
- **Documentation** — rustdoc, `docs/` specs/rules, examples
- **Browser layer** — actions, waiting, evidence collection, condition arrangement (interception)
- **AI judgment** — prompt/schema, escalation, error handling
- **Eval / accuracy** — new eval cases, metrics, harness improvements
- **Platform testing** — verifying builds and browser behavior on macOS / Windows

Looking for a starting point? Check issues labeled
[`good first issue`](https://github.com/kajiya-i/sentinel/issues?q=is%3Aopen+label%3A%22good+first+issue%22),
or `S-Ready-For-Implementation`.

---

## Issue Labels

Issues and PRs use a prefix system. Each label belongs to one family:

| Prefix | Meaning | Examples |
|---|---|---|
| `T-` | **Type** of work | `T-Feature`, `T-Bug`, `T-Tracking-Issue`, `T-Chore`, `T-Test`, `T-Docs`, `T-Refactor` |
| `A-` | **Area** / component | `A-Core`, `A-Browser`, `A-AI`, `A-Config`, `A-CLI`, `A-Report`, `A-Eval`, `A-CI`, `A-Infra` |
| `P-` | **Priority** | `P-Critical`, `P-High`, `P-Medium`, `P-Low` |
| `S-` | **Status** in the workflow | `S-Needs-Design`, `S-Ready-For-Implementation`, `S-In-Progress`, `S-Blocked`, `S-Needs-Review` |

- `S-Ready-For-Implementation` means the design is settled — safe to start a PR.
  `S-Needs-Design` means it needs discussion first.
- Work is grouped by milestone (M0–M6 for the MVP); see [`docs/roadmap/README.md`](../docs/roadmap/README.md).

---

## Reporting Bugs

Before filing a bug, search existing issues to avoid duplicates.

A good bug report includes:

1. **Description** — what happened and what you expected
2. **Minimal reproduction** — the check YAML / command that reproduces it
3. **Versions** — `sentinel --version`, `rustc --version`, Chrome/Chromium version, OS/arch
4. **Output** — full error message or panic backtrace (`RUST_BACKTRACE=1`), and the JSON report if relevant

---

## Feature Requests

Open an issue describing the use case, the desired behavior / API shape, and any alternatives
you considered. For changes that touch multiple crates or the public CLI / config surface,
please discuss in an issue before implementing.

---

## Pull Requests

1. **Open an issue first** for any non-trivial change (features, API/config changes, significant refactors).
2. Fork and create a **topic branch** off `main`:
   ```sh
   git checkout -b browser/add-wait-for
   ```
3. Make your changes. Each commit should build and pass tests independently.
4. Run the full check suite (see [Code Style](#code-style) and [Testing](#testing)).
5. Push your branch and open a PR against `main`.
6. Add new commits to address review feedback — do not force-push during review.

**PRs without tests will not be merged.** If a change is hard to test automatically, explain why.

---

## Commit Messages & DCO

- **DCO required**: sign off every commit (`git commit -s`) — adds `Signed-off-by: Name <email>`.
- **Conventional Commits**: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`, optional scope
  (e.g. `feat(browser): add wait_for action`).
- Imperative mood, first line ≤ 72 chars, no trailing period. Reference issues with
  `Closes #N` / `Fixes #N` / `Refs #N` in the footer.

---

## Code Style

Before submitting, run:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
cargo deny check   # license + advisory policy
```

Key rules (see [`docs/rules/`](../docs/rules/rust.md)):

- **No handwritten `unsafe`** — the workspace sets `unsafe_code = "forbid"` ([`unsafe.md`](../docs/rules/unsafe.md)).
- `unwrap()` / `expect()` are disallowed in production code — use `?` / typed errors
  ([`error-handling.md`](../docs/rules/error-handling.md)).
- No panics on the check-execution path; isolate failures as `verdict = error`.
- Never log secrets (API key) or raw screenshots/DOM ([`logging.md`](../docs/rules/logging.md), [`security.md`](../docs/rules/security.md)).

---

## Testing

```sh
cargo test --workspace
```

- **AI is mocked** (e.g. wiremock) — never hit the real API in CI.
- **Browser tests** run against local HTML fixtures with a real headless Chrome, and **skip**
  when no browser is available.
- Non-trivial logic ships with a test; naming: `<feature>_should_<expected_result>`
  (see [`testing.md`](../docs/rules/testing.md)).

---

## Documentation

Public items should have rustdoc comments (one-line summary + a short example when non-obvious).
Design lives in [`docs/specs/`](../docs/specs/overview.md); conventions in [`docs/rules/`](../docs/rules/rust.md).

---

## Project Docs

- **Concept / vision**: [`docs/concepts.md`](../docs/concepts.md)
- **Specs**: [`docs/specs/`](../docs/specs/overview.md) (architecture, core-mechanism, scenarios, ai-judgment, accuracy, …)
- **Rules**: [`docs/rules/`](../docs/rules/rust.md) (rust, design, error-handling, logging, testing, security, prompting, dependencies, database, unsafe)
- **Roadmap**: [`docs/roadmap/`](../docs/roadmap/README.md) (MVP milestones M0–M6)

---

## License

By contributing, you agree that your contributions are licensed under the project license:
**Apache-2.0**. Sign off your commits (DCO) to certify you have the right to submit them.

See [LICENSE](../LICENSE) for details.
