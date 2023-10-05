use teloxide::payloads;
use teloxide::prelude::*;
use teloxide::requests::JsonRequest;

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
}
