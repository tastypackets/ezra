CREATE TABLE inbound_conversations_by_agent (
    source TEXT NOT NULL,
    subject TEXT NOT NULL,
    agent TEXT NOT NULL,
    session_id INTEGER NOT NULL REFERENCES inbound_sessions(id) ON DELETE CASCADE,
    PRIMARY KEY (source, subject, agent)
) STRICT;

INSERT INTO inbound_conversations_by_agent (source, subject, agent, session_id)
SELECT conversations.source, conversations.subject, sessions.agent, conversations.session_id
FROM inbound_conversations AS conversations
JOIN inbound_sessions AS sessions ON sessions.id = conversations.session_id;

DROP TABLE inbound_conversations;

ALTER TABLE inbound_conversations_by_agent RENAME TO inbound_conversations;

CREATE INDEX inbound_conversations_session ON inbound_conversations (session_id);

CREATE TRIGGER inbound_conversations_agent_insert
BEFORE INSERT ON inbound_conversations
WHEN NEW.agent IS NOT (SELECT agent FROM inbound_sessions WHERE id = NEW.session_id)
BEGIN
    SELECT RAISE(ABORT, 'conversation agent differs from its session');
END;

CREATE TRIGGER inbound_conversations_agent_update
BEFORE UPDATE OF agent, session_id ON inbound_conversations
WHEN NEW.agent IS NOT (SELECT agent FROM inbound_sessions WHERE id = NEW.session_id)
BEGIN
    SELECT RAISE(ABORT, 'conversation agent differs from its session');
END;

CREATE TRIGGER inbound_sessions_agent_update
BEFORE UPDATE OF agent ON inbound_sessions
WHEN EXISTS (
    SELECT 1 FROM inbound_conversations WHERE session_id = NEW.id AND agent IS NOT NEW.agent
)
BEGIN
    SELECT RAISE(ABORT, 'conversation agent differs from its session');
END;
