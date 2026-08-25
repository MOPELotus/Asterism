CREATE TABLE deployment_ai_config (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    config_json TEXT NOT NULL CHECK (json_valid(config_json)),
    updated_at TEXT NOT NULL
) STRICT;
