CREATE TABLE user_ssh_keys (
  tg_id BIGINT NOT NULL /* REFERENCES tg_users(id) */,
  key TEXT NOT NULL,
  PRIMARY KEY (tg_id, key)
);
