//! Connectors: chat platforms you talk to your agent through.
//!
//! The Hermes Agent / OpenClaw idea, inside the desktop app: message a bot from
//! your phone and an OpenLeash chat on this machine does the work, with its
//! replies, approvals and questions coming back to you there. Each platform is
//! an adapter (Discord, Telegram, Slack) over a shared [`bridge`] that owns the
//! chats. Each adapter has its own bridge and links file, so a Telegram chat
//! id can never collide with a Discord channel id.
//!
//! These are not MCP "app connectors" (Settings → Connectors → App connectors),
//! which give the agent tools; these give *you* another way in.
//!
//! The config lives in `connectors.json` in the data folder, beside
//! `remote.json` and for the same reason: `settings.json` is sent to the UI,
//! rewritten by a generic patch command and copied when people share configs,
//! and a bot token must take none of those paths.

pub mod bridge;
pub mod discord;
pub mod slack;
pub mod telegram;

use crate::agent::Harness;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Vault {
    pub discord: discord::Config,
    pub telegram: telegram::Config,
    pub slack: slack::Config,
}

pub fn vault_path() -> PathBuf {
    crate::agent::store::data_dir().join("connectors.json")
}

impl Vault {
    pub fn load() -> Vault {
        std::fs::read_to_string(vault_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = vault_path();
        let tmp = path.with_extension("json.tmp");
        let s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, s).map_err(|e| format!("Couldn't save connector settings: {e}"))?;
        restrict(&tmp);
        std::fs::rename(&tmp, &path).map_err(|e| format!("Couldn't save connector settings: {e}"))
    }
}

