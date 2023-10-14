use std::fmt::Write;
use std::sync::{Arc, Mutex, MutexGuard};

use diesel::{
    ExpressionMethods, OptionalExtension, QueryDsl, QueryResult, RunQueryDsl,
    SqliteConnection,
};
use teloxide::dispatching::dialogue::InMemStorage;
use teloxide::dispatching::DpHandlerDescription;
use teloxide::prelude::Dialogue;
use teloxide::types::{Me, Message, User, UserId};
use teloxide::utils::command::BotCommands;
use teloxide::utils::html::escape;
use teloxide::Bot;

use crate::db::DbUserId;
use crate::models::Resident;
use crate::utils::BotExt;

pub type CommandHandler<Output> = dptree::Handler<
    'static,
    dptree::di::DependencyMap,
    Output,
    DpHandlerDescription,
>;

#[derive(Clone, Default)]
pub enum State {
    #[default]
    Start,
    Forward,
}

pub type MyDialogue = Dialogue<State, InMemStorage<State>>;

/// Rules describing where and who can execute a command.
#[derive(Eq, PartialEq, Debug)]
pub struct CommandRules {
    /// Required minimal role to execute this command
    pub role: Role,
    /// Allow users to execute this command in private chat with bot
    pub in_private: bool,
    /// Allow users to execute this command in group chat
    pub in_group: bool,
}

#[derive(Eq, PartialEq, Debug, Default, Copy, Clone, Ord, PartialOrd)]
pub enum Role {
    #[default]
    User,
    Resident,
    Admin,
}

impl Default for CommandRules {
    fn default() -> Self {
        Self { role: Role::User, in_private: true, in_group: true }
    }
}

pub trait HasCommandRules {
    fn command_rules(&self) -> CommandRules;
}

pub struct BotEnv {
    pub conn: Mutex<SqliteConnection>,
    pub config: crate::models::Config,
    pub reqwest_client: reqwest::Client,
    pub openai_client: async_openai::Client<async_openai::config::OpenAIConfig>,
}

impl BotEnv {
    pub fn conn(&self) -> MutexGuard<SqliteConnection> {
        self.conn.lock().unwrap()
    }
    pub fn transaction<T>(
        &self,
        f: impl FnOnce(&mut SqliteConnection) -> QueryResult<T>,
    ) -> QueryResult<T> {
        self.conn().exclusive_transaction(f)
    }
}

