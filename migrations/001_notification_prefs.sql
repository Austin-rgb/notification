-- Global channel defaults per userr

CREATE TABLE IF NOT EXISTS defaults (
    subject TEXT PRIMARY KEY,
    channel TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS preferences (
    user TEXT NOT NULL,
    subject TEXT NOT NULL,
    channel TEXT NOT NULL,
    PRIMARY KEY (user, subject)
);
