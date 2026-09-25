-- ファイルライブラリ

CREATE TABLE files (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE,     -- files_dir 内のファイル名
    size            INTEGER NOT NULL,
    description     TEXT NOT NULL DEFAULT '',
    uploader_id     INTEGER REFERENCES users(id),
    uploader_login  TEXT NOT NULL,
    uploader_handle TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    downloads       INTEGER NOT NULL DEFAULT 0,
    deleted         INTEGER NOT NULL DEFAULT 0
);
