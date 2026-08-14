ALTER TABLE browser_bridge_exchanges
    ADD COLUMN command_secret_blob_id TEXT
        REFERENCES secret_blobs(id) ON DELETE RESTRICT;

CREATE UNIQUE INDEX idx_browser_bridge_exchanges_command_secret
    ON browser_bridge_exchanges (command_secret_blob_id)
    WHERE command_secret_blob_id IS NOT NULL;
