use std::fmt::Debug;
use std::marker::PhantomData;

use diesel::result::Error::DeserializationError;
use diesel::{
    ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl,
    SqliteConnection,
};
use diesel_derive_newtype::DieselNewType;
use salvo_oapi::ToSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use teloxide::types::{ChatId, MessageId, Recipient, ThreadId, UserId};

use crate::{models, schema};

/// A definition for a typed value stored in the database table `options`.
pub struct ConfigOptionDef<T: Serialize + DeserializeOwned> {
    key_name: &'static str,
    phantom: PhantomData<T>,
}

/// A helper macro for defining a `ConfigOptionDef` constant.
macro_rules! config_option_def {
    ($name:ident, $type:ty) => {
        #[allow(non_upper_case_globals)]
        pub const $name: crate::db::ConfigOptionDef<$type> =
            crate::db::ConfigOptionDef::new(stringify!($name));
    };
}
pub(crate) use config_option_def;

impl<T: Serialize + DeserializeOwned> ConfigOptionDef<T> {
    pub const fn new(key_name: &'static str) -> Self {
        Self { key_name, phantom: PhantomData }
    }

    /// Get the value of this option from the database.
    /// Returns `Ok(None)` if the option is not set or deserialization fails.
    pub fn get(
        &self,
        conn: &mut SqliteConnection,
    ) -> diesel::QueryResult<Option<T>> {
        let value: Option<String> = schema::options::table
            .filter(schema::options::name.eq(self.key_name))
            .first::<models::ConfigOption>(conn)
            .optional()?
            .map(|option| option.value);
        match serde_json::from_str::<T>(&value.unwrap_or_default()) {
            Ok(value) => Ok(Some(value)),
            Err(e) => {
                log::error!(
                    "Error deserializing option {}: {e}",
                    self.key_name,
                );
                Ok(None)
            }
        }
    }

    /// Set the value of this option in the database.
    pub fn set(
        &self,
        conn: &mut SqliteConnection,
        value: &T,
    ) -> diesel::QueryResult<()> {
        let value = serde_json::to_string(value)
            .map_err(|e| DeserializationError(Box::new(e)))?;
        diesel::replace_into(schema::options::table)
            .values(models::ConfigOption {
                name: self.key_name.to_string(),
                value,
            })
            .execute(conn)
            .map(|_| ())
    }

    /// Unset the value of this option in the database.
    pub fn unset(
        &self,
        conn: &mut SqliteConnection,
    ) -> diesel::QueryResult<()> {
        diesel::delete(
            schema::options::table
                .filter(schema::options::name.eq(self.key_name)),
        )
        .execute(conn)
        .map(|_| ())
    }
}

macro_rules! make_db_newtype {
    ($name:ident, $inner:ty) => {
        #[derive(
            Copy,
            Clone,
            Debug,
            Eq,
            PartialEq,
            Ord,
            PartialOrd,
            Hash,
            Serialize,
            Deserialize,
            DieselNewType,
            ToSchema,
        )]
        #[doc = "A newtype wrapper for a Telegram type to be stored in the database."]
        pub struct $name($inner);
    };
}

make_db_newtype!(DbUserId, i64);
make_db_newtype!(DbChatId, i64);
make_db_newtype!(DbMessageId, i32);
make_db_newtype!(DbThreadId, i32);

impl From<UserId> for DbUserId {
    fn from(id: UserId) -> Self {
        Self(id.0.try_into().expect("UserId is too big"))
    }
}

impl From<DbUserId> for UserId {
    fn from(id: DbUserId) -> Self {
        Self(id.0.try_into().expect("DbUserId is too big"))
    }
}

impl From<ChatId> for DbChatId {
    fn from(id: ChatId) -> Self {
        Self(id.0)
    }
}

impl From<DbChatId> for ChatId {
    fn from(id: DbChatId) -> Self {
        Self(id.0)
    }
}

impl From<DbChatId> for Recipient {
    fn from(id: DbChatId) -> Self {
        Self::Id(id.into())
    }
}

impl From<MessageId> for DbMessageId {
    fn from(id: MessageId) -> Self {
        Self(id.0)
    }
}

impl From<DbMessageId> for MessageId {
    fn from(id: DbMessageId) -> Self {
        Self(id.0)
    }
}

impl From<ThreadId> for DbThreadId {
    fn from(id: ThreadId) -> Self {
        Self(id.0 .0)
    }
}

impl From<Option<ThreadId>> for DbThreadId {
    fn from(id: Option<ThreadId>) -> Self {
        Self(id.map_or(1, |id| id.0 .0))
    }
}

impl From<DbThreadId> for ThreadId {
    fn from(id: DbThreadId) -> Self {
        Self(MessageId(id.0))
    }
}
