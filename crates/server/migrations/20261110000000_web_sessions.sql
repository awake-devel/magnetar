-- Web UI sessions (D133): an HttpOnly cookie stands for the API token in the browser.
-- The cookie holds 32 random bytes; only their sha256 is stored. A session dies at expires_at,
-- on sign-out, or when the API token changes (token_fp no longer matches).
CREATE TABLE web_sessions (
    id_hash    bytea  PRIMARY KEY,          -- sha256 of the 32 random cookie bytes
    csrf       text   NOT NULL,             -- 43-char base64url of 32 random bytes
    token_fp   text   NOT NULL,             -- first 16 hex of sha256(API token) at login
    created_at bigint NOT NULL,
    expires_at bigint NOT NULL,
    last_seen  bigint NOT NULL,
    user_agent text                         -- first 200 bytes, display only
);
CREATE INDEX web_sessions_expires ON web_sessions (expires_at);
