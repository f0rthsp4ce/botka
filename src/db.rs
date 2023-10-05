use diesel::result::Error::DeserializationError;
use diesel::{
    ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl,
    SqliteConnection,
};

use crate::{models, schema};

pub struct ConfigOptionDef<T: serde::Serialize + serde::de::DeserializeOwned> {
    pub table_name: &'static str,
    pub phantom: std::marker::PhantomData<T>,
}

macro_rules! config_option_def {
    ($name:ident, $type:ty) => {
        #[allow(non_upper_case_globals)]
        pub const $name: crate::db::ConfigOptionDef<$type> =
            crate::db::ConfigOptionDef {
                table_name: stringify!($name),
                phantom: std::marker::PhantomData,
            };
    };
}
pub(crate) use config_option_def;

impl<T> ConfigOptionDef<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    pub fn get(
        &self,
        conn: &mut SqliteConnection,
    ) -> diesel::QueryResult<Option<T>> {
        get_option(conn, self.table_name)
    }

    pub fn set(
        &self,
        conn: &mut SqliteConnection,
        value: &T,
    ) -> diesel::QueryResult<()> {
        set_option(conn, self.table_name, value)
    }

    pub fn unset(
        &self,
        conn: &mut SqliteConnection,
    ) -> diesel::QueryResult<()> {
        unset_option(conn, self.table_name)
    }
}

fn get_option<T: serde::de::DeserializeOwned>(
    conn: &mut SqliteConnection,
    name: &str,
) -> diesel::QueryResult<Option<T>> {
    let value = schema::options::table
        .filter(schema::options::name.eq(name))
        .first::<models::ConfigOption>(conn)
        .optional()
        .map(|option| option.map(|option| option.value));
    match value {
        Ok(Some(value)) => match serde_json::from_str(&value) {
            Ok(value) => Ok(Some(value)),
            Err(e) => {
                log::error!("Error deserializing option {}: {}", name, e);
                Ok(None)
            }
        },
        Ok(None) => Ok(None),
        Err(e) => Err(e),
    }
}

fn set_option<T: serde::Serialize>(
    conn: &mut SqliteConnection,
    name: &str,
    value: &T,
) -> diesel::QueryResult<()> {
    let value = serde_json::to_string(value)
        .map_err(|e| DeserializationError(Box::new(e)))?;
    diesel::replace_into(schema::options::table)
        .values(models::ConfigOption { name: name.to_string(), value })
        .execute(conn)
        .map(|_| ())
}

fn unset_option(
    conn: &mut SqliteConnection,
    name: &str,
) -> diesel::QueryResult<()> {
    diesel::delete(
        schema::options::table.filter(schema::options::name.eq(name)),
    )
    .execute(conn)
    .map(|_| ())
}
