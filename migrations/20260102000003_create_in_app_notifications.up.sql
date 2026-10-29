-- In-app notifications: one row per (event, audience member), read by the user
-- through GET {namespace}/in-app.
CREATE TABLE in_app_notifications (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID        NOT NULL,
    subject    TEXT        NOT NULL,
    payload    JSONB       NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX in_app_notifications_user_created_idx
    ON in_app_notifications (user_id, created_at DESC);
