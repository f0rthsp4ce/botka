INSERT INTO tg_chat_topics (
    chat_id,
    topic_id,
    closed,
    name,
    icon_color,
    icon_emoji,
    id_closed,
    id_name,
    id_icon_emoji
)
VALUES (
    :chat_id,
    :topic_id,
    :closed,
    :name,
    :icon_color,
    :icon_emoji,
    CASE WHEN :closed     IS NULL THEN 0 ELSE :id END,
    CASE WHEN :name       IS NULL THEN 0 ELSE :id END,
    CASE WHEN :icon_emoji IS NULL THEN 0 ELSE :id END
)
ON CONFLICT (chat_id, topic_id)
DO UPDATE SET
    closed        = CASE WHEN id_closed     > :id OR :closed     IS NULL THEN closed        ELSE :closed     END,
    id_closed     = CASE WHEN id_closed     > :id OR :closed     IS NULL THEN id_closed     ELSE :id         END,

    name          = CASE WHEN id_name       > :id OR :name       IS NULL THEN name          ELSE :name       END,
    id_name       = CASE WHEN id_name       > :id OR :name       IS NULL THEN id_name       ELSE :id         END,

    icon_color    = COALESCE(:icon_color, icon_color),

    icon_emoji    = CASE WHEN id_icon_emoji > :id OR :icon_emoji IS NULL THEN icon_emoji    ELSE :icon_emoji END,
    id_icon_emoji = CASE WHEN id_icon_emoji > :id OR :icon_emoji IS NULL THEN id_icon_emoji ELSE :id         END
