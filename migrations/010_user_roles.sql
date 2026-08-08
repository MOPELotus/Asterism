CREATE TABLE user_roles (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('master', 'operator', 'user')),
    PRIMARY KEY (user_id, role)
) STRICT;

INSERT OR IGNORE INTO user_roles (user_id, role)
SELECT users.id, json_each.value
FROM users, json_each(users.roles_json)
WHERE json_each.value IN ('master', 'operator', 'user');
