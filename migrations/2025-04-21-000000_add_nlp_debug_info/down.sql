-- Drop NLP debug info from chat_history table
ALTER TABLE chat_history DROP COLUMN classification_result;
ALTER TABLE chat_history DROP COLUMN used_model;
