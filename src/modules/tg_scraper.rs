use std::sync::Arc;

use diesel::{ExpressionMethods, RunQueryDsl, SqliteConnection};
use teloxide::types::{
    Chat, ChatKind, ChatMemberUpdated, ForwardedFrom, Message, MessageEntity,
    MessageEntityKind, MessageKind, PublicChatKind, Update, UpdateKind, User,
};

use crate::common::BotEnv;
use crate::utils::Sqlizer;
use crate::{models, schema};

/// Extract all users' info from a message.
struct ScrapedInfo<'a> {
    users: Vec<models::NewTgUser<'a>>,
    chats: Vec<models::NewTgChat<'a>>,
    user_in_chat: Option<models::NewTgUserInChat>,
    topic: Option<models::NewTgChatTopic<'a>>,
}

pub fn scrape(env: Arc<BotEnv>, upd: Update) {
    env.transaction(|conn| {
        scrape_raw(conn, upd)?;
        Ok(())
    })
    .unwrap();
}

pub fn scrape_raw(
    conn: &mut SqliteConnection,
    upd: Update,
) -> Result<(), diesel::result::Error> {
    let scrape = ScrapedInfo::scrape(&upd);
    diesel::replace_into(schema::tg_users::table)
        .values(scrape.users)
        .execute(conn)?;
    diesel::replace_into(schema::tg_chats::table)
        .values(scrape.chats)
        .execute(conn)?;
    if let Some(user_in_chat) = scrape.user_in_chat {
        if user_in_chat.chat_member.is_some() {
            // Update from ChatMemberUpdated
            diesel::replace_into(schema::tg_users_in_chats::table)
                .values(&user_in_chat)
                .execute(conn)?;
        } else {
            // Update from Message seen
            diesel::insert_into(schema::tg_users_in_chats::table)
                .values(&user_in_chat)
                .on_conflict((
                    schema::tg_users_in_chats::chat_id,
                    schema::tg_users_in_chats::user_id,
                ))
                .do_update()
                .set(schema::tg_users_in_chats::seen.eq(true))
                .execute(conn)?;
        }
    }
    if let Some(topic) = scrape.topic {
        diesel::insert_into(schema::tg_chat_topics::table)
            .values(&topic)
            .on_conflict((
                schema::tg_chat_topics::chat_id,
                schema::tg_chat_topics::topic_id,
            ))
            .do_update()
            .set(&topic)
            .execute(conn)?;
    }
    Ok(())
}

#[allow(clippy::option_map_unit_fn)] // allow for brevity
impl<'a> ScrapedInfo<'a> {
    pub fn scrape(update: &'a Update) -> Self {
        let mut info = ScrapedInfo {
            users: Vec::new(),
            chats: Vec::new(),
            user_in_chat: None,
            topic: None,
        };
        info.scrape_update(update);
        info.users.sort_by_key(|u| u.id);
        info.users.dedup_by_key(|u| u.id);
        info.chats.sort_by_key(|c| c.id);
        info.chats.dedup_by_key(|c| c.id);
        info
    }

    fn scrape_update(&mut self, upd: &'a Update) {
        match &upd.kind {
            UpdateKind::Message(msg) | UpdateKind::ChannelPost(msg) => {
                self.scrape_message(msg, true);
            }
            UpdateKind::EditedMessage(msg)
            | UpdateKind::EditedChannelPost(msg) => {
                self.scrape_message(msg, false);
            }
            UpdateKind::InlineQuery(q) => self.scrape_user(&q.from),
            UpdateKind::ChosenInlineResult(r) => self.scrape_user(&r.from),
            UpdateKind::CallbackQuery(q) => {
                self.scrape_user(&q.from);
                q.message.as_ref().map(|m| self.scrape_message(m, false));
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
                self.scrape_chat_member(m);
            }
            UpdateKind::ChatJoinRequest(r) => {
                self.scrape_user(&r.from);
                self.scrape_chat(&r.chat);
            }
            UpdateKind::Error(_) => {}
        }
    }

