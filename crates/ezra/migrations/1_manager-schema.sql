CREATE TABLE inbound_events (
    source TEXT NOT NULL,
    subject TEXT NOT NULL,
    event_id TEXT NOT NULL,
    actor TEXT NOT NULL,
    created_at TEXT NOT NULL,
    received_at INTEGER NOT NULL DEFAULT (unixepoch()),
    message TEXT NOT NULL,
    delivery_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (delivery_state IN ('pending', 'delivering', 'delivered', 'uncertain', 'failed', 'expired')),
    created_at_seconds INTEGER NOT NULL DEFAULT 0,
    requested_agent TEXT,
    requested_model TEXT,
    requested_effort TEXT,
    chat_name TEXT,
    source_url TEXT,
    delivered_chat_name TEXT,
    new_chat INTEGER NOT NULL DEFAULT 0 CHECK (new_chat IN (0, 1)),
    new_chat_applied INTEGER NOT NULL DEFAULT 0 CHECK (new_chat_applied IN (0, 1)),
    initial_context TEXT,
    attempted_at INTEGER,
    PRIMARY KEY (source, subject, event_id)
) STRICT;

CREATE INDEX inbound_events_pending ON inbound_events (received_at)
    WHERE delivery_state = 'pending';
CREATE INDEX inbound_events_unresolved ON inbound_events (source, subject)
    WHERE delivery_state IN ('delivering', 'uncertain');
CREATE INDEX inbound_events_queued_bytes
    ON inbound_events (length(CAST(message AS BLOB)) + COALESCE(length(CAST(initial_context AS BLOB)), 0))
    WHERE delivery_state IN ('pending', 'delivering');
CREATE INDEX inbound_events_delivered_age
    ON inbound_events (received_at, length(CAST(message AS BLOB)) + COALESCE(length(CAST(initial_context AS BLOB)), 0))
    WHERE delivery_state IN ('delivered', 'uncertain', 'failed', 'expired');
CREATE INDEX inbound_events_queued_conversation ON inbound_events (source, subject)
    WHERE delivery_state IN ('pending', 'delivering');
CREATE INDEX inbound_events_unapplied_reset ON inbound_events (source, subject)
    WHERE new_chat = 1 AND new_chat_applied = 0;
CREATE INDEX inbound_events_waiting ON inbound_events (received_at)
    WHERE delivery_state IN ('pending', 'delivering') AND attempted_at IS NULL;

CREATE TABLE inbound_sessions (
    id INTEGER PRIMARY KEY,
    host_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    workspace TEXT NOT NULL,
    last_used_at INTEGER NOT NULL DEFAULT (unixepoch()),
    initial_context_pending INTEGER NOT NULL DEFAULT 0 CHECK (initial_context_pending IN (0, 1)),
    UNIQUE (host_id, agent, chat_id)
) STRICT;

CREATE TABLE inbound_conversations (
    source TEXT NOT NULL,
    subject TEXT NOT NULL,
    session_id INTEGER NOT NULL REFERENCES inbound_sessions(id) ON DELETE CASCADE,
    PRIMARY KEY (source, subject)
) STRICT;

CREATE INDEX inbound_conversations_session ON inbound_conversations (session_id);

CREATE INDEX inbound_sessions_last_used ON inbound_sessions (last_used_at);

CREATE TABLE manager_identity (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    host_id TEXT NOT NULL CHECK (length(host_id) = 32)
) STRICT;

INSERT INTO manager_identity (singleton, host_id) VALUES (1, lower(hex(randomblob(16))));

CREATE TABLE inbound_source_checkpoints (
    source TEXT NOT NULL CHECK (length(source) BETWEEN 1 AND 512),
    subject TEXT NOT NULL CHECK (length(subject) BETWEEN 1 AND 512),
    scanned_through INTEGER NOT NULL,
    activated_at INTEGER NOT NULL DEFAULT 0,
    active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (source, subject)
) STRICT;

CREATE INDEX inbound_source_checkpoints_updated_at
    ON inbound_source_checkpoints (active, updated_at);

CREATE TABLE inbound_feedback_attempts (
    source TEXT NOT NULL CHECK (length(source) BETWEEN 1 AND 512),
    subject TEXT NOT NULL CHECK (length(subject) BETWEEN 1 AND 512),
    event_id TEXT NOT NULL CHECK (length(event_id) BETWEEN 1 AND 512),
    created_at_seconds INTEGER NOT NULL,
    PRIMARY KEY (source, subject, event_id)
) STRICT;

CREATE INDEX inbound_feedback_attempts_age ON inbound_feedback_attempts (created_at_seconds);

CREATE TABLE inbound_feedback_retention (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    created_at_floor INTEGER NOT NULL
) STRICT;

INSERT INTO inbound_feedback_retention VALUES (1, 0);
