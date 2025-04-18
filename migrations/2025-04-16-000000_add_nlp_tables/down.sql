-- Drop memories table and its indexes
DROP INDEX IF EXISTS idx_memories_chat_expiration;
DROP INDEX IF EXISTS idx_memories_expiration;
DROP INDEX IF EXISTS idx_memories_user_expiration;
DROP TABLE IF EXISTS memories;

-- Drop chat_history table and its index
DROP INDEX IF EXISTS idx_chat_history_chat_thread_timestamp;
DROP TABLE IF EXISTS chat_history;
