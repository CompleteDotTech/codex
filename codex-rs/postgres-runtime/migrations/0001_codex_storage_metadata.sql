CREATE TABLE codex_storage.codex_schema_meta (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    format_version INTEGER NOT NULL CHECK (format_version > 0),
    min_reader_version INTEGER NOT NULL CHECK (min_reader_version > 0),
    min_writer_version INTEGER NOT NULL CHECK (min_writer_version > 0)
);

INSERT INTO codex_storage.codex_schema_meta
    (singleton, format_version, min_reader_version, min_writer_version)
VALUES (TRUE, 1, 1, 1);
