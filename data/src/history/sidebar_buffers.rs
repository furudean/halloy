use std::collections::{HashMap, HashSet};

use crate::target::{self, Target};
use crate::{Server, client, history};

#[derive(Debug, Default)]
struct Buffers {
    channels: HashSet<target::Channel>,
    queries: HashSet<target::Query>,
}

#[derive(Debug, Default)]
pub struct SidebarBuffers {
    buffers: HashMap<Server, Buffers>,
    disconnected: HashMap<Server, HashSet<target::Channel>>,
    closing: HashMap<Server, HashSet<target::Channel>>,
    removed: HashSet<Server>,
    remember_buffers: bool,
    changed: bool,
    dirty: bool,
    exited: bool,
}

impl SidebarBuffers {
    pub fn restore(&mut self, server: Server, target: Target) {
        let buffers = self.buffers.entry(server.clone()).or_default();

        match target {
            Target::Channel(channel) => {
                buffers.channels.insert(channel.clone());

                self.disconnected.entry(server).or_default().insert(channel);
            }
            Target::Query(query) => {
                buffers.queries.insert(query);
            }
        }
    }

    pub fn disconnected_channels(
        &self,
        server: &Server,
    ) -> impl Iterator<Item = &target::Channel> {
        self.disconnected.get(server).into_iter().flatten()
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn mark_channel_disconnected(
        &mut self,
        server: Server,
        channel: target::Channel,
    ) {
        self.dirty = true;

        // The PART of a closed channel completes the close.
        if let Some(channels) = self.closing.get_mut(&server)
            && channels.remove(&channel)
        {
            if channels.is_empty() {
                self.closing.remove(&server);
            }

            return;
        }

        self.disconnected.entry(server).or_default().insert(channel);
    }

    pub fn mark_channel_joined(
        &mut self,
        server: &Server,
        channel: &target::Channel,
    ) {
        self.dirty = true;
        self.unmark_disconnected(server, channel);
    }

    pub fn forget_closed_channel(
        &mut self,
        kind: &history::Kind,
        clients: &client::Map,
    ) {
        if let history::Kind::Channel(server, channel) = kind {
            self.dirty = true;
            self.unmark_disconnected(server, channel);

            if let Some(buffers) = self.buffers.get_mut(server) {
                self.changed |= buffers.channels.remove(channel);
            }

            // Only a joined channel has a PART in flight.
            if clients.contains_channel(server, channel) {
                self.closing
                    .entry(server.clone())
                    .or_default()
                    .insert(channel.clone());
            }
        }
    }

    pub fn mark_server_channels_disconnected(
        &mut self,
        server: &Server,
        clients: &client::Map,
    ) {
        self.dirty = true;

        let closing = self.closing.remove(server);

        let channels = clients
            .get_channels(server)
            .filter(|channel| {
                !closing
                    .as_ref()
                    .is_some_and(|closing| closing.contains(*channel))
            })
            .cloned()
            .collect::<Vec<_>>();

        if !channels.is_empty() {
            self.disconnected
                .entry(server.clone())
                .or_default()
                .extend(channels);
        }
    }

    pub fn forget_server(&mut self, server: &Server) {
        self.dirty = true;

        if self.buffers.remove(server).is_some() {
            self.changed = true;
        }

        self.disconnected.remove(server);
        self.closing.remove(server);
        self.removed.insert(server.clone());
    }

    pub fn forget_unlisted_bouncer_networks(
        &mut self,
        parent: &Server,
        listed: &[Server],
    ) {
        let unlisted = self
            .buffers
            .keys()
            .chain(self.disconnected.keys())
            .filter(|server| {
                server.parent().as_ref() == Some(parent)
                    && !listed.contains(server)
            })
            .cloned()
            .collect::<Vec<_>>();

        for server in unlisted {
            self.forget_server(&server);
        }
    }

    fn unmark_disconnected(
        &mut self,
        server: &Server,
        channel: &target::Channel,
    ) {
        if let Some(channels) = self.disconnected.get_mut(server) {
            channels.remove(channel);

            if channels.is_empty() {
                self.disconnected.remove(server);
            }
        }
    }

    // Returns true when the saved buffers changed.
    pub fn update_saved<'a>(
        &mut self,
        queries: impl Iterator<Item = (&'a Server, &'a target::Query)> + Clone,
        clients: &client::Map,
        remember_buffers: bool,
    ) -> bool {
        if self.exited {
            return false;
        }

        if remember_buffers != self.remember_buffers {
            self.remember_buffers = remember_buffers;
            self.dirty = true;
        }

        if !remember_buffers {
            let cleared = self.buffers.values().any(|buffers| {
                !buffers.channels.is_empty() || !buffers.queries.is_empty()
            });

            self.buffers.clear();
            self.changed = false;
            self.dirty = false;

            return cleared;
        }

        // A removed server that connects again has its queries saved again.
        if self
            .removed
            .iter()
            .any(|server| clients.client(server).is_some())
        {
            self.dirty = true;
        }

        let mut changed = std::mem::take(&mut self.changed);

        if !std::mem::take(&mut self.dirty) {
            return changed;
        }

        for server in clients.connected_servers() {
            self.removed.remove(server);

            let closing = self.closing.get(server);

            // Left channels stay saved, since their buffer stays open.
            for channel in clients.get_channels(server) {
                // Closed channels stay unsaved while their PART is in flight.
                if closing.is_some_and(|closing| closing.contains(channel)) {
                    continue;
                }

                if !self
                    .buffers
                    .get(server)
                    .is_some_and(|buffers| buffers.channels.contains(channel))
                {
                    self.buffers
                        .entry(server.clone())
                        .or_default()
                        .channels
                        .insert(channel.clone());
                    changed = true;
                }
            }
        }

        let removed = &self.removed;
        let queries =
            queries.filter(move |(server, _)| !removed.contains(*server));

        if !self.queries_unchanged(queries.clone()) {
            for buffers in self.buffers.values_mut() {
                buffers.queries.clear();
            }

            for (server, query) in queries {
                self.buffers
                    .entry(server.clone())
                    .or_default()
                    .queries
                    .insert(query.clone());
            }

            changed = true;
        }

        self.buffers.retain(|_, buffers| {
            !buffers.channels.is_empty() || !buffers.queries.is_empty()
        });

        changed
    }

