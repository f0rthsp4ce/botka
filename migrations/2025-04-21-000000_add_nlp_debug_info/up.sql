-- Add NLP debug info to chat_history table
ALTER TABLE chat_history ADD COLUMN classification_result TEXT DEFAULT NULL;
ALTER TABLE chat_history ADD COLUMN used_model TEXT DEFAULT NULL;
