//! Common helpers to be used by various bot modules.

use std::collections::HashMap;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;
use diesel::{
    ExpressionMethods, QueryDsl, QueryResult, RunQueryDsl, SqliteConnection,
};
use itertools::Itertools;
use ldap_rs::LdapClient;
use teloxide::requests::Requester;
use teloxide::types::{Me, Message, StickerKind, User, UserId};
use teloxide::utils::command::BotCommands;
use teloxide::utils::html::escape;
use teloxide::Bot;

use crate::config::Config;
use crate::db::DbUserId;
use crate::utils::{BotExt, GENERAL_THREAD_ID};

/// Wrapper around [`teloxide::dispatching::UpdateHandler`] to be used in this
/// crate.
pub type UpdateHandler = teloxide::dispatching::UpdateHandler<anyhow::Error>;

/// Access rules describing where and who can execute a command.
#[derive(Eq, PartialEq, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct CommandAccessRules {
    /// Require an user to be a bot admin to execute this command
    pub admin: bool,
    /// Require an user to be a resident to execute this command
    pub resident: bool,
    /// Allow users to execute this command in private chat with bot
    pub in_private: bool,
    /// Allow users to execute this command in group chat
    pub in_group: bool,
    /// Allow users to execute this command only in resident chat
    pub in_resident_chat: bool,
}

impl CommandAccessRules {
    pub const fn new() -> Self {
        Self {
            admin: false,
            resident: false,
            in_private: true,
            in_group: true,
            in_resident_chat: false,
        }
    }
}

impl Default for CommandAccessRules {
    fn default() -> Self {
        Self::new()
    }
}

/// An extension to [`BotCommands`] trait that allows to specify command rules
/// for each command.
///
/// [`BotCommands`]: teloxide::utils::command::BotCommands
pub trait BotCommandsExtTrait: BotCommands {
    const COMMAND_RULES: &'static [CommandAccessRules];
    fn command_rules(&self) -> CommandAccessRules;
}

/// Bot environment: global state shared between all handlers.
pub struct BotEnv {
    pub conn: Mutex<SqliteConnection>,
    pub config: Arc<Config>,
    pub config_path: PathBuf,
    pub reqwest_client: reqwest::Client,
    pub openai_client: async_openai::Client<async_openai::config::OpenAIConfig>,
    // For some reason std mutexes not working in teloxide handlers
    pub ldap_client: tokio::sync::Mutex<LdapClient>,
}

impl BotEnv {
    pub fn conn(&self) -> MutexGuard<'_, SqliteConnection> {
        self.conn.lock().unwrap()
    }
    pub fn transaction<T>(
        &self,
        f: impl FnOnce(&mut SqliteConnection) -> QueryResult<T>,
    ) -> QueryResult<T> {
        self.conn().exclusive_transaction(f)
    }

    pub async fn ldap_client(&self) -> tokio::sync::MutexGuard<'_, LdapClient> {
        self.ldap_client.lock().await
    }
}

