CREATE TABLE session_index (
    session_id   TEXT PRIMARY KEY,
    project      TEXT NOT NULL,
    project_path TEXT,
    slug         TEXT,
    first_message TEXT,
    created_at   TEXT,
    updated_at   TEXT,
    file_path    TEXT NOT NULL,
    last_offset  INTEGER NOT NULL DEFAULT 0,
    indexed_at   TEXT NOT NULL
);

CREATE TABLE messages (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   TEXT NOT NULL REFERENCES session_index(session_id),
    event_uuid   TEXT NOT NULL,
    role         TEXT NOT NULL,
    content      TEXT NOT NULL,
    timestamp    TEXT NOT NULL,
    chunk_index  INTEGER NOT NULL DEFAULT 0,
    embedding    BLOB,
    UNIQUE(event_uuid, chunk_index)
);

CREATE VIRTUAL TABLE messages_fts USING fts5(
    content,
    content_rowid='id',
    content='messages',
    tokenize='porter unicode61'
);

CREATE TABLE session_files (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   TEXT NOT NULL REFERENCES session_index(session_id),
    file_path    TEXT NOT NULL,
    message_id   TEXT,
    UNIQUE(session_id, file_path, message_id)
);

CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content)
    VALUES('delete', old.id, old.content);
END;
