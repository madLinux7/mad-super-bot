use crate::{
    config::Config,
    outbound::{Queue, Request},
};
use std::collections::HashMap;
use tokio::time::Instant;

pub struct Chat<'a> {
    pub user_id: &'a str,
    pub login: &'a str,
    pub text: &'a str,
}
#[derive(Default)]
pub struct Commands {
    accepted: HashMap<String, Instant>,
}
impl Commands {
    pub fn accept(
        &mut self,
        config: &Config,
        bot: &str,
        chat: Chat<'_>,
        now: Instant,
        queue: &mut Queue,
    ) -> bool {
        if chat.user_id == bot || config.blacklist.contains(&chat.login.to_ascii_lowercase()) {
            return false;
        }
        let Some(token) = chat
            .text
            .split_whitespace()
            .next()
            .and_then(|s| s.strip_prefix(&config.prefix))
        else {
            return false;
        };
        let Some(name) = config.command_name(token) else {
            return false;
        };
        if self
            .accepted
            .get(chat.user_id)
            .is_some_and(|last| now.duration_since(*last) < config.user_cooldown)
        {
            return false;
        }
        if !queue.push(Request::command(name, chat.user_id, chat.login, now)) {
            tracing::warn!("Command dropped: outbound queue full");
            return false;
        }
        self.accepted
            .retain(|_, last| now.duration_since(*last) < config.user_cooldown);
        if !config.user_cooldown.is_zero() {
            self.accepted.insert(chat.user_id.into(), now);
        }
        true
    }
}
