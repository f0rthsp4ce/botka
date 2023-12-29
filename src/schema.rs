// @generated automatically by Diesel CLI.

diesel::table! {
    borrowed_items (chat_id, user_message_id) {
        chat_id -> BigInt,
        thread_id -> Integer,
        user_message_id -> Integer,
        bot_message_id -> Integer,
        user_id -> BigInt,
        items -> Text,
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
    forwards (orig_chat_id) {
        orig_chat_id -> BigInt,
        orig_msg_id -> Integer,
        backup_chat_id -> BigInt,
        backup_msg_id -> Integer,
        backup_text -> Text,
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

diesel::joinable!(forwards -> tg_users (orig_chat_id));

diesel::allow_tables_to_appear_in_same_query!(
    borrowed_items,
    dashboard_messages,
    forwards,
    needed_items,
    options,
    residents,
    tg_chat_topics,
    tg_chats,
    tg_users,
    tg_users_in_chats,
    tracked_polls,
    user_macs,
);
