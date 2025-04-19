-- Add memories table
CREATE TABLE memories (
  rowid INTEGER PRIMARY KEY NOT NULL,
  memory_text TEXT NOT NULL,
  creation_date DATETIME NOT NULL,
  expiration_date DATETIME,
  chat_id BIGINT,
  thread_id INTEGER,
  user_id BIGINT
);

-- Index for efficient retrieval of active memories
CREATE INDEX idx_memories_expiration
ON memories(expiration_date);

-- Index for chat-specific memories
CREATE INDEX idx_memories_chat_expiration
ON memories(chat_id, expiration_date);

-- Index for user-specific memories
CREATE INDEX idx_memories_user_expiration
ON memories(user_id, expiration_date);

-- Add chat_history table
CREATE TABLE chat_history (
  rowid INTEGER PRIMARY KEY NOT NULL,
  chat_id BIGINT NOT NULL,
  thread_id INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  from_user_id BIGINT,        -- NULL if from bot
  timestamp DATETIME NOT NULL,
  message_text TEXT NOT NULL,
  UNIQUE(chat_id, thread_id, message_id)
);

-- Index for efficient retrieval and pruning
CREATE INDEX idx_chat_history_chat_thread_timestamp
ON chat_history(chat_id, thread_id, timestamp);
