use std::fmt::{Display, Formatter, Write};

use serde::{Deserialize, Serialize};
use teloxide::payloads;
use teloxide::prelude::*;
use teloxide::requests::{JsonRequest, MultipartRequest};
use teloxide::types::{
    ChatId, ChatKind, ChatPublic, InputFile, MessageId, PublicChatKind,
    PublicChatSupergroup, ThreadId, User,
};
use teloxide::utils::html;

/// The ID of the "general" thread in Telegram.
pub const GENERAL_THREAD_ID: ThreadId = ThreadId(MessageId(1));

/// An extension trait for [`Bot`].
pub trait BotExt {
    /// Similar to [`Bot::send_message`], but replies to the given message.
    fn reply_message<T: Into<String>>(
        &self,
        msg: &Message,
        text: T,
    ) -> JsonRequest<payloads::SendMessage>;

    /// Similar to [`Bot::send_poll`], but replies to the given message.
    #[allow(dead_code)]
    fn reply_poll<Q: Into<String>, O: IntoIterator<Item = String>>(
        &self,
        msg: &Message,
        question: Q,
        options: O,
    ) -> JsonRequest<payloads::SendPoll>;

    /// Similar to [`Bot::send_photo`], but replies to the given message.
    fn reply_photo(
        &self,
        msg: &Message,
        photo: InputFile,
    ) -> MultipartRequest<payloads::SendPhoto>;
}

impl BotExt for Bot {
    fn reply_message<T: Into<String>>(
        &self,
        msg: &Message,
        text: T,
    ) -> JsonRequest<payloads::SendMessage> {
        let mut reply =
            self.send_message(msg.chat.id, text).reply_to_message_id(msg.id);
        reply.message_thread_id = msg.thread_id;
        reply
    }

    fn reply_poll<Q: Into<String>, O: IntoIterator<Item = String>>(
        &self,
        msg: &Message,
        question: Q,
        options: O,
    ) -> JsonRequest<payloads::SendPoll> {
        let mut reply = self
            .send_poll(msg.chat.id, question, options)
            .reply_to_message_id(msg.id);
        reply.message_thread_id = msg.thread_id;
        reply
    }

    fn reply_photo(
        &self,
        msg: &Message,
        photo: InputFile,
    ) -> MultipartRequest<payloads::SendPhoto> {
        let mut reply =
            self.send_photo(msg.chat.id, photo).reply_to_message_id(msg.id);
        reply.message_thread_id = msg.thread_id;
        reply
    }
}

/// An extension trait for [`Message`].
pub trait MessageExt {
    /// Similar to [`Message::thread_id`], but returns [`GENERAL_THREAD_ID`] for
    /// messages in the "general" thread.
    ///
    /// NOTE: In Telegram Bot API, such messages don't have fields `thread_id`
    /// and `is_topic_message`, but we could distinguish them by looking at
    /// `chat.is_forum` field.
    fn thread_id_ext(&self) -> Option<ThreadId>;
}

impl MessageExt for Message {
    fn thread_id_ext(&self) -> Option<ThreadId> {
        let is_forum = matches!(
            &self.chat.kind,
            ChatKind::Public(ChatPublic {
                kind: PublicChatKind::Supergroup(PublicChatSupergroup {
                    is_forum: true,
                    ..
                }),
                ..
            })
        );
        self.thread_id.or_else(|| is_forum.then_some(GENERAL_THREAD_ID))
    }
}

/// An extension trait for [`ChatId`].
pub trait ChatIdExt {
    /// Returns the ID of the channel or supergroup, if this chat is either of
    /// them.  This ID could be used in `t.me/c/...` links.
    fn channel_t_me_id(&self) -> Option<i64>;
}

impl ChatIdExt for ChatId {
    fn channel_t_me_id(&self) -> Option<i64> {
        // https://github.com/teloxide/teloxide/blob/v0.12.2/crates/teloxide-core/src/types/chat_id.rs#L76-L96
        const MIN_MARKED_CHANNEL_ID: i64 = -1_997_852_516_352;
        const MAX_MARKED_CHANNEL_ID: i64 = -1_000_000_000_000;
        (self.0 >= MIN_MARKED_CHANNEL_ID && self.0 <= MAX_MARKED_CHANNEL_ID)
            .then_some(MAX_MARKED_CHANNEL_ID - self.0)
    }
}

pub struct UserHtmlLink<'a>(&'a User);

/// An extension trait for [`teloxide::types::User`].
pub trait UserExt {
    fn html_link(&self) -> UserHtmlLink<'_>;
}

impl UserExt for User {
    fn html_link(&self) -> UserHtmlLink<'_> {
        UserHtmlLink(self)
    }
}

impl Display for UserHtmlLink<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "<a href=\"tg://user?id={}\">{}",
            self.0.id,
            html::escape(&self.0.first_name)
        )?;
        if let Some(last_name) = &self.0.last_name {
            write!(f, " {}", html::escape(last_name))?;
        }
        write!(f, "</a>")
    }
}

/// A pair of chat and thread IDs. Uniquely identifies Telegram thread.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Copy, Clone)]
pub struct ThreadIdPair {
    pub chat: ChatId,
    pub thread: ThreadId,
}

impl ThreadIdPair {
    /// Checks if the given message belongs to this thread.
    pub fn has_message(&self, msg: &Message) -> bool {
        self.chat == msg.chat.id && Some(self.thread) == msg.thread_id
    }
}

pub fn write_message_link(
    out: &mut String,
    chat_id: impl Into<ChatId>,
    message_id: impl Into<MessageId>,
) {
    write!(
        out,
        "<a href=\"https://t.me/c/{}/{}\">",
        -chat_id.into().0 - 1_000_000_000_000,
        message_id.into(),
    )
    .unwrap();
}
