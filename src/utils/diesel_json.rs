//! Store any serde-serializable type as JSON string in diesel database.
//! TODO: use <https://github.com/PPakalns/diesel_json>

use std::fmt::Debug;
use std::ops::Deref;

use diesel::backend::Backend;
use diesel::deserialize::FromSql;
use diesel::serialize::ToSql;
use diesel::{AsExpression, FromSqlRow};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, FromSqlRow, AsExpression)]
#[diesel(sql_type = diesel::sql_types::Text)]
/// A wrapper type for any serde-serializable type to store in diesel database.
pub struct Sqlizer<T>(T, String);

impl<T: Serialize + Debug> Sqlizer<T> {
    pub fn new(t: T) -> Result<Self, serde_json::Error> {
        let s = serde_json::to_string(&t)?;
        Ok(Self(t, s))
    }
    pub fn map(
        &self,
        f: impl FnOnce(&T) -> T,
    ) -> Result<Self, serde_json::Error> {
        let t = f(&self.0);
        let s = serde_json::to_string(&t)?;
        Ok(Self(t, s))
    }
}

impl<T> Deref for Sqlizer<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> AsRef<T> for Sqlizer<T> {
    fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<DB, T> ToSql<diesel::sql_types::Text, DB> for Sqlizer<T>
where
    DB: Backend,
    T: Debug,
    str: ToSql<diesel::sql_types::Text, DB>,
{
    fn to_sql<'b>(
        &'b self,
        out: &mut diesel::serialize::Output<'b, '_, DB>,
    ) -> diesel::serialize::Result {
        self.1.as_str().to_sql(out)
    }
}

impl<DB, T> FromSql<diesel::sql_types::Text, DB> for Sqlizer<T>
where
    DB: diesel::backend::Backend,
    T: for<'de> Deserialize<'de> + Debug,
    String: FromSql<diesel::sql_types::Text, DB>,
{
    fn from_sql(bytes: DB::RawValue<'_>) -> diesel::deserialize::Result<Self> {
        let s = String::from_sql(bytes)?;
        let t = serde_json::from_str(&s)?;
        Ok(Self(t, s))
    }
}
