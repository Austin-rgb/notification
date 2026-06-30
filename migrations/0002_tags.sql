-- Add migration script here
CREATE TABLE IF NOT EXISTS notification_tags (
    tag TEXT NOT NULL PRIMARY KEY,
    user_id TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_notification_tags_user_id
    ON notification_tags(user_id);