/// Derive macro for [`BotCommandsExtTrait`] trait. Should be applied with
/// [`macro_rules_attribute::derive`].
macro_rules! BotCommandsExt {
    (
        $( #[ $_attr:meta ] )*
        $pub:vis
        enum $name:ident {
            $(
                $( #[ $($attr:tt)* ] )*
                $item:ident $( ( $($item_args:tt)* ) )?
            ),* $(,)?
        }
    ) => {
        impl $crate::common::BotCommandsExtTrait for $name {
            const COMMAND_RULES: &'static [$crate::common::CommandAccessRules] =
                &[$({
                    #[allow(unused_mut)]
                    let mut meta = $crate::common::CommandAccessRules::new();
                    BotCommandsExt!(
                        impl set_meta;
                        meta;
                        $( #[ $($attr)* ] )*
                    );
                    meta
                }),*]
            ;
            fn command_rules(&self) -> $crate::common::CommandAccessRules {
                match self {$(
                    BotCommandsExt!(
                        impl skip_item_args;
                        $item $( ( $($item_args)* ) )?
                    ) => {
                        #[allow(unused_mut)]
                        let mut meta =
                            $crate::common::CommandAccessRules::default();
                        BotCommandsExt!(
                            impl set_meta;
                            meta;
                            $( #[ $($attr)* ] )*
                        );
                        meta
                    }
                )*}
            }
        }
    };

    // Internal rules, using <https://stackoverflow.com/a/40484901> trick
    // set_meta
    (
        impl set_meta;
        $name:expr;
        #[custom( $( $meta_key:ident = $meta_value:expr ),* $(,)? )]
        $( #[ $( $rest:tt )* ] )*
    ) => {
        $( $name.$meta_key = $meta_value; )*
        BotCommandsExt!(impl set_meta; $name; $( #[ $( $rest )* ] )* );
    };
    (
        impl set_meta;
        $name:expr;
        #[ $attr:meta ]
        $( #[ $( $rest:tt )* ] )*
    ) => {
        BotCommandsExt!(impl set_meta; $name; $( #[ $( $rest )* ] )* );
    };
    (
        impl set_meta;
        $name:expr;
    ) => {};

    // skip_item_args
    (impl skip_item_args; $v:ident ) => { Self::$v };
    (impl skip_item_args; $v:ident($($t:ty),+) ) => { Self::$v(..) };
}

pub(crate) use BotCommandsExt;

pub fn format_users<'a>(
    out: &mut String,
    iter: impl Iterator<
        Item = (
            impl Into<UserId>,
            impl Into<Option<&'a crate::models::TgUser>>,
        ),
    >,
) {
    let mut first = true;
    for (tg_id, user) in iter {
        if first {
            first = false;
        } else {
            out.push_str(", ");
        }
        format_user(out, tg_id, user, true);
    }
    if first {
        out.push_str("(no one)");
    }
}

pub fn format_user<'a>(
    out: &mut String,
    tg_id: impl Into<UserId>,
    user: impl Into<Option<&'a crate::models::TgUser>>,
    link: bool,
) {
    match user.into() {
        None => {
            write!(out, "id={} (unknown)", tg_id.into().0).unwrap();
        }
        Some(u) => {
            if link {
                if let Some(username) = &u.username {
                    write!(out, "<a href=\"https://t.me/{username}\">")
                        .unwrap();
                }
            }
            write!(out, "{}", escape(&u.first_name)).unwrap();
            if let Some(last_name) = &u.last_name {
                write!(out, " {}", escape(last_name)).unwrap();
            }
            if link && u.username.is_some() {
                write!(out, "</a>").unwrap();
            }
        }
    }
}

/// Similar to [`teloxide::filter_command`], but for commands implementing
/// [`BotCommandsExtTrait`].
#[must_use]
pub fn filter_command<C>() -> UpdateHandler
where
    C: BotCommands + BotCommandsExtTrait + Send + Sync + 'static,
{
    dptree::filter_map_async(filter_command_impl::<C>)
}

async fn filter_command_impl<C>(
    bot: Bot,
    me: Me,
    msg: Message,
    env: Arc<BotEnv>,
) -> Option<C>
where
    C: BotCommands + BotCommandsExtTrait + Send + Sync + 'static,
{
    let cmd = C::parse(msg.text()?, &me.user.username?).ok()?;
    let rules = cmd.command_rules();

    let error_text = if !rules.in_group
        && (msg.chat.is_group() || msg.chat.is_supergroup())
    {
        Some("This command is not allowed in group chats")
    } else if !rules.in_private && msg.chat.is_private() {
        Some("This command is not allowed in private chats")
    } else if rules.admin
        && !env.config.telegram.admins.contains(&msg.from.as_ref()?.id)
    {
        Some("You must be an admin to execute this command")
    } else if rules.resident
        && !is_resident(&mut env.conn(), msg.from.as_ref()?)
    {
        Some("You must be a resident to execute this command")
    } else if rules.in_resident_chat
        && !env.config.telegram.chats.residential.contains(&msg.chat.id)
    {
        Some("This command is allowed only in resident chat")
    } else {
        None
    };

    if let Some(error_text) = error_text {
        let _ = bot.reply_message(&msg, error_text).await;
        return None;
    }

    Some(cmd)
}

pub fn is_resident(conn: &mut SqliteConnection, user: &User) -> bool {
    crate::schema::residents::table
        .filter(crate::schema::residents::end_date.is_null())
        .filter(crate::schema::residents::tg_id.eq(DbUserId::from(user.id)))
        .count()
        .get_result::<i64>(conn)
        .ok()
        .unwrap_or(0)
        > 0
}

/// A container for associating emojis with topics.
pub struct TopicEmojis(HashMap<String, String>);

impl TopicEmojis {
    /// Fetch emojis for topics from Telegram.
    pub async fn fetch(
        bot: &Bot,
        topics: impl Iterator<Item = &crate::models::TgChatTopic> + Send,
    ) -> Result<Self> {
        let mut emojis = topics
            .filter_map(|t| t.icon_emoji.as_ref())
            .filter(|i| !i.is_empty())
            .cloned()
            .collect_vec();
        emojis.sort();
        emojis.dedup();
        if emojis.is_empty() {
            return Ok(Self(HashMap::new()));
        }
        let emojis = bot
            .get_custom_emoji_stickers(emojis)
            .await?
            .into_iter()
            .filter_map(|e| {
                let StickerKind::CustomEmoji { custom_emoji_id } = e.kind
                else {
                    return None;
                };
                Some((custom_emoji_id, e.emoji?))
            })
            .collect::<HashMap<_, _>>();
        Ok(Self(emojis))
    }

    /// Get emoji for a topic.
    pub fn get(&self, topic: &crate::models::TgChatTopic) -> &str {
        topic.icon_emoji.as_ref().and_then(|e| self.0.get(e)).map_or(
            if GENERAL_THREAD_ID == topic.topic_id.into() {
                "#\u{fe0f}\u{20e3}"
            } else {
                "💬"
            },
            |s| s.as_str(),
        )
    }
}

#[cfg(test)]
mod tests {
    use macro_rules_attribute::derive;

    use super::*;

    #[derive(Debug, BotCommands, BotCommandsExt!)]
    #[command(parse_with = "split")]
    enum MyCommand {
        Defaults,

        #[doc = "Variant 2"]
        WithDoc,

        #[custom(resident = true)]
        WithCustom,

        #[doc = "Variant 4"]
        #[custom(admin = true)]
        WithDocAndCustom,

        #[custom(in_private = true, in_group = true)]
        WithArgsAndCustom(i32, i32),
    }

    #[test]
    fn test() {
        assert_eq!(
            MyCommand::Defaults.command_rules(),
            CommandAccessRules::default()
        );
        assert_eq!(
            MyCommand::WithDoc.command_rules(),
            CommandAccessRules::default()
        );
        assert_eq!(
            MyCommand::WithCustom.command_rules(),
            CommandAccessRules { resident: true, ..Default::default() }
        );
        assert_eq!(
            MyCommand::WithDocAndCustom.command_rules(),
            CommandAccessRules { admin: true, ..Default::default() }
        );
        assert_eq!(
            MyCommand::WithArgsAndCustom(1, 2).command_rules(),
            CommandAccessRules {
                in_private: true,
                in_group: true,
                ..Default::default()
            }
        );
    }
}
