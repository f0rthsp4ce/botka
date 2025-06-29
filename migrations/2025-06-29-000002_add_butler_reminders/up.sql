CREATE TABLE borrowed_items_reminders (
  chat_id BIGINT NOT NULL,
  user_message_id INTEGER NOT NULL,
  user_id BIGINT NOT NULL,
  item_name TEXT NOT NULL,
  reminders_sent INTEGER NOT NULL DEFAULT 0,
  last_reminder_sent DATETIME,
  created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (chat_id, user_message_id, item_name)
);
