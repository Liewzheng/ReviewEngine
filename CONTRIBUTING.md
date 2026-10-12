# Contributing to review-engine

Thank you for your interest in improving review-engine! We welcome contributions from the community, whether they are bug fixes, documentation improvements, new tests, or feature suggestions.

## Project model

ReviewEngine is released under the Apache License 2.0. The core engine in this repository is open source and free for individuals, teams, and organizations to build upon. Enterprise features and commercial support are developed separately and are not part of this open-source codebase.

By keeping the core open, we hope to benefit individual developers, foster external contributions, and maintain transparency in the code review process.

## How to Contribute

1. **Open an issue** first for large changes, new features, or anything that may affect architecture or public APIs.
2. **Fork the repository** and create a feature branch.
3. **Write clear commits** and follow the existing Rust style (`cargo fmt`, `cargo clippy`).
4. **Add tests** when fixing bugs or adding behavior that should remain stable.
5. **Open a Pull Request** and fill out the description so reviewers can understand the change.

## What We Look For

- Clear, focused changes with good commit messages.
- Respectful, constructive communication.
- Minimal, well-explained diffs.

## 静态专家开发约定 / Static expert development conventions

Static experts (any expert that scores the repo from the file tree rather than from an LLM call) **must be language- and ecosystem-aware**. Hardcoding one ecosystem's lockfile or manifest — or worse, claiming a perfect score when no recognised artifact is present — produces false positives on every other ecosystem and erodes the report's credibility.

- **Detect per-ecosystem artifacts.** Recognise the lockfile / manifest shapes the ecosystem actually uses: `Cargo.lock` (Rust, `name = ` lines), `go.mod` (Go, single-line and block `require`), `package-lock.json` (npm / pnpm, v1 `dependencies` and v2/v3 `packages`), `poetry.lock` (Python, `[[package]]`), `Package.resolved` (SwiftPM, `pins`), and the equivalents for any future ecosystem added. Never hardcode a single shape.
- **Say nothing — correctly — when nothing matches.** If no recognised lockfile is present, emit an explicit neutral summary (e.g. `"no dependency lockfile detected — dependency auditing skipped"`) and an ecosystem-aware recommendation (`govulncheck` for Go, `cargo audit` for Rust, `npm audit` for npm, etc.). A silent perfect score is a bug.
- **Mark placeholder scores `fallback = true`** (RENG-115) when the expert cannot produce a real signal — `LLM_FALLBACK_SCORE = 70`, an LLM call that returned empty, a parse failure, etc. The aggregator's `top_risks` and `weighted_total` both filter on this flag, so an honest placeholder never pollutes the report's headline math.
- **Cover every shape with a test.** One unit test per supported ecosystem (so a regression in a single shape surfaces as a single test failure), plus a no-artifact test that pins the neutral message and the `fallback = true` flag.

**Origin example — RENG-118.** The third-party repo-review eval on a Go project (`gitlab.islet.space/isletspace/callme`) surfaced the pattern that produced this rule: the `dependency` expert reported `100/100 healthy` with the false claim `"0 dependencies from Cargo.lock"` for a repo whose only lockfile was `go.mod`, and the `test_coverage` expert reported `40/100 critical` with `MEDIUM — No test framework configured` for a Go project whose test runner is `go test`. Both findings were RUST-context false positives on a non-Rust repo. The same shape repeats for any expert that assumes its single ecosystem is universal — that is what this convention exists to prevent.

## Questions?

If you are unsure about anything, open an issue and we will be happy to help.
