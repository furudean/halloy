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
    read: HashMap<Server, DateTime<Utc>>,
    written: HashMap<Server, DateTime<Utc>>,
    refreshed_at: Instant,
}

impl Default for TargetsTimestamps {
    fn default() -> Self {
        Self {
            read: HashMap::new(),
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

        let read = HashMap::from_iter(clients.chathistory_targets_timestamps());

        let mut tasks = vec![];

        for (server, timestamp) in std::mem::replace(&mut self.read, read) {
            if self.written.get(&server) == Some(&timestamp) {
                continue;
            }

            self.written.insert(server.clone(), timestamp);

            tasks.push(
                async move {
                    let result =
                        client::overwrite_chathistory_targets_timestamp(
                            server.clone(),
                            timestamp,
                        )
                        .await;

                    Message::TargetsTimestampUpdated(server, timestamp, result)
                }
                .boxed(),
            );
        }

        tasks
    }

    pub fn exit(
        &mut self,
        clients: &client::Map,
    ) -> Vec<(Server, DateTime<Utc>)> {
        clients
            .chathistory_targets_timestamps()
            .into_iter()
            .filter(|(server, timestamp)| {
                self.written.get(server) != Some(timestamp)
            })
            .collect()
    }
}