    fn scrape_message(&mut self, msg: &'a Message, new: bool) {
        if let Some(from) = &msg.from {
            self.scrape_user(from);
            self.user_in_chat = Some(models::NewTgUserInChat {
                chat_id: msg.chat.id.into(),
                user_id: from.id.into(),
                chat_member: None,
                seen: true,
            });
        }
        if let Some(thread_id) = msg.thread_id {
            if new {
                self.topic = Some(models::NewTgChatTopic {
                    chat_id: msg.chat.id.into(),
                    topic_id: thread_id.into(),
                    closed: None,
                    name: None,
                    icon_color: None,
                    icon_custom_emoji_id: None,
                });
            }
        }
        let topic = self.topic.as_mut();
        match &msg.kind {
            MessageKind::Common(k) => {
                match k.forward.as_ref().map(|f| &f.from) {
                    Some(ForwardedFrom::User(u)) => self.scrape_user(u),
                    Some(ForwardedFrom::Chat(c)) => self.scrape_chat(c),
                    _ => (),
                }
                k.reply_to_message
                    .as_deref()
                    .map(|r| self.scrape_message(r, false));
            }
            MessageKind::NewChatMembers(k) => {
                for user in &k.new_chat_members {
                    self.scrape_user(user);
                }
            }
            MessageKind::LeftChatMember(k) => {
                self.scrape_user(&k.left_chat_member);
            }
            MessageKind::Pinned(k) => self.scrape_message(&k.pinned, false),
            MessageKind::ProximityAlertTriggered(k) => {
                self.scrape_user(&k.proximity_alert_triggered.traveler);
                self.scrape_user(&k.proximity_alert_triggered.watcher);
            }
            MessageKind::ForumTopicCreated(k) if new => {
                let topic = topic.expect("ForumTopicCreated without topic");
                topic.name = Some(k.forum_topic_created.name.as_str());
                topic.icon_color =
                    Some(u8x3_to_i32(k.forum_topic_created.icon_color));
                topic.icon_custom_emoji_id =
                    k.forum_topic_created.icon_custom_emoji_id.as_deref();
                topic.closed = Some(false);
            }
            MessageKind::ForumTopicEdited(k) if new => {
                let topic = topic.expect("ForumTopicEdited without topic");
                topic.name = k.forum_topic_edited.name.as_deref();
                topic.icon_custom_emoji_id =
                    k.forum_topic_edited.icon_custom_emoji_id.as_deref();
            }
            MessageKind::ForumTopicClosed(_) if new => {
                let topic = topic.expect("ForumTopicClosed without topic");
                topic.closed = Some(true);
            }
            MessageKind::ForumTopicReopened(_) if new => {
                let topic = topic.expect("ForumTopicReopened without topic");
                topic.closed = Some(false);
            }
            MessageKind::VideoChatParticipantsInvited(k) => {
                k.video_chat_participants_invited
                    .users
                    .iter()
                    .flatten()
                    .for_each(|u| self.scrape_user(u));
            }
            _ => (),
        }
        msg.entities().map(|e| self.scrape_entities(e));
        msg.caption_entities().map(|e| self.scrape_entities(e));
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

    fn scrape_chat_member(&mut self, cmu: &'a ChatMemberUpdated) {
        self.scrape_user(&cmu.from);
        self.scrape_chat(&cmu.chat);
        self.scrape_user(&cmu.old_chat_member.user);
        self.scrape_user(&cmu.new_chat_member.user);
        self.user_in_chat = Some(models::NewTgUserInChat {
            chat_id: cmu.chat.id.into(),
            user_id: cmu.from.id.into(),
            chat_member: Some(
                Sqlizer::new(cmu.new_chat_member.clone())
                    .expect("Sqlizer ChatMember failed"),
            ),
            seen: cmu.new_chat_member.is_present(),
        });
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

const fn u8x3_to_i32(c: [u8; 3]) -> i32 {
    ((c[0] as i32) << 16) | ((c[1] as i32) << 8) | (c[2] as i32)
}