/// Derive macro for `HasCommandRules` trait. Should be applied with
/// `macro_rules_attribute::derive`.
#[macro_export]
macro_rules! HasCommandRules {
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
        impl $crate::common::HasCommandRules for $name {
            fn command_rules(&self) -> $crate::common::CommandRules {
                match self {
                    $(
                        HasCommandRules!(
                            impl skip_item_args;
                            $item $( ( $($item_args)* ) )?
                        ) => {
                            #[allow(unused_mut)]
                            let mut meta = $crate::common::CommandRules::default();
                            HasCommandRules!(impl set_meta; meta; $( #[ $($attr)* ] )* );
                            meta
                        }
                    )*
                }
            }
        }
    };

    // Internal rules, using https://stackoverflow.com/a/40484901 trick
    // set_meta
    (
        impl set_meta;
        $name:expr;
        #[custom( $( $meta_key:ident = $meta_value:expr ),* $(,)? )]
        $( #[ $( $rest:tt )* ] )*
    ) => {
        $( $name.$meta_key = $meta_value; )*
        HasCommandRules!(impl set_meta; $name; $( #[ $( $rest )* ] )* );
    };
    (
        impl set_meta;
        $name:expr;
        #[ $attr:meta ]
        $( #[ $( $rest:tt )* ] )*
    ) => {
        HasCommandRules!(impl set_meta; $name; $( #[ $( $rest )* ] )* );
    };
    (
        impl set_meta;
        $name:expr;
    ) => {};

    // skip_item_args
    (impl skip_item_args; $v:ident ) => { Self::$v };
    (impl skip_item_args; $v:ident($($t:ty),+) ) => { Self::$v(..) };
}

pub fn format_users<'a>(
    iter: impl Iterator<
        Item = (&'a crate::models::Resident, &'a Option<crate::models::TgUser>),
    >,
) -> String {
    let mut text = String::new();
    let mut first = true;
    for (resident, user) in iter {
        if first {
            first = false;
        } else {
            text.push_str(", ");
        }
        text.push_str(format_user(resident.tg_id, user).as_str());
    }
    if first {
        text.push_str("(no one)");
    }
    text
}

pub fn format_users2<'a>(
    out: &mut String,
    iter: impl Iterator<Item = (DbUserId, &'a Option<crate::models::TgUser>)>,
) {
    let mut first = true;
    for (tg_id, user) in iter {
        if first {
            first = false;
        } else {
            out.push_str(", ");
        }
        format_user2(out, tg_id, user);
    }
    if first {
        out.push_str("(no one)");
    }
}

fn format_user(
    tg_id: DbUserId,
    user: &Option<crate::models::TgUser>,
) -> String {
    match user {
        None => {
            format!("id={} (unknown)", UserId::from(tg_id).0)
        }
        Some(crate::models::TgUser { username: Some(username), .. }) => {
            format!("@{username}")
        }
        Some(crate::models::TgUser { first_name, .. }) => first_name.clone(),
    }
}

pub fn format_user2(
    out: &mut String,
    tg_id: DbUserId,
    user: &Option<crate::models::TgUser>,
) {
    match user {
        None => {
            write!(out, "id={} (unknown)", UserId::from(tg_id).0).unwrap();
        }
        Some(u) => {
            if let Some(username) = &u.username {
                write!(out, "<a href=\"https://t.me/{username}\">").unwrap();
            }
            write!(out, "{}", escape(&u.first_name)).unwrap();
            if let Some(last_name) = &u.last_name {
                write!(out, " {}", escape(last_name)).unwrap();
            }
            if u.username.is_some() {
                write!(out, "</a>").unwrap();
            }
        }
    }
}

#[must_use]
pub fn filter_command<C, Output>() -> CommandHandler<Output>
where
    C: BotCommands + HasCommandRules + Send + Sync + 'static,
    Output: Send + Sync + 'static,
{
    dptree::filter_map_async(filter_command2::<C>)
}

async fn filter_command2<C>(
    bot: Bot,
    me: Me,
    msg: Message,
    env: Arc<BotEnv>,
) -> Option<C>
where
    C: BotCommands + HasCommandRules + Send + Sync + 'static,
{
    let cmd = C::parse(msg.text()?, &me.user.username?).ok()?;
    let rules = cmd.command_rules();

    let error_text = if !rules.in_group
        && (msg.chat.is_group() || msg.chat.is_supergroup())
    {
        Some("This command is not allowed in group chats")
    } else if !rules.in_private && msg.chat.is_private() {
        Some("This command is not allowed in private chats")
    } else if rules.role != Role::User {
        let user = msg.from()?;
        let user_role = user_role(&mut env.conn(), user);
        if user_role < rules.role {
            Some("You don't have enough permissions to execute this command")
        } else {
            None
        }
    } else {
        None
    };

    if let Some(error_text) = error_text {
        let _ = bot.reply_message(&msg, error_text).await;
        return None;
    }

    Some(cmd)
}

pub fn user_role(conn: &mut SqliteConnection, user: &User) -> Role {
    let resident: Option<Resident> = crate::schema::residents::table
        .filter(crate::schema::residents::tg_id.eq(DbUserId::from(user.id)))
        .first::<Resident>(conn)
        .optional()
        .ok()
        .flatten();
    match resident {
        Some(Resident { is_bot_admin: true, .. }) => Role::Admin,
        Some(Resident { is_resident: true, .. }) => Role::Resident,
        _ => Role::User,
    }
}

#[cfg(test)]
mod tests {
    use macro_rules_attribute::derive;

    use super::*;

    #[derive(Debug, HasCommandRules!)]
    enum MyCommand {
        Defaults,

        #[doc = "Variant 2"]
        WithDoc,

        #[custom(role = Role::Resident)]
        WithCustom,

        #[doc = "Variant 4"]
        #[custom(role = Role::Admin)]
        WithDocAndCustom,

        #[custom(in_private = true, in_group = true)]
        WithArgsAndCustom(i32, i32),
    }

    #[test]
    fn test() {
        assert_eq!(
            MyCommand::Defaults.command_rules(),
            CommandRules::default()
        );
        assert_eq!(MyCommand::WithDoc.command_rules(), CommandRules::default());
        assert_eq!(
            MyCommand::WithCustom.command_rules(),
            CommandRules { role: Role::Resident, ..Default::default() }
        );
        assert_eq!(
            MyCommand::WithDocAndCustom.command_rules(),
            CommandRules { role: Role::Admin, ..Default::default() }
        );
        assert_eq!(
            MyCommand::WithArgsAndCustom(1, 2).command_rules(),
            CommandRules {
                in_private: true,
                in_group: true,
                ..Default::default()
            }
        );
    }
}
