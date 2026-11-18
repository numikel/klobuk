-- klobuk store schema v1. Applied inside one transaction by SqliteStore::migrate.

CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE repos (
  id             TEXT PRIMARY KEY,
  root           TEXT NOT NULL UNIQUE,
  id_prefix      TEXT NOT NULL DEFAULT 'asf',
  next_seq       INTEGER NOT NULL DEFAULT 1,
  default_branch TEXT NOT NULL DEFAULT 'main',
  created_at     TEXT NOT NULL
);

CREATE TABLE issues (
  id         TEXT PRIMARY KEY,
  uid        TEXT NOT NULL UNIQUE,
  title      TEXT NOT NULL,
  body       TEXT NOT NULL DEFAULT '',
  type       TEXT NOT NULL CHECK (type IN ('task','review','epic')),
  status     TEXT NOT NULL CHECK (status IN ('backlog','ready','in-progress','blocked','in-review','done','cancelled')),
  priority   INTEGER NOT NULL DEFAULT 2,
  parent_id  TEXT REFERENCES issues(id),
  file_scope TEXT NOT NULL DEFAULT '[]',
  acceptance TEXT NOT NULL DEFAULT '[]',
  estimate   TEXT,
  labels     TEXT NOT NULL DEFAULT '[]',
  source     TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  closed_at  TEXT
);

CREATE TABLE issue_deps (
  issue_id   TEXT NOT NULL REFERENCES issues(id),
  depends_on TEXT NOT NULL REFERENCES issues(id),
  PRIMARY KEY (issue_id, depends_on)
);

CREATE TABLE claims (
  issue_id   TEXT PRIMARY KEY REFERENCES issues(id),
  worker     TEXT NOT NULL,
  run_id     TEXT,
  worktree   TEXT,
  branch     TEXT,
  claimed_at TEXT NOT NULL,
  note       TEXT
);

CREATE TABLE comments (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  issue_id    TEXT NOT NULL REFERENCES issues(id),
  author      TEXT NOT NULL,
  type        TEXT NOT NULL CHECK (type IN ('QUESTION','ANSWER','DECISION','HANDOFF','BLOCKER','STATUS','FYI')),
  to_lane     TEXT NOT NULL DEFAULT 'all',
  body        TEXT NOT NULL,
  reply_to    INTEGER REFERENCES comments(id),
  resolved_at TEXT,
  created_at  TEXT NOT NULL
);
CREATE INDEX comments_issue ON comments (issue_id, id);

CREATE TABLE cursors (
  lane            TEXT NOT NULL,
  issue_id        TEXT NOT NULL,
  last_comment_id INTEGER NOT NULL,
  PRIMARY KEY (lane, issue_id)
);

CREATE TABLE runs (
  id                TEXT PRIMARY KEY,
  issue_id          TEXT NOT NULL REFERENCES issues(id),
  attempt           INTEGER NOT NULL DEFAULT 1,
  runtime           TEXT NOT NULL,
  agent_name        TEXT,
  workspace_id      TEXT,
  pane_id           TEXT,
  claude_session_id TEXT,
  transcript_path   TEXT,
  worktree          TEXT NOT NULL,
  branch            TEXT NOT NULL,
  base_sha          TEXT NOT NULL,
  state             TEXT NOT NULL CHECK (state IN ('starting','working','blocked','finishing','verifying','succeeded','failed','stalled','over_budget','aborted')),
  budget_usd        REAL,
  cost_usd          REAL NOT NULL DEFAULT 0,
  input_tokens      INTEGER DEFAULT 0,
  output_tokens     INTEGER DEFAULT 0,
  turns             INTEGER DEFAULT 0,
  tool_calls        INTEGER DEFAULT 0,
  exit_reason       TEXT,
  report_path       TEXT,
  started_at        TEXT NOT NULL,
  last_seen_at      TEXT,
  finished_at       TEXT,
  herdr_revision    INTEGER
);

CREATE TABLE merge_queue (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  issue_id     TEXT NOT NULL UNIQUE REFERENCES issues(id),
  run_id       TEXT,
  branch       TEXT NOT NULL,
  state        TEXT NOT NULL CHECK (state IN ('queued','rebasing','gating','reviewing','pr_open','merging','merged','failed','blocked','skipped')),
  attempts     INTEGER NOT NULL DEFAULT 0,
  head_sha     TEXT,
  rebased_onto TEXT,
  pr_url       TEXT,
  gate_results TEXT,
  last_error   TEXT,
  enqueued_at  TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);

CREATE TABLE events (
  seq      INTEGER PRIMARY KEY AUTOINCREMENT,
  ts       TEXT NOT NULL,
  kind     TEXT NOT NULL,
  issue_id TEXT,
  run_id   TEXT,
  actor    TEXT,
  payload  TEXT NOT NULL DEFAULT '{}'
);

INSERT INTO meta (key, value) VALUES ('schema_version', '1');
