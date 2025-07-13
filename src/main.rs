#![warn(rust_2018_idioms)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
// Restriction lints
#![warn(
    clippy::clone_on_ref_ptr,
    clippy::deref_by_slicing,
    clippy::if_then_some_else_none,
    clippy::undocumented_unsafe_blocks,
    clippy::unnecessary_cast,
    clippy::unnecessary_safety_comment
)]
// False positives
#![allow(clippy::needless_pass_by_value)] // for dptree handlers
// Style
#![allow(clippy::items_after_statements)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::redundant_closure_for_method_calls)]
// Style in tests
#![cfg_attr(
    test,
    allow(clippy::iter_on_empty_collections, clippy::iter_on_single_items)
)]

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context as _, Result};
use argh::FromArgs;
use common::LdapClientState;
use diesel::sqlite::SqliteConnection;
use diesel::Connection;
use metrics_exporter_prometheus::PrometheusBuilder;
use modules::vortex_of_doom::vortex_of_doom;
use tap::Pipe as _;
use teloxide::dispatching::{Dispatcher, UpdateFilterExt};
use teloxide::payloads::AnswerCallbackQuerySetters;
use teloxide::requests::Requester;
use teloxide::types::{CallbackQuery, Message, Update};
use teloxide::Bot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use utils::{ldap, HandlerExt as _};

mod common;
mod config;
mod db;
mod metrics;
mod models;
mod modules;
mod schema;
mod tracing_proxy;
mod utils;
mod web_srv;

static VERSION: OnceLock<String> = OnceLock::new();

static DB_FILENAME: &str = "db.sqlite3";
static TRACE_FILENAME: &str = "trace.jsonl";

fn version() -> &'static str {
    VERSION.get().expect("VERSION is not set")
}

/// botka
#[derive(FromArgs, PartialEq, Debug)]
struct Args {
    #[argh(option, hidden_help = true, long = "set-revision")]
    set_revision: Option<String>,

    #[argh(subcommand)]
    subcommand: SubCommand,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum SubCommand {
    Bot(SubCommandBot),
    Scrape(SubCommandScrape),
}

/// run the bot
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "bot")]
struct SubCommandBot {
    /// config file
    #[argh(positional)]
    config_file: OsString,
}

/// scrape the log
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "scrape")]
struct SubCommandScrape {
    /// db file
    #[argh(positional)]
    db_file: String,

    /// log file
    #[argh(positional)]
    log_file: OsString,

