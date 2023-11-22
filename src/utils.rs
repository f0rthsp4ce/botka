//! Various self-contained utilities.

mod diesel_json;
mod dptree_ext;
mod format_to;
mod log_error;
mod parsers;
mod replace_urls;
mod teloxide;
mod wikijs;

pub use diesel_json::Sqlizer;
pub use dptree_ext::HandlerExt;
pub(crate) use format_to::format_to;
pub use log_error::ResultExt;
pub use parsers::{deserealize_duration, parse_tgapi_method};
pub use replace_urls::replace_urls_with_titles;
pub use wikijs::{get_wikijs_updates, WikiJsUpdateState};

pub use self::teloxide::{
    write_message_link, BotExt, ChatIdExt, MessageExt, ThreadIdPair, UserExt,
    GENERAL_THREAD_ID,
};
