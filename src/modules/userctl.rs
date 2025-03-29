//! `/userctl` command to add or remove MAC addresses.

use std::sync::Arc;

use anyhow::Result;
use argh::FromArgs;
use diesel::prelude::*;
use macro_rules_attribute::derive;
use teloxide::macros::BotCommands;
use teloxide::prelude::*;
use teloxide::utils::html;

use crate::common::{filter_command, BotCommandsExt, BotEnv, UpdateHandler};
use crate::db::DbUserId;
use crate::models;
use crate::utils::BotExt;

#[derive(Clone, BotCommands, BotCommandsExt!)]
#[command(rename_rule = "snake_case")]
pub enum Commands {
    #[command(
        description = "control personal configuration, see <code>/userctl --help</code>."
    )]
    Userctl(String),

    #[command(description = "add an SSH public key for yourself.")]
    #[custom(resident = true)]
    AddSsh(String),

    #[command(description = "get SSH public keys of a user by username.")]
    #[custom(resident = true)]
    GetSsh(String),

    #[command(description = "add a user as a resident by username or ID.")]
    #[custom(admin = true)]
    AddResident(String),

    #[command(description = "remove a user from residents by username or ID.")]
    #[custom(admin = true)]
    RemoveResident(String),
}

/// Control personal configuration.
#[derive(argh::FromArgs, Debug)]
struct UserctlArgs {
    /// add mac address
    #[argh(option)]
    add_mac: Vec<macaddr::MacAddr6>,

    /// remove mac address
    #[argh(option)]
    remove_mac: Vec<macaddr::MacAddr6>,
}

pub fn command_handler() -> UpdateHandler {
    filter_command::<Commands>().endpoint(handle_command)
}

async fn handle_command(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    command: Commands,
) -> Result<()> {
    match command {
        Commands::Userctl(args) => cmd_userctl(bot, env, msg, args).await,
        Commands::AddSsh(args) => cmd_add_ssh(bot, env, msg, args).await,
        Commands::GetSsh(args) => cmd_get_ssh(bot, env, msg, args).await,
        Commands::AddResident(args) => {
            cmd_add_resident(bot, env, msg, args).await
        }
        Commands::RemoveResident(args) => {
            cmd_remove_resident(bot, env, msg, args).await
        }
    }
}