    fn queries_unchanged<'a>(
        &self,
        mut queries: impl Iterator<Item = (&'a Server, &'a target::Query)>,
    ) -> bool {
        let mut count = 0;

        queries.all(|(server, query)| {
            count += 1;

            self.buffers
                .get(server)
                .is_some_and(|buffers| buffers.queries.contains(query))
        }) && count
            == self
                .buffers
                .values()
                .map(|buffers| buffers.queries.len())
                .sum::<usize>()
    }

    pub fn exit(&mut self) {
        self.exited = true;
    }

    pub fn to_vec(&self) -> Vec<(Server, Target)> {
        self.buffers
            .iter()
            .flat_map(|(server, buffers)| {
                buffers
                    .channels
                    .iter()
                    .cloned()
                    .map(Target::Channel)
                    .chain(buffers.queries.iter().cloned().map(Target::Query))
                    .map(|target| (server.clone(), target))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use futures::channel::mpsc;
    use irc::proto::{self, Command};

    use super::*;
    use crate::bouncer::BouncerNetwork;
    use crate::{config, isupport, message};

    fn server() -> Server {
        Server::from(Arc::<str>::from("test"))
    }

    fn channel(name: &str) -> target::Channel {
        target::Channel::parse(
            name,
            isupport::DEFAULT_CHANTYPES,
            &[],
            isupport::CaseMap::default(),
        )
        .unwrap()
    }

    fn connect(clients: &mut client::Map) {
        let (sender, _) = mpsc::channel(1);

        clients.ready(
            server(),
            client::Client::new(
                server(),
                Arc::new(config::Server {
                    nickname: "tester".to_string(),
                    ..Default::default()
                }),
                sender,
            ),
        );
    }

    fn clients() -> client::Map {
        let mut clients = client::Map::default();

        connect(&mut clients);

        clients
    }

    // Mirrors the dashboard, which marks left channels as the events arrive.
    fn receive(
        clients: &mut client::Map,
        buffers: &mut SidebarBuffers,
        nickname: &str,
        command: Command,
    ) {
        let events = clients
            .receive(
                &server(),
                message::Encoded(proto::Message {
                    tags: BTreeMap::default(),
                    source: Some(proto::Source::User(proto::User {
                        nickname: nickname.to_string(),
                        username: Some(nickname.to_string()),
                        hostname: Some("example.test".to_string()),
                    })),
                    command,
                }),
                &config::Config::default(),
            )
            .unwrap();

        for event in events {
            match event {
                client::Event::JoinedChannel(channel, _) => {
                    buffers.mark_channel_joined(&server(), &channel);
                }
                client::Event::LeftChannel(channel) => {
                    buffers.mark_channel_disconnected(server(), channel);
                }
                _ => {}
            }
        }
    }

    fn join(
        clients: &mut client::Map,
        buffers: &mut SidebarBuffers,
        name: &str,
    ) {
        receive(
            clients,
            buffers,
            "tester",
            Command::JOIN(name.to_string(), None),
        );
    }

    fn part(
        clients: &mut client::Map,
        buffers: &mut SidebarBuffers,
        name: &str,
    ) {
        receive(
            clients,
            buffers,
            "tester",
            Command::PART(name.to_string(), None),
        );
    }

    fn kick(
        clients: &mut client::Map,
        buffers: &mut SidebarBuffers,
        name: &str,
    ) {
        receive(
            clients,
            buffers,
            "alice",
            Command::KICK(name.to_string(), "tester".to_string(), None),
        );
    }

    // Mirrors the app, which marks the server disconnected before dropping
    // its client.
    fn disconnect(clients: &mut client::Map, buffers: &mut SidebarBuffers) {
        buffers.mark_server_channels_disconnected(&server(), clients);
        clients.disconnected(server(), false);
    }

    fn update(buffers: &mut SidebarBuffers, clients: &client::Map) -> bool {
        buffers.update_saved(std::iter::empty(), clients, true)
    }

    fn update_disabled(
        buffers: &mut SidebarBuffers,
        clients: &client::Map,
    ) -> bool {
        buffers.update_saved(std::iter::empty(), clients, false)
    }

    fn close(clients: &client::Map, buffers: &mut SidebarBuffers, name: &str) {
        buffers.forget_closed_channel(
            &history::Kind::Channel(server(), channel(name)),
            clients,
        );
    }

    fn is_saved(buffers: &SidebarBuffers, name: &str) -> bool {
        buffers
            .to_vec()
            .contains(&(server(), Target::Channel(channel(name))))
    }

    fn is_disconnected(buffers: &SidebarBuffers, name: &str) -> bool {
        buffers
            .disconnected_channels(&server())
            .any(|disconnected| *disconnected == channel(name))
    }

    #[test]
    fn kicked_channel_stays_saved_and_disconnected() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        assert!(update(&mut buffers, &clients));
        assert!(is_saved(&buffers, "#halloy"));
        assert!(!is_disconnected(&buffers, "#halloy"));

        kick(&mut clients, &mut buffers, "#halloy");
        assert!(is_disconnected(&buffers, "#halloy"));
        assert!(!update(&mut buffers, &clients));
        assert!(is_saved(&buffers, "#halloy"));
        assert!(is_disconnected(&buffers, "#halloy"));
    }

    #[test]
    fn rejoined_channel_is_no_longer_disconnected() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        kick(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        assert!(is_saved(&buffers, "#halloy"));
        assert!(!is_disconnected(&buffers, "#halloy"));

        // Parting again keeps the buffer open again.
        part(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        assert!(is_saved(&buffers, "#halloy"));
        assert!(is_disconnected(&buffers, "#halloy"));
    }

    #[test]
    fn left_channel_is_listed_once_after_reconnect() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        kick(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        disconnect(&mut clients, &mut buffers);
        update(&mut buffers, &clients);
        connect(&mut clients);
        update(&mut buffers, &clients);

        assert!(is_saved(&buffers, "#halloy"));
        assert!(is_disconnected(&buffers, "#halloy"));
    }

    #[test]
    fn joined_channels_stay_disconnected_with_server() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        join(&mut clients, &mut buffers, "#rust");
        update(&mut buffers, &clients);

        disconnect(&mut clients, &mut buffers);
        assert!(!update(&mut buffers, &clients));
        assert!(is_saved(&buffers, "#halloy"));
        assert!(is_saved(&buffers, "#rust"));
        assert!(is_disconnected(&buffers, "#halloy"));
        assert!(is_disconnected(&buffers, "#rust"));

        // Only channels joined again after reconnecting are connected.
        connect(&mut clients);
        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        assert!(is_saved(&buffers, "#halloy"));
        assert!(is_saved(&buffers, "#rust"));
        assert!(!is_disconnected(&buffers, "#halloy"));
        assert!(is_disconnected(&buffers, "#rust"));
    }

    #[test]
    fn closing_channel_is_not_disconnected_with_server() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        // The PART is in flight, so the channel is still joined.
        close(&clients, &mut buffers, "#halloy");
        disconnect(&mut clients, &mut buffers);
        update(&mut buffers, &clients);
        assert!(!is_saved(&buffers, "#halloy"));
        assert!(!is_disconnected(&buffers, "#halloy"));
    }

    #[test]
    fn joined_channels_stay_disconnected_when_remember_buffers_is_disabled() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update_disabled(&mut buffers, &clients);

        disconnect(&mut clients, &mut buffers);
        update_disabled(&mut buffers, &clients);
        assert!(!is_saved(&buffers, "#halloy"));
        assert!(is_disconnected(&buffers, "#halloy"));
    }

    #[test]
    fn part_for_untracked_channel_is_not_disconnected() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        part(&mut clients, &mut buffers, "#halloy");
        kick(&mut clients, &mut buffers, "#rust");
        update(&mut buffers, &clients);
        assert!(!is_saved(&buffers, "#halloy"));
        assert!(!is_disconnected(&buffers, "#halloy"));
        assert!(!is_disconnected(&buffers, "#rust"));
    }

    #[test]
    fn closed_channel_is_removed_when_part_arrives_before_update() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        close(&clients, &mut buffers, "#halloy");
        part(&mut clients, &mut buffers, "#halloy");
        assert!(update(&mut buffers, &clients));
        assert!(!is_saved(&buffers, "#halloy"));
        assert!(!is_disconnected(&buffers, "#halloy"));
    }

    #[test]
    fn closed_channel_is_removed_when_part_arrives_after_update() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        close(&clients, &mut buffers, "#halloy");

        // The PART is in flight, so the channel is still joined.
        assert!(update(&mut buffers, &clients));
        assert!(!is_saved(&buffers, "#halloy"));

        part(&mut clients, &mut buffers, "#halloy");
        assert!(!update(&mut buffers, &clients));
        assert!(!is_saved(&buffers, "#halloy"));
        assert!(!is_disconnected(&buffers, "#halloy"));
    }

    #[test]
    fn removed_server_channels_are_forgotten() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        join(&mut clients, &mut buffers, "#rust");
        update(&mut buffers, &clients);
        kick(&mut clients, &mut buffers, "#rust");

        // Mirrors the app, which drops the client before forgetting it.
        clients.remove(&server());
        buffers.forget_server(&server());
        assert!(update(&mut buffers, &clients));
        assert!(!is_saved(&buffers, "#halloy"));
        assert!(!is_saved(&buffers, "#rust"));
        assert!(!is_disconnected(&buffers, "#rust"));
    }

    #[test]
    fn removed_server_queries_are_forgotten() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();
        let query = target::Query::parse(
            "alice",
            isupport::DEFAULT_CHANTYPES,
            &[],
            isupport::CaseMap::default(),
        )
        .unwrap();
        let server = server();
        let queries = [(&server, &query)];

        assert!(buffers.update_saved(queries.into_iter(), &clients, true));
        assert!(
            buffers
                .to_vec()
                .contains(&(server.clone(), Target::Query(query.clone())))
        );

        // the query's history outlives server
        clients.remove(&server);
        buffers.forget_server(&server);
        assert!(buffers.update_saved(queries.into_iter(), &clients, true));
        assert!(buffers.to_vec().is_empty());

        // connecting again saves the query again
        connect(&mut clients);
        assert!(buffers.update_saved(queries.into_iter(), &clients, true));
        assert!(
            buffers
                .to_vec()
                .contains(&(server.clone(), Target::Query(query.clone())))
        );
    }

    #[test]
    fn unlisted_bouncer_networks_are_forgotten() {
        let mut buffers = SidebarBuffers::default();
        let network = |id: &str| Server {
            network: Some(Arc::new(BouncerNetwork {
                id: id.to_string(),
                name: id.to_string(),
            })),
            ..server()
        };

        buffers.restore(network("1"), Target::Channel(channel("#halloy")));
        buffers.restore(network("2"), Target::Channel(channel("#rust")));

        buffers.forget_unlisted_bouncer_networks(&server(), &[network("1")]);
        assert!(
            buffers
                .to_vec()
                .contains(&(network("1"), Target::Channel(channel("#halloy"))))
        );
        assert!(
            !buffers
                .to_vec()
                .contains(&(network("2"), Target::Channel(channel("#rust"))))
        );
        assert!(
            buffers
                .disconnected_channels(&network("2"))
                .next()
                .is_none()
        );
    }

    #[test]
    fn closed_channel_is_not_saved_on_exit_before_part_arrives() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        close(&clients, &mut buffers, "#halloy");

        // Mirrors the dashboard, which updates once more before exiting.
        update(&mut buffers, &clients);
        buffers.exit();
        assert!(!is_saved(&buffers, "#halloy"));
    }

    #[test]
    fn closed_channel_is_not_saved_when_server_disconnects_before_part_arrives()
    {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        close(&clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        disconnect(&mut clients, &mut buffers);
        update(&mut buffers, &clients);
        assert!(!is_saved(&buffers, "#halloy"));
        assert!(!is_disconnected(&buffers, "#halloy"));
    }

    #[test]
    fn closed_disconnected_channel_is_removed() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        kick(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);

        close(&clients, &mut buffers, "#halloy");
        assert!(update(&mut buffers, &clients));
        assert!(!is_saved(&buffers, "#halloy"));
        assert!(!is_disconnected(&buffers, "#halloy"));

        // A later join is not mistaken for the closed channel's PART.
        join(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        kick(&mut clients, &mut buffers, "#halloy");
        update(&mut buffers, &clients);
        assert!(is_saved(&buffers, "#halloy"));
        assert!(is_disconnected(&buffers, "#halloy"));
    }

    #[test]
    fn left_channels_stay_disconnected_when_remember_buffers_is_disabled() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        join(&mut clients, &mut buffers, "#rust");
        assert!(!update_disabled(&mut buffers, &clients));

        kick(&mut clients, &mut buffers, "#halloy");
        part(&mut clients, &mut buffers, "#rust");
        assert!(!update_disabled(&mut buffers, &clients));
        assert!(!is_saved(&buffers, "#halloy"));
        assert!(!is_saved(&buffers, "#rust"));
        assert!(is_disconnected(&buffers, "#halloy"));
        assert!(is_disconnected(&buffers, "#rust"));

        join(&mut clients, &mut buffers, "#halloy");
        update_disabled(&mut buffers, &clients);
        assert!(!is_disconnected(&buffers, "#halloy"));

        close(&clients, &mut buffers, "#rust");
        update_disabled(&mut buffers, &clients);
        assert!(!is_disconnected(&buffers, "#rust"));
    }

    #[test]
    fn closed_channel_is_not_kept_when_remember_buffers_is_disabled() {
        let mut clients = clients();
        let mut buffers = SidebarBuffers::default();

        join(&mut clients, &mut buffers, "#halloy");
        update_disabled(&mut buffers, &clients);

        close(&clients, &mut buffers, "#halloy");
        update_disabled(&mut buffers, &clients);
        part(&mut clients, &mut buffers, "#halloy");
        update_disabled(&mut buffers, &clients);
        assert!(!is_disconnected(&buffers, "#halloy"));
    }
}
