ALTER TABLE users ADD COLUMN password_initialized INTEGER NOT NULL DEFAULT 1 CHECK (password_initialized IN (0, 1));
