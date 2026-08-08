ALTER TABLE secret_blobs
    ADD COLUMN version INTEGER NOT NULL DEFAULT 1
    CHECK (version > 0);
