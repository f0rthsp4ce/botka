use std::sync::Arc;

use diesel::RunQueryDsl;
use teloxide::types::{
    Chat, ChatKind, ForwardedFrom, Message, MessageEntity, MessageEntityKind,
    PublicChatKind, Update, UpdateKind, User,
};

use crate::common::BotEnv;
use crate::{models, schema};

/// Extract all users' info from a message.
struct ScrapedInfo<'a> {
    pub users: Vec<models::NewTgUser<'a>>,
    pub chats: Vec<models::NewTgChat<'a>>,
}

pub fn scrape(env: Arc<BotEnv>, upd: Update) {
    let scrape = ScrapedInfo::scrape(&upd);
    diesel::replace_into(schema::tg_users::table)
        .values(scrape.users)
        .execute(&mut *env.conn())
        .unwrap();
}

impl<'a> ScrapedInfo<'a> {
    pub fn scrape(update: &'a Update) -> Self {
        let mut info = ScrapedInfo { users: Vec::new(), chats: Vec::new() };
        info.scrape_update(update);
        info.users.sort_by_key(|u| u.id);
        info.users.dedup_by_key(|u| u.id);
        info.chats.sort_by_key(|c| c.id);
        info.chats.dedup_by_key(|c| c.id);
        info
    }

    fn scrape_update(&mut self, upd: &'a Update) {
        match &upd.kind {
            UpdateKind::Message(msg)
            | UpdateKind::EditedMessage(msg)
            | UpdateKind::ChannelPost(msg)
            | UpdateKind::EditedChannelPost(msg) => self.scrape_message(msg),
            UpdateKind::InlineQuery(q) => self.scrape_user(&q.from),
            UpdateKind::ChosenInlineResult(r) => self.scrape_user(&r.from),
            UpdateKind::CallbackQuery(q) => {
                self.scrape_user(&q.from);
                q.message.as_ref().map(|m| self.scrape_message(m));
            }
            UpdateKind::ShippingQuery(q) => self.scrape_user(&q.from),
            UpdateKind::PreCheckoutQuery(q) => self.scrape_user(&q.from),
            UpdateKind::Poll(p) => {
                p.explanation_entities
                    .as_ref()
                    .map(|e| self.scrape_entities(e));
            }
            UpdateKind::PollAnswer(a) => self.scrape_user(&a.user),
            UpdateKind::MyChatMember(m) | UpdateKind::ChatMember(m) => {
                self.scrape_user(&m.from);
                self.scrape_chat(&m.chat);
                // TODO: scrape chat member for chat status?
                self.scrape_user(&m.old_chat_member.user);
                self.scrape_user(&m.new_chat_member.user);
            }
            UpdateKind::ChatJoinRequest(r) => {
                self.scrape_user(&r.from);
                self.scrape_chat(&r.chat);
            }
            UpdateKind::Error(_) => {}
        }
    }

    fn scrape_message(&mut self, msg: &'a Message) {
        msg.from().map(|u| self.scrape_user(u));
        match msg.forward_from() {
            Some(ForwardedFrom::User(u)) => self.scrape_user(u),
            Some(ForwardedFrom::Chat(c)) => self.scrape_chat(c),
            _ => (),
        }
        msg.entities().map(|e| self.scrape_entities(e));
        msg.caption_entities().map(|e| self.scrape_entities(e));
        msg.reply_to_message().map(|r| self.scrape_message(r));
    }

    fn scrape_entities(&mut self, entities: &'a [MessageEntity]) {
        for entity in entities {
            if let MessageEntityKind::TextMention { user } = &entity.kind {
                self.scrape_user(user);
            }
        }
    }

    fn scrape_user(&mut self, user: &'a User) {
        self.users.push(user.into());
    }

    fn scrape_chat(&mut self, chat: &'a Chat) {
        if let ChatKind::Public(chat_public) = &chat.kind {
            self.chats.push(models::NewTgChat {
                id: chat.id.into(),
                kind: match chat_public.kind {
                    PublicChatKind::Channel(_) => "channel",
                    PublicChatKind::Group(_) => "group",
                    PublicChatKind::Supergroup(_) => "supergroup",
                },
                username: chat.username(),
                title: chat.title(),
            });
        }
    }
}

impl<'a> From<&'a User> for models::NewTgUser<'a> {
    fn from(user: &'a User) -> Self {
        models::NewTgUser {
            id: user.id.into(),
            username: user.username.as_deref(),
            first_name: &user.first_name,
            last_name: user.last_name.as_deref(),
        }
    }
}
