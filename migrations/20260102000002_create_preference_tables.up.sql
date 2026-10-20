-- One table per channel; maps to EmailPreference / PushPreference / ConsolePreference
-- (id, subject, address, user_id).
-- `user_id` is TEXT on purpose: the entities declare it as String and the
-- service binds the session `sub` as text. (It holds a UUID string.)
-- UNIQUE (user_id, subject) is REQUIRED: `Preferences::confirm` upserts with
-- ON CONFLICT (user_id, subject) DO UPDATE.

CREATE TABLE email_preferences (
    id      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subject TEXT NOT NULL,
    address TEXT NOT NULL,
    user_id TEXT NOT NULL,
    CONSTRAINT email_preferences_user_subject_key UNIQUE (user_id, subject)
);

CREATE TABLE push_preferences (
    id      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subject TEXT NOT NULL,
    address TEXT NOT NULL,
    user_id TEXT NOT NULL,
    CONSTRAINT push_preferences_user_subject_key UNIQUE (user_id, subject)
);

CREATE TABLE console_preferences (
    id      UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subject TEXT NOT NULL,
    address TEXT NOT NULL,
    user_id TEXT NOT NULL,
    CONSTRAINT console_preferences_user_subject_key UNIQUE (user_id, subject)
);