async fn cmd_userctl(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    args: String,
) -> Result<()> {
    let Some(from) = &msg.from else { return Ok(()) };
    let args = args.split_whitespace().collect::<Vec<_>>();
    let args = match UserctlArgs::from_args(&["/userctl"], &args) {
        Ok(args) => args,
        Err(ee) => {
            bot.reply_message(&msg, ee.output).await?;
            return Ok(());
        }
    };

    let tg_id = DbUserId::from(from.id);
    let updated_macs = env.transaction(|conn| {
        diesel::delete(crate::schema::user_macs::table)
            .filter(crate::schema::user_macs::tg_id.eq(tg_id))
            .filter(
                crate::schema::user_macs::mac
                    .eq_any(args.remove_mac.iter().map(|m| m.to_string())),
            )
            .execute(conn)?;
        diesel::insert_into(crate::schema::user_macs::table)
            .values(
                args.add_mac
                    .iter()
                    .map(|m| {
                        (
                            crate::schema::user_macs::tg_id.eq(tg_id),
                            crate::schema::user_macs::mac.eq(m.to_string()),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .execute(conn)?;

        let macs = crate::schema::user_macs::table
            .filter(crate::schema::user_macs::tg_id.eq(tg_id))
            .select(crate::schema::user_macs::mac)
            .load::<String>(conn)?;

        Ok(macs)
    })?;

    bot.reply_message(&msg, format!("Updated: {updated_macs:?}")).await?;

    Ok(())
}

async fn cmd_add_ssh(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    args: String,
) -> Result<()> {
    let Some(from) = &msg.from else { return Ok(()) };

    // Validate SSH key format
    let ssh_key = args.trim();
    if !is_valid_ssh_key(ssh_key) {
        bot.reply_message(
            &msg,
            "Invalid SSH key format. Please provide a valid public SSH key.",
        )
        .await?;
        return Ok(());
    }

    // Check if user has reached the limit of 10 keys
    let tg_id = DbUserId::from(from.id);
    let user_keys_count = env.transaction(|conn| {
        use crate::schema::user_ssh_keys::dsl as s;
        s::user_ssh_keys
            .filter(s::tg_id.eq(tg_id))
            .count()
            .get_result::<i64>(conn)
    })?;

    if user_keys_count >= 10 {
        bot.reply_message(&msg, "You have reached the maximum limit of 10 SSH keys. Please remove some keys before adding more.")
            .await?;
        return Ok(());
    }

    // Add the key to the database
    let result = env.transaction(|conn| {
        use crate::schema::user_ssh_keys::dsl as s;
        diesel::insert_into(s::user_ssh_keys)
            .values((s::tg_id.eq(tg_id), s::key.eq(ssh_key)))
            .execute(conn)
    });

    match result {
        Ok(_) => {
            bot.reply_message(&msg, "SSH key added successfully.").await?;
        }
        Err(e) => {
            if let diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) = e
            {
                bot.reply_message(
                    &msg,
                    "This SSH key is already associated with your account.",
                )
                .await?;
            } else {
                bot.reply_message(&msg, format!("Failed to add SSH key: {e}"))
                    .await?;
            }
        }
    }

    Ok(())
}

async fn cmd_get_ssh(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    args: String,
) -> Result<()> {
    let username = args.trim();
    if username.is_empty() {
        bot.reply_message(&msg, "Please provide a username.").await?;
        return Ok(());
    }

    // Find the user by username
    let user_id = env.transaction(|conn| {
        use crate::schema::tg_users::dsl as t;
        t::tg_users
            .filter(t::username.eq(username))
            .select(t::id)
            .first::<DbUserId>(conn)
            .optional()
    })?;

    let Some(user_id) = user_id else {
        bot.reply_message(
            &msg,
            format!("User with username '{username}' not found."),
        )
        .await?;
        return Ok(());
    };

    // Check if the user is a resident
    let is_resident = env.transaction(|conn| {
        use crate::schema::residents::dsl as r;
        r::residents
            .filter(r::tg_id.eq(user_id))
            .filter(r::end_date.is_null())
            .count()
            .get_result::<i64>(conn)
    })? > 0;

    if !is_resident {
        bot.reply_message(
            &msg,
            format!("User '{username}' is not a resident."),
        )
        .await?;
        return Ok(());
    }

    // Get the user's SSH keys
    let keys = env.transaction(|conn| {
        use crate::schema::user_ssh_keys::dsl as s;
        s::user_ssh_keys
            .filter(s::tg_id.eq(user_id))
            .select(s::key)
            .load::<String>(conn)
    })?;

    if keys.is_empty() {
        bot.reply_message(&msg, format!("User '{username}' has no SSH keys."))
            .await?;
        return Ok(());
    }

    let mut response = format!("SSH keys for user '{username}':\n\n");
    for key in keys {
        response.push_str(&format!("<pre>{}</pre>\n\n", html::escape(&key)));
    }

    bot.reply_message(&msg, response)
        .parse_mode(teloxide::types::ParseMode::Html)
        .await?;

    Ok(())
}

async fn cmd_add_resident(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    args: String,
) -> Result<()> {
    let args = args.trim();
    if args.is_empty() {
        bot.reply_message(&msg, "Please provide a Telegram username or ID.")
            .await?;
        return Ok(());
    }

    // Try to find the user by username or ID
    let user = find_user_by_username_or_id(&env, args)?;
    let Some(user) = user else {
        bot.reply_message(&msg, "User not found.").await?;
        return Ok(());
    };

    // Check if user is already a resident
    let is_resident = env.transaction(|conn| {
        use crate::schema::residents::dsl as r;
        r::residents
            .filter(r::tg_id.eq(user.id))
            .filter(r::end_date.is_null())
            .count()
            .get_result::<i64>(conn)
    })? > 0;

    if is_resident {
        let mut response = String::new();
        response.push_str("User ");
        crate::common::format_user(&mut response, user.id, Some(&user), false);
        response.push_str(" is already a resident.");
        bot.reply_message(&msg, response)
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
        return Ok(());
    }

    // Add user as resident
    env.transaction(|conn| {
        use crate::schema::residents::dsl as r;
        diesel::insert_into(r::residents)
            .values((r::tg_id.eq(user.id), r::begin_date.eq(diesel::dsl::now)))
            .execute(conn)
    })?;

    let mut response = String::new();
    response.push_str("User ");
    crate::common::format_user(&mut response, user.id, Some(&user), false);
    response.push_str(" has been added as a resident.");
    bot.reply_message(&msg, response)
        .parse_mode(teloxide::types::ParseMode::Html)
        .await?;

    Ok(())
}

async fn cmd_remove_resident(
    bot: Bot,
    env: Arc<BotEnv>,
    msg: Message,
    args: String,
) -> Result<()> {
    let args = args.trim();
    if args.is_empty() {
        bot.reply_message(&msg, "Please provide a Telegram username or ID.")
            .await?;
        return Ok(());
    }

    // Try to find the user by username or ID
    let user = find_user_by_username_or_id(&env, args)?;
    let Some(user) = user else {
        bot.reply_message(&msg, "User not found.").await?;
        return Ok(());
    };

    // Check if user is a resident
    let is_resident = env.transaction(|conn| {
        use crate::schema::residents::dsl as r;
        r::residents
            .filter(r::tg_id.eq(user.id))
            .filter(r::end_date.is_null())
            .count()
            .get_result::<i64>(conn)
    })? > 0;

    if !is_resident {
        let mut response = String::new();
        response.push_str("User ");
        crate::common::format_user(&mut response, user.id, Some(&user), false);
        response.push_str(" is not a resident.");
        bot.reply_message(&msg, response)
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
        return Ok(());
    }

    // Remove user from residents
    let rows_affected = env.transaction(|conn| {
        use crate::schema::residents::dsl as r;
        diesel::update(r::residents)
            .filter(r::tg_id.eq(user.id))
            .filter(r::end_date.is_null())
            .set(r::end_date.eq(diesel::dsl::now))
            .execute(conn)
    })?;

    let mut response = String::new();
    if rows_affected > 0 {
        response.push_str("User ");
        crate::common::format_user(&mut response, user.id, Some(&user), false);
        response.push_str(" has been removed from residents.");
        bot.reply_message(&msg, response)
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
    } else {
        response.push_str("Failed to remove user ");
        crate::common::format_user(&mut response, user.id, Some(&user), false);
        response.push_str(" from residents.");
        bot.reply_message(&msg, response)
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
    }

    Ok(())
}

fn find_user_by_username_or_id(
    env: &Arc<BotEnv>,
    username_or_id: &str,
) -> Result<Option<models::TgUser>> {
    // Try to parse as ID first
    if let Ok(user_id) = username_or_id.parse::<u64>() {
        let user_id = UserId(user_id);
        let user = env.transaction(|conn| {
            use crate::schema::tg_users::dsl as t;
            t::tg_users
                .filter(t::id.eq(DbUserId::from(user_id)))
                .first::<models::TgUser>(conn)
                .optional()
        })?;

        if user.is_some() {
            return Ok(user);
        }
    }

    // If not found by ID or parsing failed, try as username
    let username = username_or_id.trim_start_matches('@');
    let user = env.transaction(|conn| {
        use crate::schema::tg_users::dsl as t;
        t::tg_users
            .filter(t::username.eq(username))
            .first::<models::TgUser>(conn)
            .optional()
    })?;

    Ok(user)
}

fn is_valid_ssh_key(key: &str) -> bool {
    let parts: Vec<&str> = key.split_whitespace().collect();
    if parts.len() < 2 {
        return false;
    }

    // Check if the key type is recognized
    matches!(
        parts[0],
        "ssh-rsa"
            | "ssh-ed25519"
            | "ecdsa-sha2-nistp256"
            | "ecdsa-sha2-nistp384"
            | "ecdsa-sha2-nistp521"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssh_key_validation() {
        // Valid keys
        assert!(is_valid_ssh_key(
            "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC comment"
        ));
        assert!(is_valid_ssh_key(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGit comment"
        ));
        assert!(is_valid_ssh_key(
            "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAI"
        ));

        // Invalid keys
        assert!(!is_valid_ssh_key("invalid-key"));
        assert!(!is_valid_ssh_key(""));
        assert!(!is_valid_ssh_key(
            "ssh-invalid AAAAB3NzaC1yc2EAAAADAQABAAABAQC"
        ));
    }
}
