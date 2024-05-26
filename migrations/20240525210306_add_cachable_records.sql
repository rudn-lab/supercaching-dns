-- Add migration script here
CREATE TABLE record (
    id INTEGER NOT NULL PRIMARY KEY,
    record_name TEXT NOT NULL,
    record_type TEXT NOT NULL,
    content_json TEXT NOT NULL,
    data_received_at_unix INTEGER NOT NULL,
    last_query_at_unix INTEGER NOT NULL
);

CREATE UNIQUE INDEX record_name_type_idx ON record(record_name, record_type);