-- Create table for temporary open tokens
CREATE TABLE temp_open_tokens (
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    token TEXT NOT NULL UNIQUE,
    resident_tg_id BIGINT NOT NULL,
    guest_tg_id BIGINT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TIMESTAMP NOT NULL,
    used_at TIMESTAMP
);

CREATE INDEX idx_temp_open_tokens_token ON temp_open_tokens(token);
CREATE INDEX idx_temp_open_tokens_resident_tg_id ON temp_open_tokens(resident_tg_id);
CREATE INDEX idx_temp_open_tokens_guest_tg_id ON temp_open_tokens(guest_tg_id);
