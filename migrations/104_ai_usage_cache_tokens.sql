ALTER TABLE ai_usage_records
    ADD COLUMN remote_cache_read_tokens INTEGER CHECK (remote_cache_read_tokens >= 0);

ALTER TABLE ai_usage_records
    ADD COLUMN remote_cache_write_tokens INTEGER CHECK (remote_cache_write_tokens >= 0);