/// Owner-only on unix. On Windows the file inherits the profile folder's ACL,
/// which already keeps other users out. This is not encryption: anything
/// running as the user can read the token, the same as `settings.json`.
fn restrict(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

pub struct Connectors {
    pub discord: Arc<discord::Discord>,
    pub telegram: Arc<telegram::Telegram>,
    pub slack: Arc<slack::Slack>,
}

impl Connectors {
    /// Load the saved config and bring up whatever is turned on.
    pub fn start(h: Arc<Harness>) -> Arc<Connectors> {
        let v = Vault::load();
        let discord = discord::Discord::new(h.clone(), v.discord);
        discord.apply();
        let telegram = telegram::Telegram::new(h.clone(), v.telegram);
        telegram.apply();
        let slack = slack::Slack::new(h, v.slack);
        slack.apply();
        Arc::new(Connectors {
            discord,
            telegram,
            slack,
        })
    }

    pub fn shutdown(&self) {
        self.discord.shutdown();
        self.telegram.shutdown();
        self.slack.shutdown();
    }

    fn save_discord(&self, cfg: discord::Config) -> Result<(), String> {
        let mut v = Vault::load();
        v.discord = cfg.clone();
        v.save()?;
        self.discord.set_config(cfg);
        self.discord.apply();
        Ok(())
    }

    // Each save re-reads the file and replaces only its own platform, so two
    // platforms saved in quick succession can't drop each other's settings.
    fn save_telegram(&self, cfg: telegram::Config) -> Result<(), String> {
        let mut v = Vault::load();
        v.telegram = cfg.clone();
        v.save()?;
        self.telegram.set_config(cfg);
        self.telegram.apply();
        Ok(())
    }

    fn save_slack(&self, cfg: slack::Config) -> Result<(), String> {
        let mut v = Vault::load();
        v.slack = cfg.clone();
        v.save()?;
        self.slack.set_config(cfg);
        self.slack.apply();
        Ok(())
    }
}

type C<'a> = tauri::State<'a, Arc<Connectors>>;

#[tauri::command]
pub async fn discord_status(c: C<'_>) -> Result<discord::Status, String> {
    Ok(c.discord.status().await)
}

/// Everything but the token. Saving restarts the bot under the new rules, so a
/// user removed from the list is locked out from the next message on.
#[tauri::command]
pub async fn discord_configure(c: C<'_>, cfg: discord::Patch) -> Result<discord::Status, String> {
    let mut next = c.discord.config();
    next.apply(cfg)?;
    c.save_discord(next)?;
    Ok(c.discord.status().await)
}

/// Set or (with an empty string) remove the bot token. Write-only: no command
/// returns it.
#[tauri::command]
pub async fn discord_set_token(c: C<'_>, token: String) -> Result<discord::Status, String> {
    let token = discord::clean_token(&token)?;
    let mut next = c.discord.config();
    next.token = token;
    c.save_discord(next)?;
    Ok(c.discord.status().await)
}

/// Unlink every Discord conversation from its chat, so each starts fresh. The
/// chats themselves stay in the app.
#[tauri::command]
pub async fn discord_forget_chats(c: C<'_>) -> Result<discord::Status, String> {
    c.discord.bridge.forget_all().await;
    Ok(c.discord.status().await)
}

#[tauri::command]
pub async fn telegram_status(c: C<'_>) -> Result<telegram::Status, String> {
    Ok(c.telegram.status().await)
}

/// Everything but the token; saving restarts the poller under the new rules.
#[tauri::command]
pub async fn telegram_configure(
    c: C<'_>,
    cfg: telegram::Patch,
) -> Result<telegram::Status, String> {
    let mut next = c.telegram.config();
    next.apply(cfg)?;
    c.save_telegram(next)?;
    Ok(c.telegram.status().await)
}

/// Set or (with an empty string) remove the bot token. Write-only.
#[tauri::command]
pub async fn telegram_set_token(c: C<'_>, token: String) -> Result<telegram::Status, String> {
    let token = telegram::clean_token(&token)?;
    let mut next = c.telegram.config();
    next.token = token;
    c.save_telegram(next)?;
    Ok(c.telegram.status().await)
}

/// Check the saved token with Telegram without changing anything.
#[tauri::command]
pub async fn telegram_test(c: C<'_>) -> Result<String, String> {
    c.telegram.test().await
}

#[tauri::command]
pub async fn telegram_forget_chats(c: C<'_>) -> Result<telegram::Status, String> {
    c.telegram.bridge.forget_all().await;
    Ok(c.telegram.status().await)
}

#[tauri::command]
pub async fn slack_status(c: C<'_>) -> Result<slack::Status, String> {
    Ok(c.slack.status().await)
}

/// Everything but the tokens; saving reconnects under the new rules.
#[tauri::command]
pub async fn slack_configure(c: C<'_>, cfg: slack::Patch) -> Result<slack::Status, String> {
    let mut next = c.slack.config();
    next.apply(cfg)?;
    c.save_slack(next)?;
    Ok(c.slack.status().await)
}

/// Set or (with an empty string) remove one of the two tokens: `kind` is
/// `"app"` (xapp-…) or `"bot"` (xoxb-…). Write-only.
#[tauri::command]
pub async fn slack_set_token(
    c: C<'_>,
    kind: String,
    token: String,
) -> Result<slack::Status, String> {
    let kind = slack::TokenKind::parse(&kind)?;
    let token = slack::clean_token(kind, &token)?;
    let mut next = c.slack.config();
    match kind {
        slack::TokenKind::App => next.app_token = token,
        slack::TokenKind::Bot => next.bot_token = token,
    }
    c.save_slack(next)?;
    Ok(c.slack.status().await)
}

/// Check the saved tokens with Slack without changing anything.
#[tauri::command]
pub async fn slack_test(c: C<'_>) -> Result<String, String> {
    c.slack.test().await
}

#[tauri::command]
pub async fn slack_forget_chats(c: C<'_>) -> Result<slack::Status, String> {
    c.slack.bridge.forget_all().await;
    Ok(c.slack.status().await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vault_round_trips_and_old_files_load() {
        let dir = std::env::temp_dir().join(format!("ol-conn-{}", std::process::id()));
        let _g = crate::agent::store::test_home(&dir);
        let mut v = Vault::default();
        v.discord.token = "a.b.c".into();
        v.discord.allowed_users = vec!["100000000000000001".into()];
        v.save().unwrap();
        let back = Vault::load();
        assert_eq!(back.discord.token, "a.b.c");
        assert_eq!(back.discord.allowed_users, v.discord.allowed_users);
        // Safe defaults for anything a file doesn't say.
        std::fs::write(vault_path(), r#"{"discord":{"enabled":true}}"#).unwrap();
        let partial = Vault::load();
        assert!(
            partial.discord.enabled && partial.discord.allow_dms && partial.discord.require_mention
        );
        assert_eq!(partial.discord.perm, "auto");
        assert!(partial.discord.missing().is_some());
        // A file from before Telegram and Slack existed: both load switched
        // off, with safe defaults, and Discord is untouched.
        std::fs::write(
            vault_path(),
            r#"{"discord":{"enabled":true,"token":"a.b.c","allowed_users":["100000000000000001"]}}"#,
        )
        .unwrap();
        let old = Vault::load();
        assert!(old.discord.enabled);
        assert_eq!(old.discord.token, "a.b.c");
        assert!(!old.telegram.enabled && old.telegram.token.is_empty());
        assert!(old.telegram.allowed_users.is_empty() && old.telegram.require_mention);
        assert!(!old.telegram.allow_groups);
        assert_eq!(old.telegram.perm, "auto");
        assert!(old.telegram.missing().is_some());
        assert!(
            !old.slack.enabled && old.slack.app_token.is_empty() && old.slack.bot_token.is_empty()
        );
        assert!(old.slack.allowed_users.is_empty() && old.slack.require_mention);
        assert_eq!(old.slack.perm, "auto");
        assert!(old.slack.missing().is_some());
        // Each platform's section round-trips beside the others.
        let mut all = old.clone();
        all.telegram.token = "123456:dummy".into();
        all.slack.app_token = "xapp-dummy".into();
        all.slack.bot_token = "xoxb-dummy".into();
        all.save().unwrap();
        let back = Vault::load();
        assert_eq!(back.discord.token, "a.b.c");
        assert_eq!(back.telegram.token, "123456:dummy");
        assert_eq!(back.slack.app_token, "xapp-dummy");
        assert_eq!(back.slack.bot_token, "xoxb-dummy");
        // Debug output (which is what ends up in logs) never shows a token.
        let dbg = format!("{:?} {:?}", back.telegram, back.slack);
        assert!(!dbg.contains("dummy"), "{dbg}");
        // A corrupt file loads as "off", never as a half-read config.
        std::fs::write(vault_path(), "{not json").unwrap();
        assert!(!Vault::load().discord.enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
