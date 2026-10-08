use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::time::Instant;

use super::manager::Message;
use crate::{Server, client};

const REFRESH_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub struct TargetsTimestamps {
    written: HashMap<Server, DateTime<Utc>>,
    refreshed_at: Instant,
}

impl Default for TargetsTimestamps {
    fn default() -> Self {
        Self {
            written: HashMap::new(),
            refreshed_at: Instant::now(),
        }
    }
}

impl TargetsTimestamps {
    pub fn refresh(
        &mut self,
        now: Instant,
        clients: &client::Map,
    ) -> Vec<BoxFuture<'static, Message>> {
        if now.duration_since(self.refreshed_at) < REFRESH_EVERY {
            return vec![];
        }

        self.refreshed_at = now;

        self.write_unwritten(clients.chathistory_targets_timestamps())
    }

    pub fn server_disconnected(
        &mut self,
        server: &Server,
        clients: &client::Map,
    ) -> Vec<BoxFuture<'static, Message>> {
        self.write_unwritten(
            clients
                .chathistory_targets_timestamps()
                .into_iter()
                .filter(|(timestamp_server, _)| timestamp_server == server),
        )
    }

    pub fn exit(
        &mut self,
        clients: &client::Map,
    ) -> Vec<BoxFuture<'static, Message>> {
        self.write_unwritten(clients.chathistory_targets_timestamps())
    }

    fn write_unwritten(
        &mut self,
        timestamps: impl IntoIterator<Item = (Server, DateTime<Utc>)>,
    ) -> Vec<BoxFuture<'static, Message>> {
        timestamps
            .into_iter()
            .filter_map(|(server, timestamp)| {
                (self.written.insert(server.clone(), timestamp)
                    != Some(timestamp))
                .then(|| write(server, timestamp))
            })
            .collect()
    }
}

fn write(
    server: Server,
    timestamp: DateTime<Utc>,
) -> BoxFuture<'static, Message> {
    async move {
        let result = client::overwrite_chathistory_targets_timestamp(
            server.clone(),
            timestamp,
        )
        .await;

        match result {
            Ok(()) => {
                log::debug!(
                    "updated targets timestamp for {server} to {timestamp}"
                );
            }
            Err(error) => {
                log::warn!(
                    "failed to update targets timestamp for {server} to {timestamp}: {error}"
                );
            }
        }

        Message::TargetsTimestampUpdated
    }
    .boxed()
}
