// @generated automatically by Diesel CLI.

diesel::table! {
    borrowed_items (chat_id, user_message_id) {
        chat_id -> BigInt,
        thread_id -> Integer,
        user_message_id -> Integer,
        bot_message_id -> Integer,
        user_id -> BigInt,
        items -> Text,
        created_at -> Timestamp,
    }
}

diesel::table! {
    borrowed_items_reminders (chat_id, user_message_id, item_name) {
        chat_id -> BigInt,
        user_message_id -> Integer,
        user_id -> BigInt,
        item_name -> Text,
        reminders_sent -> Integer,
        last_reminder_sent -> Nullable<Timestamp>,
        created_at -> Timestamp,
    }
}

diesel::table! {
    chat_history (rowid) {
        rowid -> Integer,
        chat_id -> BigInt,
        thread_id -> Integer,
        message_id -> Integer,
        from_user_id -> Nullable<BigInt>,
        timestamp -> Timestamp,
        message_text -> Text,
        classification_result -> Nullable<Text>,
        used_model -> Nullable<Text>,
    }
}

diesel::table! {
    dashboard_messages (chat_id, thread_id, message_id) {
        chat_id -> BigInt,
        thread_id -> Integer,
        message_id -> Integer,
        text -> Text,
    }
}

diesel::table! {
    memories (rowid) {
        rowid -> Integer,
        memory_text -> Text,
        creation_date -> Timestamp,
        expiration_date -> Nullable<Timestamp>,
        chat_id -> Nullable<BigInt>,
        thread_id -> Nullable<Integer>,
        user_id -> Nullable<BigInt>,
    }
}

diesel::table! {
    needed_items (rowid) {
        rowid -> Integer,
        request_chat_id -> BigInt,
        request_message_id -> Integer,
        request_user_id -> BigInt,
        pinned_chat_id -> BigInt,
        pinned_message_id -> Integer,
        buyer_user_id -> Nullable<BigInt>,
        item -> Text,
    }
}

diesel::table! {
    options (name) {
        name -> Text,
        value -> Text,
    }
}

diesel::table! {
    residents (rowid) {
        rowid -> Integer,
        tg_id -> BigInt,
        begin_date -> Timestamp,
        end_date -> Nullable<Timestamp>,
    }
}

diesel::table! {
    temp_open_tokens (id) {
        id -> Integer,
        token -> Text,
        resident_tg_id -> BigInt,
        guest_tg_id -> Nullable<BigInt>,
        created_at -> Timestamp,
        expires_at -> Timestamp,
        used_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    tg_chat_topics (chat_id, topic_id) {
        chat_id -> BigInt,
        topic_id -> Integer,
        closed -> Nullable<Bool>,
        name -> Nullable<Text>,
        icon_color -> Nullable<Integer>,
        icon_emoji -> Nullable<Text>,
        id_closed -> Integer,
        id_name -> Integer,
        id_icon_emoji -> Integer,
    }
}

diesel::table! {
    tg_chats (id) {
        id -> BigInt,
        kind -> Text,
        username -> Nullable<Text>,
        title -> Nullable<Text>,
    }
}

diesel::table! {
    tg_users (id) {
        id -> BigInt,
        username -> Nullable<Text>,
        first_name -> Text,
        last_name -> Nullable<Text>,
    }
}

diesel::table! {
    tg_users_in_chats (chat_id, user_id) {
        chat_id -> BigInt,
        user_id -> BigInt,
        chat_member -> Nullable<Text>,
        seen -> Bool,
    }
}

diesel::table! {
    tracked_polls (tg_poll_id) {
        tg_poll_id -> Text,
        creator_id -> BigInt,
        info_chat_id -> BigInt,
        info_message_id -> Integer,
        voted_users -> Text,
    }
}

diesel::table! {
    user_macs (tg_id, mac) {
        tg_id -> BigInt,
        mac -> Text,
    }
}

diesel::table! {
    user_ssh_keys (tg_id, key) {
        tg_id -> BigInt,
        key -> Text,
    }
}

diesel::allow_tables_to_appear_in_same_query!(
    borrowed_items,
    borrowed_items_reminders,
    chat_history,
    dashboard_messages,
    memories,
    needed_items,
    options,
    residents,
    temp_open_tokens,
    tg_chat_topics,
    tg_chats,
    tg_users,
    tg_users_in_chats,
    tracked_polls,
    user_macs,
    user_ssh_keys,
);