    /// list of `residential_chats`
    #[argh(positional)]
    residential_chats: Vec<i64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    pretty_env_logger::init();
    let args: Args = argh::from_env();
    VERSION
        .set(args.set_revision.unwrap_or_else(|| {
            git_version::git_version!(fallback = "unknown").to_string()
        }))
        .unwrap();
    log::info!("Version {}", version());
    match args.subcommand {
        SubCommand::Bot(c) => run_bot(&c.config_file).await?,
        SubCommand::Scrape(c) => {
            scrape_log(&c.db_file, &c.log_file, &c.residential_chats)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::redundant_pub_crate)]
async fn run_bot(config_fpath: &OsStr) -> Result<()> {
    let prometheus = PrometheusBuilder::new().install_recorder()?;
    metrics::register_metrics();
    modules::borrowed_items::register_metrics();
    modules::nlp::register_metrics();

    let config: Arc<crate::config::Config> = Arc::new(
        File::open(config_fpath)
            .context("Failed to open config file")?
            .pipe(serde_yaml::from_reader)
            .context("Failed to parse config file")?,
    );

    if config.telegram.passive_mode {
        log::info!("Running in passive mode");
    }

    let reqwest_client = reqwest::ClientBuilder::new()
        .danger_accept_invalid_certs(true)
        .build()?;

    let ldap_client = Arc::new(tokio::sync::Mutex::new(LdapClientState::new()));

    let mut openai_config = async_openai::config::OpenAIConfig::new()
        .with_api_key(config.services.openai.api_key.clone());

    if let Some(api_base) = &config.services.openai.api_base {
        openai_config = openai_config.with_api_base(api_base.clone());
    }

    // Create bot and set API URL
    let bot = Bot::new(&config.telegram.token);
    let proxy_addr = tracing_proxy::start().await?;
    let bot = bot.set_api_url(proxy_addr);

    // Get bot's user ID
    let me = bot.get_me().await?;
    let bot_user_id = me.id.0;
    let bot_env = Arc::new(common::BotEnv {
        conn: Mutex::new(SqliteConnection::establish(&format!(
            "sqlite://{DB_FILENAME}"
        ))?),
        reqwest_client: reqwest_client.clone(),
        openai_client: async_openai::Client::with_config(openai_config),
        config: Arc::<config::Config>::clone(&config),
        config_path: config_fpath.into(),
        ldap_client: Arc::<tokio::sync::Mutex<common::LdapClientState>>::clone(
            &ldap_client,
        ),
        bot_user_id,
    });

    let mac_monitoring_state = modules::mac_monitoring::state();

    let mut dispatcher = Dispatcher::builder(
        bot.clone(),
        dptree::entry()
            // should be the first handler
            .inspect(modules::tg_scraper::inspect_update)
            .inspect(modules::resident_tracker::inspect_update)
            .branch(
                Update::filter_message()
                    .filter(|msg: Message, env: Arc<common::BotEnv>| {
                        !msg.chat.is_channel()
                            && !env.config.telegram.passive_mode
                    })
                    .inspect_err(modules::rename_closed_topics::inspect_message)
                    .inspect_err(modules::forward_topic_pins::inspect_message)
                    .branch(modules::basic::command_handler())
                    .branch(modules::dashboard::command_handler())
                    .branch(modules::userctl::command_handler())
                    .branch(modules::polls::message_handler())
                    .branch(modules::borrowed_items::command_handler())
                    .branch(modules::needs::message_handler())
                    .branch(modules::ask_to_visit::message_handler())
                    .branch(modules::welcome::message_handler())
                    .branch(modules::camera::command_handler())
                    .branch(modules::ldap::command_handler())
                    .branch(modules::butler::command_handler())
                    .branch(modules::butler::guest_token_handler())
                    .branch(modules::tldr::command_handler())
                    .branch(modules::nlp::command_handler())
                    .inspect_err(modules::nlp::store_message)
                    .branch(modules::nlp::message_handler())
                    .branch(modules::nlp::random_message_handler())
                    .endpoint(drop_endpoint),
            )
            .branch(
                Update::filter_callback_query()
                    .branch(modules::needs::callback_handler())
                    .branch(modules::polls::callback_handler())
                    .branch(modules::borrowed_items::callback_handler())
                    .branch(modules::butler::callback_handler())
                    .endpoint(drop_callback_query),
            )
            .branch(modules::polls::poll_answer_handler())
            .endpoint(drop_endpoint),
    )
    .dependencies(dptree::deps![
        modules::forward_topic_pins::state(),
        modules::welcome::state(),
        Arc::clone(&mac_monitoring_state),
        Arc::clone(&bot_env)
    ])
    .build();
    let bot_shutdown_token = dispatcher.shutdown_token().clone();
    let mut set = JoinSet::new();
    set.spawn(async move { dispatcher.dispatch().await });

    let cancel = CancellationToken::new();

    if !bot_env.config.telegram.passive_mode {
        set.spawn(modules::updates::task(
            Arc::clone(&bot_env),
            bot.clone(),
            cancel.clone(),
        ));
    }

    if let Some(ldap_config) = &config.services.ldap {
        // Connect to LDAP server in the background
        // and store the client in the bot_env

        // Spawn a task to connect to LDAP server
        let ldap_config = ldap_config.clone();
        let ldap_client =
            Arc::<tokio::sync::Mutex<common::LdapClientState>>::clone(
                &ldap_client,
            );
        set.spawn(async move {
            let mut attempt = 0;
            loop {
                attempt += 1;
                let mut ldap_state = ldap_client.lock().await;
                if ldap_state.is_initialized() {
                    log::warn!("LDAP client is already initialized");
                    return;
                }
                match ldap::connect(&ldap_config).await {
                    Ok(client) => {
                        ldap_state.set(client);
                        drop(ldap_state);
                        log::info!("Connected to LDAP server");
                    }
                    Err(e) => {
                        let to_wait =
                            std::time::Duration::from_secs(5 * attempt);
                        log::warn!("Failed to connect to LDAP server: {e}");
                        log::warn!("Retrying in {to_wait:?}");
                        tokio::time::sleep(to_wait).await;
                    }
                }
            }
        });
    }

    set.spawn(web_srv::run(
        SqliteConnection::establish(&format!("sqlite://{DB_FILENAME}"))?,
        Arc::clone(&bot_env.config),
        prometheus,
        cancel.clone(),
    ));

    set.spawn(crate::modules::mac_monitoring::watch_loop(
        Arc::clone(&bot_env),
        mac_monitoring_state,
        bot.clone(),
    ));

    set.spawn(vortex_of_doom(
        bot.clone(),
        reqwest_client.clone(),
        Arc::clone(&config),
    ));

    set.spawn(modules::borrowed_items::reminder_task(
        Arc::clone(&bot_env),
        bot.clone(),
        cancel.clone(),
    ));

    run_signal_handler(bot_shutdown_token.clone(), cancel.clone());

    let first_ctrl_c = tokio::signal::ctrl_c();
    tokio::select! {
        _ = first_ctrl_c => {
            let second_ctrl_c = tokio::signal::ctrl_c();
            let wait = tokio::time::sleep(std::time::Duration::from_secs(5));
            log::warn!("Waiting 5 seconds for tasks to finish.");
            log::warn!("You can press ^C to cancel this.");
            tokio::select! {
                _ = second_ctrl_c => {
                    set.abort_all();
                }
                () = wait => {
                    set.abort_all();
                }
            };
        }
    };

    while (set.join_next().await).is_some() {}

    Ok(())
}

fn scrape_log(
    db_fpath: &str,
    log_fpath: &OsStr,
    residential_chats: &[i64],
) -> Result<()> {
    let mut conn = SqliteConnection::establish(db_fpath)?;
    let mut log_file = File::open(log_fpath)?;
    let mut buf_reader = BufReader::new(&mut log_file);
    let mut line = String::new();

    conn.exclusive_transaction(|conn| {
        while buf_reader.read_line(&mut line)? > 0 {
            if line.starts_with(r#"{"__f0bot":""#) {
                // Ignore requests/responses for now
                line.clear();
                continue;
            }
            let update: Update = match serde_json::from_str(&line) {
                Ok(update) => update,
                Err(e) => {
                    log::error!("Failed to parse line: {e} {line}");
                    line.clear();
                    continue;
                }
            };
            modules::tg_scraper::scrape(conn, &update)?;
            modules::resident_tracker::scrape(
                conn,
                &update,
                &residential_chats
                    .iter()
                    .map(|&i| teloxide::types::ChatId(i))
                    .collect::<Vec<_>>(),
            )?;
            line.clear();
        }
        Result::<_, anyhow::Error>::Ok(())
    })?;
    Ok(())
}

async fn drop_callback_query(
    bot: Bot,
    callback_query: CallbackQuery,
) -> Result<()> {
    log::warn!(
        "Unexpected callback query: {:?}",
        serde_json::to_string(&callback_query).unwrap()
    );
    bot.answer_callback_query(&callback_query.id)
        .text("Error: unexpected callback query")
        .await?;
    Ok(())
}

async fn drop_endpoint() -> Result<()> {
    Ok(())
}

fn run_signal_handler(
    bot_shutdown_token: teloxide::dispatching::ShutdownToken,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        loop {
            tokio::signal::ctrl_c().await.expect("Failed to listen for SIGINT");
            cancel.cancel();
            match bot_shutdown_token.shutdown() {
                #[allow(
                    clippy::redundant_pub_crate,
                    // reason = "https://github.com/rust-lang/rust-clippy/issues/10636"
                )]
                Ok(f) => {
                    log::info!(
                        "^C received, trying to shutdown the dispatcher..."
                    );
                    tokio::select! {
                        () = f => {
                            log::info!("dispatcher is shutdown...");
                        }
                        _ = tokio::signal::ctrl_c() => {
                            log::info!("Got another ^C, exiting immediately");
                            std::process::exit(0);
                        }
                    }
                }
                Err(_) => {
                    log::info!("^C received, the dispatcher isn't running, ignoring the signal");
                }
            }
        }
    });
}
