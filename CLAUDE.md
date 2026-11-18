# Klobuk (`klobuk`)

Rust workspace: a local software factory that dispatches Claude Code workers into git worktrees, tracked by a per-repo SQLite
issue store, with Herdr as the terminal engine. Design decisions D1–D12 and the milestone plan live outside the repo
(`~/.claude/plans/hej-podoba-mi-si-parsed-cerf.md`; project memory `asf-design-decisions`). Do not re-open them here.

## Repo policy
- **No documentation or implementation artifacts are committed**: `docs/`, `context/`, `plans/`, `*.md` (except this file) are gitignored.
  Design docs and plans stay local under `docs/superpowers/`.
- `.factory/factory.db*`, `.factory/runs/`, `.worktrees/` are local runtime state.
- Conventional Commits, English, no AI trailers unless asked. Subject ≤ 72 chars.

## Docs/types first
Before writing any call into rusqlite, clap, ratatui, tokio, globset, ulid, time or the Herdr CLI: read the current docs
(Context7 `resolve-library-id` → `query-docs`) or `herdr <group>` help. Never infer method names, flags or JSON field names
from memory. Herdr facts verified for 0.9.5 / protocol 22 are in project memory `asf-herdr-cli-facts`.

## Crates
- `klobuk-core` — domain types, ports (`IssueStore`, later `AgentRuntime`, `Vcs`), scheduler, lane resolution, config, JSONL records. **No IO.**
- `klobuk-store` — rusqlite adapter (`schema.sql`, WAL, `busy_timeout` 10 s, IMMEDIATE transactions for writes).
- `klobuk` — binary: clap CLI with `--json` everywhere; exit codes 0 ok / 1 error / 2 usage / 3 conflict.

## Gates (run all before claiming green; CI runs the same)
```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Locally run `cargo fmt --all` first, then clippy, then tests.

## Conventions
- Errors: `thiserror` enums in library crates, `anyhow` only in the binary.
- Time: RFC 3339 UTC strings in SQLite (`time::OffsetDateTime`).
- JSON output envelope: `{"ok":true,"result":…}` / `{"ok":false,"error":{"code":"…","message":"…"}}`.
- Files written by `klobuk` are UTF-8 without BOM, LF line endings.
- Tests: unit tests next to code, integration tests in `crates/<crate>/tests/`, CLI tests with `assert_cmd` + `tempfile`.
