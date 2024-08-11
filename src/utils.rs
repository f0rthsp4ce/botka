//! Various self-contained utilities.

mod diesel_json;
mod dptree_ext;
mod espcam;
mod format_to;
pub mod ldap;
mod log_error;
pub mod mikrotik;
mod parsers;
mod replace_urls;
mod status_change;
mod teloxide;
mod wikijs;

pub use diesel_json::Sqlizer;
pub use dptree_ext::HandlerExt;
pub use espcam::read_camera_image;
pub(crate) use format_to::format_to;
pub use log_error::ResultExt;
pub use parsers::{
    deserealize_duration, parse_tg_thread_link, parse_tgapi_method,
};
pub use replace_urls::replace_urls_with_titles;
pub use status_change::StatusChangeDetector;
pub use wikijs::{get_wikijs_page, get_wikijs_updates, WikiJsUpdateState};

pub use self::teloxide::{
    write_message_link, BotExt, ChatIdExt, MessageExt, ThreadIdPair, UserExt,
    GENERAL_THREAD_ID,
};
