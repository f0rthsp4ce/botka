#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
// False positives
#![allow(clippy::needless_pass_by_value)] // for dptree handlers
// Style
#![allow(clippy::items_after_statements)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::redundant_closure_for_method_calls)]
// FIXME: fix these
#![allow(clippy::too_many_lines)]

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use common::{MyDialogue, State};
use diesel::sqlite::SqliteConnection;
use diesel::Connection;
use itertools::Itertools;
use metrics_exporter_prometheus::PrometheusBuilder;
use teloxide::dispatching::dialogue::InMemStorage;
use teloxide::dispatching::{Dispatcher, HandlerExt, UpdateFilterExt};
use teloxide::payloads::AnswerCallbackQuerySetters;
use teloxide::requests::Requester;
use teloxide::types::{CallbackQuery, Message, Update};
use teloxide::Bot;
use tokio_util::sync::CancellationToken;

mod common;
mod db;
mod models;
mod modules;
mod schema;
mod tracing_proxy;
mod utils;
mod web_srv;

const VERSION: &str = git_version::git_version!(fallback = "unknown");

#[tokio::main]
async fn main() -> Result<()> {
    std::env::set_var("RUST_LOG", "info");
    pretty_env_logger::init();
    log::info!("Version {}", VERSION);

    let args = std::env::args().collect_vec();
    let args = args.iter().map(|s| s.as_str()).collect_vec();
    match args.as_slice()[1..] {
        ["bot", config_file] => run_bot(config_file).await?,
        ["scrape", db_file, log_file] => scrape_log(db_file, log_file)?,
        _ => {
            eprintln!("Usage: {} bot <config.yaml>", args[0]);
            eprintln!("Usage: {} scrape <db.sqlite> <log.txt>", args[0]);
            std::process::exit(1);
        }
    }

    Ok(())
}

async fn run_bot(config_fpath: &str) -> Result<()> {
    let prometheus = PrometheusBuilder::new().install_recorder()?;
    modules::basic::register_metrics();
    modules::borrowed_items::register_metrics();
    modules::updates::register_metrics();
    metrics::register_gauge!("last_tg_update_time");
    metrics::describe_gauge!(
        "last_tg_update_time",
        "Timestamp of the last Telegram update"
    );
    metrics::register_gauge!("last_tg_update_id");
    metrics::describe_gauge!(
        "last_tg_update_id",
        "ID of the last Telegram update"
    );

    let config: models::Config =
        serde_yaml::from_reader(File::open(config_fpath)?)
            .map_err(|e| anyhow::anyhow!("Failed to parse config: {}", e))?;

    let bot_env = Arc::new(common::BotEnv {
        conn: Mutex::new(SqliteConnection::establish(&config.db)?),
        reqwest_client: reqwest::ClientBuilder::new()
            .danger_accept_invalid_certs(true)
            .build()?,
        openai_client: async_openai::Client::with_config(
            async_openai::config::OpenAIConfig::new()
                .with_api_key(config.services.openai.api_key.clone()),
        ),
        config: Arc::new(config),
    });

    let proxy_addr =
        tracing_proxy::start(bot_env.config.log_file.as_str()).await?;
    let bot = Bot::new(&bot_env.config.telegram.token).set_api_url(proxy_addr);

    let mut dispatcher = Dispatcher::builder(
        bot.clone(),
        dptree::entry()
            .inspect(modules::tg_scraper::scrape)
            .inspect(|update: Update| {
                metrics::gauge!("last_tg_update_time", now_f64());
                metrics::gauge!("last_tg_update_id", f64::from(update.id.0));
            })
            .branch(
                Update::filter_message()
                    .enter_dialogue::<Message, InMemStorage<State>, State>()
                    .inspect_async(reset_dialogue_on_command)
                    .branch(modules::basic::command_handler())
                    .branch(modules::debates::command_handler())
                    .branch(modules::userctl::command_handler())
                    .branch(
                        dptree::case![State::Forward]
                            .endpoint(modules::debates::debate_send),
                    )
                    .branch(modules::polls::message_handler())
                    .branch(modules::borrowed_items::command_handler())
                    .endpoint(drop_endpoint),
            )
            .branch(
                Update::filter_callback_query()
                    .branch(modules::polls::callback_handler())
                    .branch(modules::borrowed_items::callback_handler())
                    .endpoint(drop_callback_query),
            )
            .branch(modules::polls::poll_answer_handler())
            .endpoint(drop_endpoint),
    )
    .dependencies(dptree::deps![InMemStorage::<State>::new(), bot_env.clone()])
    .build();
    let bot_shutdown_token = dispatcher.shutdown_token().clone();
    let mut join_handles = Vec::new();
    join_handles.push(tokio::spawn(async move { dispatcher.dispatch().await }));

    let cancel = CancellationToken::new();

    join_handles.push(tokio::spawn(modules::updates::task(
        bot_env.clone(),
        bot.clone(),
        cancel.clone(),
    )));

    join_handles.push(tokio::spawn(web_srv::run(
        SqliteConnection::establish(&bot_env.config.db)?,
        bot_env.config.clone(),
        prometheus,
        cancel.clone(),
    )));

    run_signal_handler(bot_shutdown_token.clone(), cancel.clone());

    futures::future::join_all(join_handles).await;

    Ok(())
}

fn scrape_log(db_fpath: &str, log_fpath: &str) -> Result<()> {
    let mut conn = SqliteConnection::establish(db_fpath)?;
    let mut log_file = File::open(log_fpath)?;
    let mut buf_reader = BufReader::new(&mut log_file);
    let mut line = String::new();

    conn.exclusive_transaction(|conn| {
        while buf_reader.read_line(&mut line)? > 0 {
            let update: Update = serde_json::from_str(&line)?;
            modules::tg_scraper::scrape_raw(conn, update)?;
            line.clear();
        }
        Result::<_, anyhow::Error>::Ok(())
    })?;
    Ok(())
}

async fn reset_dialogue_on_command(msg: Message, dialogue: MyDialogue) {
    let message_is_command =
        msg.entities().and_then(|e| e.first()).is_some_and(|e| {
            e.kind == teloxide::types::MessageEntityKind::BotCommand
                && e.offset == 0
        });
    if message_is_command {
        dialogue.update(State::Start).await.ok();
    }
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
                Ok(f) => {
                    log::info!(
                        "^C received, trying to shutdown the dispatcher..."
                    );
                    f.await;
                    log::info!("dispatcher is shutdown...");
                }
                Err(_) => {
                    log::info!("^C received, the dispatcher isn't running, ignoring the signal");
                }
            }
        }
    });
}

/// Returns the current time in seconds since the Unix epoch. Used in metrics.
fn now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
