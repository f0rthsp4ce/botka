//! Commands related to LDAP.

use std::sync::Arc;

use anyhow::Result;
use argh::FromArgs;
use macro_rules_attribute::derive;
use passwords::PasswordGenerator;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::common::{filter_command, BotCommandsExt, BotEnv, UpdateHandler};
use crate::utils::{ldap, BotExt};

const PASSWORD_GENERATOR: PasswordGenerator = PasswordGenerator {
    length: 24,
    numbers: true,
    lowercase_letters: true,
    uppercase_letters: true,
    symbols: false,
    spaces: false,
    exclude_similar_characters: false,
    strict: true,
};

#[allow(clippy::enum_variant_names)]
#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[command(description = "Register in LDAP.")]
    #[custom(in_group = false, resident = true)]
    LdapRegister(String),
    #[command(description = "Reset LDAP password.")]
    #[custom(in_group = false, resident = true)]
    LdapResetPassword,
    #[command(description = "Update LDAP settings.")]
    #[custom(in_group = false, resident = true)]
    LdapUpdate(String),
    // #[command(description = "Show your LDAP groups.")]
    // #[custom(in_group = false, resident = true)]
    // LdapGroups,
}

/// Control personal configuration.
#[derive(argh::FromArgs, Debug)]
struct LdapRegisterArgs {
    /// email
    #[argh(positional)]
    mail: String,

    /// username
    #[argh(positional)]
    username: Option<String>,
}

/// Control personal configuration.
#[derive(argh::FromArgs, Debug)]
struct LdapUpdateArgs {
    /// email
    #[argh(option)]
    mail: Option<String>,

    /// username
    #[argh(option)]
    username: Option<String>,

    /// display name
    #[argh(option)]
    display_name: Option<String>,
}

pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(start)
}

async fn start(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    command: Commands,
) -> Result<()> {
    match command {
        Commands::LdapRegister(args) => {
            ldap_register(bot, env, msg, &args).await?;
        }
        Commands::LdapResetPassword => {
            ldap_reset_password(bot, env, msg).await?;
        }
        Commands::LdapUpdate(args) => ldap_update(bot, env, msg, &args).await?,
        // Commands::LdapGroups => ldap_groups(bot, env, msg).await?,
    }
    Ok(())
}

async fn ldap_not_found(bot: Bot, msg: Message) -> Result<()> {
    bot.reply_message(&msg, "You are not in the LDAP database. You need to do /ldap_register first.").await?;
    Ok(())
}

fn user_full_name(user: &teloxide::types::User) -> String {
    user.last_name.as_ref().map_or_else(
        || user.first_name.clone(),
        |last_name| format!("{} {}", user.first_name, last_name),
    )
}

async fn ldap_register(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    args: &str,
) -> Result<()> {
    let args = args.split_whitespace().collect::<Vec<_>>();
    let args = match LdapRegisterArgs::from_args(&["/ldap_register"], &args) {
        Ok(args) => args,
        Err(ee) => {
            bot.reply_message(&msg, ee.output).await?;
            return Ok(());
        }
    };

    let mut ldap_state = env.ldap_client().await;
    let ldap_conn = ldap_state.get()?;
    let ldap_config = env.ldap_config()?;

    let user =
        msg.from.as_ref().ok_or_else(|| anyhow::anyhow!("No user ID"))?;

    if ldap::get_user(ldap_conn, ldap_config, user.id).await?.is_some() {
        bot.reply_message(
            &msg,
            "You are already registered in the LDAP database.",
        )
        .await?;
        return Ok(());
    }

    let Some(username) = args.username.or_else(|| user.username.clone()) else {
        bot.reply_message(&msg, "No username provided or found.").await?;
        return Ok(());
    };

    let mut ldap_user = ldap::User::new_from_telegram(
        ldap_config,
        user.id,
        &username,
        &args.mail,
        Some(user_full_name(user)),
    );
    let password = PASSWORD_GENERATOR.generate_one().unwrap();
    ldap_user.update_password(ldap::Sha512PasswordHash::new(), &password);

    ldap::add_user(ldap_conn, ldap_config, &ldap_user).await?;
    ldap::add_user_to_group(
        ldap_conn,
        ldap_config,
        &ldap_user,
        &ldap_config.attributes.resident_group,
    )
    .await?;

    drop(ldap_state);

    bot.reply_message(&msg, format!("You have been registered in the LDAP database with password: <code>{password}</code>."))
    .parse_mode(teloxide::types::ParseMode::Html)
        .await?;

    Ok(())
}

async fn ldap_update(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    args: &str,
) -> Result<()> {
    let args = args.split_whitespace().collect::<Vec<_>>();
    let args = match LdapUpdateArgs::from_args(&["/ldap_update"], &args) {
        Ok(args) => args,
        Err(ee) => {
            bot.reply_message(&msg, ee.output).await?;
            return Ok(());
        }
    };

    let mut ldap_state = env.ldap_client().await;
    let ldap_conn = ldap_state.get()?;
    let ldap_config = env.ldap_config()?;
    let user_id =
        msg.from.as_ref().ok_or_else(|| anyhow::anyhow!("No user ID"))?.id;
    let Some(mut user) =
        ldap::get_user(ldap_conn, ldap_config, user_id).await?
    else {
        ldap_not_found(bot, msg).await?;
        return Ok(());
    };

    if let Some(email) = args.mail {
        user.mail = Some(email);
    }
    if let Some(username) = args.username {
        user.sn = username;
    }
    if let Some(display_name) = args.display_name {
        user.display_name = Some(display_name);
    }

    ldap::update_user(ldap_conn, ldap_config, &user).await?;

    drop(ldap_state);

    bot.reply_message(&msg, "Your LDAP settings have been updated.").await?;
    Ok(())
}

async fn ldap_reset_password(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
) -> Result<()> {
    let mut ldap_state = env.ldap_client().await;
    let ldap_conn = ldap_state.get()?;
    let ldap_config = env.ldap_config()?;

    let user_id =
        msg.from.as_ref().ok_or_else(|| anyhow::anyhow!("No user ID"))?.id;
    let Some(mut user) =
        ldap::get_user(ldap_conn, ldap_config, user_id).await?
    else {
        ldap_not_found(bot, msg).await?;
        return Ok(());
    };

    let password = PASSWORD_GENERATOR.generate_one().unwrap();

    user.update_password(ldap::Sha512PasswordHash::new(), &password);
    ldap::update_user(ldap_conn, ldap_config, &user).await?;

    drop(ldap_state);

    bot.reply_message(
        &msg,
        format!("Your new password is <code>{password}</code>."),
    )
    .parse_mode(teloxide::types::ParseMode::Html)
    .await?;
    Ok(())
}

#[allow(dead_code)]
#[allow(clippy::significant_drop_tightening)]
async fn ldap_groups(bot: Bot, env: Arc<BotEnv>, msg: Message) -> Result<()> {
    let mut ldap_state = env.ldap_client().await;
    let ldap_conn = ldap_state.get()?;
    let ldap_config = env.ldap_config()?;
    let user_id =
        msg.from.as_ref().ok_or_else(|| anyhow::anyhow!("No user ID"))?.id;
    let Some(user) = ldap::get_user(ldap_conn, ldap_config, user_id).await?
    else {
        ldap_not_found(bot, msg).await?;
        return Ok(());
    };

    let groups = ldap::get_user_groups(ldap_conn, ldap_config, &user).await?;

    let mut text = "Your LDAP groups:\n".to_string();
    for group in groups {
        text.push_str(&format!("- {group}\n"));
    }

    bot.reply_message(&msg, text).await?;
    Ok(())
}
