"""SQLite schema for the chibipop dictionary."""

SCHEMA_VERSION = 1

DDL = """
CREATE TABLE dict (
    dict_id  INTEGER PRIMARY KEY,
    name     TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE entry (
    entry_id INTEGER PRIMARY KEY,
    dict_id  INTEGER NOT NULL REFERENCES dict(dict_id),
    senses   TEXT NOT NULL
);

CREATE TABLE term (
    surface  TEXT NOT NULL,
    written  TEXT,
    reading  TEXT,
    pos      TEXT NOT NULL DEFAULT '',
    freq     INTEGER,
    entry_id INTEGER NOT NULL REFERENCES entry(entry_id)
);

CREATE TABLE meta (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);
"""

INDEXES = """
CREATE INDEX idx_term_surface ON term(surface);
"""


def create_schema(conn):
    """Create all tables, the surface index, and record the schema version."""
    conn.executescript(DDL)
    conn.executescript(INDEXES)
    conn.execute(
        "INSERT INTO meta (k, v) VALUES ('schema_version', ?)",
        (str(SCHEMA_VERSION),),
    )
    conn.commit()
