use std::fmt::{Display, Formatter, Write};

use serde::{Deserialize, Serialize};
use teloxide::payloads;
use teloxide::prelude::*;
use teloxide::requests::{JsonRequest, MultipartRequest};
use teloxide::types::{ChatId, InputFile, MessageId, ThreadId, User};
use teloxide::utils::html;

pub trait BotExt {
    fn reply_message<T: Into<String>>(
        &self,
        msg: &Message,
        text: T,
    ) -> JsonRequest<payloads::SendMessage>;

    fn reply_poll<Q: Into<String>, O: IntoIterator<Item = String>>(
        &self,
        msg: &Message,
        question: Q,
        options: O,
    ) -> JsonRequest<payloads::SendPoll>;

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

pub struct UserHtmlLink<'a>(&'a User);

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

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Copy, Clone)]
pub struct ThreadIdPair {
    pub chat: ChatId,
    pub thread: ThreadId,
}

impl ThreadIdPair {
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
