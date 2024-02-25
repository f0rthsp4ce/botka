CREATE TABLE tg_users (
  id BIGINT PRIMARY KEY NOT NULL,
  username TEXT,
  first_name TEXT NOT NULL,
  last_name TEXT
);

CREATE TABLE tg_chats (
  id BIGINT PRIMARY KEY NOT NULL,
  kind TEXT NOT NULL,
  username TEXT,
  title TEXT
);

CREATE TABLE tg_users_in_chats (
  chat_id BIGINT NOT NULL /* REFERENCES tg_chats(id) */,
  user_id BIGINT NOT NULL /* REFERENCES tg_users(id) */,
  chat_member TEXT, -- JSON
  seen BOOLEAN NOT NULL,
  PRIMARY KEY (chat_id, user_id)
);

CREATE TABLE tg_chat_topics (
  chat_id BIGINT NOT NULL /* REFERENCES tg_chats(id) */,
  topic_id INTEGER NOT NULL,
  -- Following fields might be NULL if bot missed the update
  closed BOOLEAN,
  name TEXT,
  icon_color INTEGER,
  icon_emoji TEXT,
  -- Last seen message id for each field
  id_closed INTEGER NOT NULL,
  id_name INTEGER NOT NULL,
  id_icon_emoji INTEGER NOT NULL,
  PRIMARY KEY (chat_id, topic_id)
);

CREATE TABLE residents (
  rowid INTEGER PRIMARY KEY NOT NULL, -- Needed for diesel
  tg_id BIGINT NOT NULL /* REFERENCES tg_users(id) */,
  begin_date DATETIME NOT NULL,
  end_date DATETIME -- NULL means "current resident"
);

CREATE TABLE user_macs (
  tg_id BIGINT NOT NULL /* REFERENCES tg_users(id) */,
  mac TEXT NOT NULL,
  PRIMARY KEY (tg_id, mac)
);

CREATE TABLE options (
  name TEXT PRIMARY KEY NOT NULL,
  value TEXT NOT NULL
);

CREATE TABLE tracked_polls (
  tg_poll_id TEXT NOT NULL,
  creator_id BIGINT NOT NULL,
  info_chat_id BIGINT NOT NULL, -- TODO: rename to chat_id
  -- TODO: add poll_message_id INTEGER NOT NULL,
  info_message_id INTEGER NOT NULL,
  voted_users TEXT NOT NULL,
  PRIMARY KEY (tg_poll_id)
);

CREATE TABLE borrowed_items (
  chat_id BIGINT NOT NULL,
  thread_id INTEGER NOT NULL,
  user_message_id INTEGER NOT NULL,
  bot_message_id INTEGER NOT NULL,
  user_id BIGINT NOT NULL,
  items TEXT NOT NULL,
  PRIMARY KEY (chat_id, user_message_id)
);

CREATE TABLE needed_items (
  rowid INTEGER PRIMARY KEY NOT NULL,

  request_chat_id BIGINT NOT NULL,
  request_message_id INTEGER NOT NULL,
  request_user_id BIGINT NOT NULL,

  pinned_chat_id BIGINT NOT NULL,
  pinned_message_id INTEGER NOT NULL,

  buyer_user_id BIGINT,

  item TEXT NOT NULL
);

CREATE TABLE dashboard_messages (
  chat_id BIGINT NOT NULL,
  thread_id INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  text TEXT NOT NULL,
  PRIMARY KEY (chat_id, thread_id, message_id)
);
