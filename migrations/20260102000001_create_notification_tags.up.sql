-- Maps to `tagging::Tag` (id, tag, user_id).
-- `MyIdResolver::resolve` does `SELECT user_id FROM notification_tags WHERE tag = $1`
-- with fetch_one, so a tag must identify exactly one user: hence UNIQUE (tag).

CREATE EXTENSION IF NOT EXISTS pgcrypto;  -- gen_random_uuid() on PG < 13

CREATE TABLE notification_tags (
    id      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tag     TEXT NOT NULL CHECK (length(btrim(tag)) > 0),
    user_id UUID NOT NULL
);

CREATE UNIQUE INDEX notification_tags_tag_key ON notification_tags (tag);
CREATE INDEX notification_tags_user_id_idx ON notification_tags (user_id);
