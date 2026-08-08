ALTER TABLE scan_schedules
    ADD COLUMN desired_interval_seconds INTEGER NOT NULL DEFAULT 1
    CHECK (desired_interval_seconds > 0);

UPDATE scan_schedules
SET desired_interval_seconds = interval_seconds;
