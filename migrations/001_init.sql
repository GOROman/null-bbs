-- 時刻はすべて UNIX 秒

CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    login         TEXT NOT NULL UNIQUE COLLATE NOCASE,
    handle        TEXT NOT NULL,
    pw_hash       TEXT NOT NULL,
    level         INTEGER NOT NULL DEFAULT 10,   -- 0 ゲスト / 10 会員 / 50 サブ SYSOP / 100 SYSOP
    banned        INTEGER NOT NULL DEFAULT 0,
    screen_rows   INTEGER NOT NULL DEFAULT 24,
    profile       TEXT NOT NULL DEFAULT '',
    created_at    INTEGER NOT NULL,
    last_login_at INTEGER,
    login_count   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE boards (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    read_level  INTEGER NOT NULL DEFAULT 0,
    write_level INTEGER NOT NULL DEFAULT 10,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    next_seq    INTEGER NOT NULL DEFAULT 1,
    deleted     INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
);

CREATE TABLE posts (
    id            INTEGER PRIMARY KEY,
    board_id      INTEGER NOT NULL REFERENCES boards(id),
    seq           INTEGER NOT NULL,
    parent_seq    INTEGER,
    author_id     INTEGER REFERENCES users(id),
    author_login  TEXT NOT NULL,
    author_handle TEXT NOT NULL,
    title         TEXT NOT NULL,
    body          TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    deleted       INTEGER NOT NULL DEFAULT 0,
    UNIQUE (board_id, seq)
);

CREATE TABLE read_marks (
    user_id       INTEGER NOT NULL REFERENCES users(id),
    board_id      INTEGER NOT NULL REFERENCES boards(id),
    last_read_seq INTEGER NOT NULL,
    PRIMARY KEY (user_id, board_id)
);

CREATE TABLE mail (
    id          INTEGER PRIMARY KEY,
    from_id     INTEGER NOT NULL REFERENCES users(id),
    to_id       INTEGER NOT NULL REFERENCES users(id),
    subject     TEXT NOT NULL,
    body        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    read_at     INTEGER,
    del_by_from INTEGER NOT NULL DEFAULT 0,
    del_by_to   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX mail_to ON mail (to_id, del_by_to);

CREATE TABLE access_log (
    id        INTEGER PRIMARY KEY,
    user_id   INTEGER REFERENCES users(id),
    login     TEXT NOT NULL,
    handle    TEXT NOT NULL,
    line_no   INTEGER NOT NULL,
    line_kind TEXT NOT NULL,
    peer      TEXT NOT NULL,
    login_at  INTEGER NOT NULL,
    logout_at INTEGER,
    reason    TEXT
);

CREATE TABLE chat_log (
    id      INTEGER PRIMARY KEY,
    room    TEXT NOT NULL,
    handle  TEXT NOT NULL,
    line_no INTEGER NOT NULL,
    text    TEXT NOT NULL,
    at      INTEGER NOT NULL
);
