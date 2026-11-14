use std::time::{Duration, Instant};

use lunetad::api::Client;

const TRUSTED: Duration = Duration::from_secs(3);

const HUNT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roster {
    pub session: String,
    pub clients: Option<Vec<Client>>,
}

enum Was<'a> {
    Absent,
    Unlisted,
    Listed(&'a [Client]),
}

fn was<'a>(rosters: &'a [Roster], session: &str) -> Was<'a> {
    let mut named = rosters.iter().filter(|roster| roster.session == session);
    match (named.next(), named.next()) {
        (None, _) => Was::Absent,
        (Some(Roster { clients: Some(clients), .. }), None) => Was::Listed(clients),
        (Some(_), _) => Was::Unlisted,
    }
}

fn roster<'a>(rosters: &'a [Roster], session: &str) -> Option<&'a [Client]> {
    match was(rosters, session) {
        Was::Listed(clients) => Some(clients),
        Was::Absent | Was::Unlisted => None,
    }
}

fn newcomers<'r>(seen: &[Roster], rosters: &'r [Roster]) -> Option<Vec<(&'r str, Client)>> {
    let mut came = Vec::new();
    for listed in rosters {
        let clients = listed.clients.as_deref().unwrap_or_default();
        let before = match was(seen, &listed.session) {
            Was::Absent => &[][..],
            Was::Listed(before) => before,
            Was::Unlisted if clients.is_empty() => continue,
            Was::Unlisted => return None,
        };
        let new = clients.iter().filter(|client| before.iter().all(|was| was.id != client.id));
        came.extend(new.map(|client| (listed.session.as_str(), *client)));
    }
    Some(came)
}

#[derive(Clone, Debug)]
struct Seen {
    rosters: Vec<Roster>,
    coming: Vec<(String, u32, Instant)>,
}

impl Seen {
    fn after(before: Option<&Seen>, own: &str, rosters: &[Roster], now: Instant) -> Seen {
        let Some(before) = before else {
            return Seen { rosters: rosters.to_vec(), coming: Vec::new() };
        };
        let mut coming = Vec::new();
        let mut kept = Vec::with_capacity(rosters.len());
        for listed in rosters {
            let Some(clients) = listed.clients.as_deref().filter(|_| listed.session != own) else {
                kept.push(listed.clone());
                continue;
            };
            let was = match was(&before.rosters, &listed.session) {
                Was::Absent => Some(&[][..]),
                Was::Listed(was) => Some(was),
                Was::Unlisted => None,
            };
            let mut settled = Vec::with_capacity(clients.len());
            for client in clients {
                let there = was.is_some_and(|was| was.iter().any(|it| it.id == client.id));
                let since = (before.coming.iter())
                    .find(|(session, id, _)| *session == listed.session && *id == client.id)
                    .map_or(now, |(.., since)| *since);
                if there || now >= since + TRUSTED {
                    settled.push(*client);
                } else {
                    coming.push((listed.session.clone(), client.id, since));
                }
            }
            let held = settled.len() < clients.len();
            let clients = (was.is_some() || !held).then_some(settled);
            kept.push(Roster { session: listed.session.clone(), clients });
        }
        Seen { rosters: kept, coming }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    pub session: String,
    pub number: u64,
    pub went: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Looked {
    pub number: u64,
    pub went: Option<u32>,
    pub clients: Vec<Client>,
    pub before: Option<Before>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Before {
    Listed(Vec<Client>),
    Made,
    Unknown,
}

impl Before {
    pub fn clients(&self) -> &[Client] {
        match self {
            Before::Listed(clients) => clients,
            Before::Made | Before::Unknown => &[],
        }
    }

    pub fn fresh(&self) -> bool {
        !matches!(self, Before::Listed(_))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Word {
    #[default]
    Daemon,
    Own { listed: Option<Vec<Client>>, until: Instant },
}

#[derive(Debug)]
struct Others {
    clients: Vec<Client>,
    by_zellij: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unplaced {
    Waiting,
    Unsure,
    Plugin,
}

#[derive(Debug)]
struct Left {
    session: String,
    id: u32,
    sure: bool,
    pane: Option<u32>,
    seen: Option<Seen>,
}

#[derive(Debug)]
struct Astray {
    left: String,
    seen: Option<Seen>,
    until: Instant,
}

#[derive(Debug, Default)]
pub struct OwnClient {
    session: Option<String>,
    id: Option<u32>,
    sure: bool,
    pane: Option<u32>,
    word: Word,
    before: Option<Others>,
    looked: Option<Vec<Client>>,
    going: Option<(String, Option<u32>)>,
    left: Option<Left>,
    asked: u64,
    since: u64,
    taken: u64,
    seen: Option<Seen>,
    astray: Option<Astray>,
    doubt: bool,
}

impl OwnClient {
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub fn pane(&self) -> Option<u32> {
        self.pane
    }

    pub fn place(&self) -> Result<(&str, u32), Unplaced> {
        let Some(session) = self.session.as_deref() else {
            return Err(Unplaced::Waiting);
        };
        match (self.pane, self.id, &self.looked) {
            (Some(pane), ..) => Ok((session, pane)),
            (None, Some(_), _) => Err(Unplaced::Plugin),
            (None, None, Some(_)) => Err(Unplaced::Unsure),
            (None, None, None) => Err(Unplaced::Waiting),
        }
    }

    pub fn came(&mut self, session: &str, listed: Option<&[Client]>, now: Instant) -> Look {
        let going = self.going.take().filter(|(to, _)| to == session);
        let went = going.as_ref().and_then(|(_, pane)| *pane);
        let left = self.left.take();
        let back = left.as_ref().filter(|left| left.session == session);
        self.session = Some(session.to_owned());
        self.id = back.map(|left| left.id);
        self.sure = back.is_some_and(|left| left.sure);
        self.pane = back.map_or(went, |left| left.pane);
        self.word = Word::Own { listed: listed.map(<[Client]>::to_vec), until: now + TRUSTED };
        self.before = match back {
            Some(_) => None,
            None => listed.map(|clients| Others { clients: clients.to_vec(), by_zellij: false }),
        };
        self.looked = None;
        self.since = self.asked + 1;
        self.seen = back.and_then(|left| left.seen.clone());
        self.astray = None;
        self.doubt = false;
        if going.is_some() {
            self.left = left;
        }
        self.look(went)
    }

    pub fn unfound(&mut self) {
        self.looked = Some(Vec::new());
    }

    pub fn anew(&mut self) {
        self.going = None;
        self.left = None;
        self.seen = None;
        self.astray = None;
        self.doubt = false;
    }

    pub fn astray(&self) -> Option<&str> {
        self.astray.as_ref().map(|astray| astray.left.as_str())
    }

    pub fn titled(&mut self) {
        self.astray = None;
    }

    pub fn went(
        &mut self,
        session: &str,
        pane: Option<u32>,
        listed: Option<&[Client]>,
        now: Instant,
    ) -> Option<Look> {
        if self.session.as_deref() != Some(session) {
            self.going = Some((session.to_owned(), pane));
            if let (Some(left), Some(id)) = (self.session.clone(), self.id) {
                let (sure, pane, seen) = (self.sure, self.pane, self.seen.clone());
                self.left = Some(Left { session: left, id, sure, pane, seen });
            }
            return None;
        }
        let pane = pane?;
        self.pane = Some(pane);
        self.word = Word::Own { listed: listed.map(<[Client]>::to_vec), until: now + TRUSTED };
        Some(self.look(Some(pane)))
    }

    pub fn heard(&mut self, rosters: &[Roster], now: Instant) -> Option<String> {
        if self.astray.is_some() {
            return self.hunt(rosters, now);
        }
        let session = self.session.as_deref()?;
        let listed = roster(rosters, session);
        if self.id.is_none() && self.before.is_none() {
            self.before =
                listed.map(|clients| Others { clients: clients.to_vec(), by_zellij: false });
            self.learn(None);
        }
        if let Word::Own { listed: then, until } = &self.word {
            if then.as_deref() == listed && now < *until {
                return None;
            }
            self.word = Word::Daemon;
        }
        let listed = listed?;
        match self.id {
            Some(id) => match listed.iter().find(|client| client.id == id) {
                Some(client) => {
                    self.pane = client.pane;
                    if let (true, Some(own)) = (self.sure, self.session.as_deref()) {
                        self.seen = Some(Seen::after(self.seen.as_ref(), own, rosters, now));
                        self.doubt = false;
                    }
                },
                None if self.sure && self.going.is_none() => {
                    let listed = Some(listed.to_vec());
                    self.word = Word::Own { listed, until: now + TRUSTED };
                    self.doubt = true;
                },
                None => {},
            },
            None => self.pane = None,
        }
        None
    }

    fn strayed(&mut self, now: Instant) {
        let Some(left) = self.session.take() else {
            return;
        };
        let seen = self.seen.take();
        self.astray = Some(Astray { left, seen, until: now + HUNT });
        self.id = None;
        self.sure = false;
        self.pane = None;
        self.word = Word::Daemon;
        self.before = None;
        self.looked = None;
        self.left = None;
        self.since = self.asked + 1;
        self.doubt = false;
    }

    fn hunt(&mut self, rosters: &[Roster], now: Instant) -> Option<String> {
        let astray = self.astray.as_mut()?;
        let seen = astray.seen.as_ref().filter(|_| now < astray.until)?;
        let (session, own) = match newcomers(&seen.rosters, rosters).as_deref() {
            Some(&[(session, own)]) => (session.to_owned(), own),
            Some([]) => return None,
            Some(_) | None => {
                astray.seen = None;
                return None;
            },
        };
        self.astray = None;
        self.session = Some(session.clone());
        self.id = Some(own.id);
        self.sure = true;
        self.pane = own.pane;
        self.seen = Some(Seen { rosters: rosters.to_vec(), coming: Vec::new() });
        Some(session)
    }

    pub fn looked(&mut self, looked: Looked, listed: Option<&[Client]>, now: Instant) -> bool {
        if looked.number < self.since || looked.number <= self.taken {
            return false;
        }
        self.taken = looked.number;
        let last = looked.number == self.asked;
        let Looked { went, clients, before, .. } = looked;
        if let Some(before) = before {
            self.left = None;
            let others = match before {
                Before::Listed(clients) => Some(clients),
                Before::Made => Some(Vec::new()),
                Before::Unknown => None,
            };
            if let Some(clients) = others {
                self.before = Some(Others { clients, by_zellij: true });
            }
        }
        self.word = Word::Own { listed: listed.map(<[Client]>::to_vec), until: now + TRUSTED };
        let on = |client: &Client| went.is_none_or(|pane| client.pane == Some(pane));
        let own = self.id.and_then(|id| clients.iter().find(|client| client.id == id));
        if let Some(own) = own.filter(|own| self.sure || on(own)) {
            self.pane = own.pane;
            return last && !on(own);
        }
        if own.is_none() && self.sure && self.going.is_none() {
            self.strayed(now);
            return false;
        }
        self.id = None;
        self.sure = false;
        self.looked = Some(clients);
        self.pane = went;
        self.learn(went);
        false
    }

    fn learn(&mut self, went: Option<u32>) {
        let (None, Some(looked)) = (self.id, &self.looked) else {
            return;
        };
        let before = self.before.as_ref();
        let new = |client: &&Client| {
            before.is_some_and(|before| before.clients.iter().all(|was| was.id != client.id))
        };
        let there = |client: &&Client| went.is_some_and(|pane| client.pane == Some(pane));
        let by_zellij = before.is_some_and(|before| before.by_zellij);
        let Some((own, sure)) = alone(looked.iter())
            .map(|own| (own, true))
            .or_else(|| alone(looked.iter().filter(new)).map(|own| (own, by_zellij)))
            .or_else(|| alone(looked.iter().filter(there)).map(|own| (own, false)))
        else {
            return;
        };
        self.id = Some(own.id);
        self.sure = sure;
        self.pane = own.pane;
        self.before = None;
        self.looked = None;
    }

    pub fn again(&mut self) -> Option<Look> {
        if self.session.is_none() || self.going.is_some() || !self.sure {
            return None;
        }
        Some(self.look(None))
    }

    pub fn doubted(&mut self) -> Option<Look> {
        if !std::mem::take(&mut self.doubt) {
            return None;
        }
        self.again()
    }

    fn look(&mut self, went: Option<u32>) -> Look {
        self.asked += 1;
        let session = self.session.clone().expect("luneta looks at the session its client is in");
        Look { session, number: self.asked, went }
    }
}

fn alone<'a>(mut clients: impl Iterator<Item = &'a Client>) -> Option<Client> {
    match (clients.next(), clients.next()) {
        (Some(client), None) => Some(*client),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(id: u32, pane: u32) -> Client {
        Client { id, pane: Some(pane) }
    }

    fn listed(session: &str, clients: &[Client]) -> Roster {
        Roster { session: session.to_owned(), clients: Some(clients.to_vec()) }
    }

    fn work(clients: &[Client]) -> Vec<Roster> {
        vec![listed("work", clients)]
    }

    fn answer(look: &Look, clients: &[Client]) -> Looked {
        Looked { number: look.number, went: look.went, clients: clients.to_vec(), before: None }
    }

    #[test]
    fn a_second_client_never_moves_the_mark() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3)]), None, now);
        assert_eq!(own.pane(), Some(3));
        own.heard(&work(&[client(1, 5), client(2, 5)]), now);
        assert_eq!(own.pane(), Some(5));
        own.heard(&work(&[client(1, 5), client(2, 7)]), now);
        assert_eq!(own.pane(), Some(5));
        own.heard(&work(&[client(1, 6), client(2, 7)]), now);
        assert_eq!(own.pane(), Some(6));
    }

    #[test]
    fn luneta_s_client_is_the_one_that_came_with_it() {
        let now = Instant::now();
        let other = [client(1, 3)];
        let by_zellij = Some(Before::Listed(other.to_vec()));
        let unknown = Some(Before::Unknown);
        let cases = [(Some(&other[..]), None), (None, by_zellij), (Some(&other[..]), unknown)];
        for (listed, before) in cases {
            let mut own = OwnClient::default();
            let look = own.came("work", listed, now);
            let both = [client(1, 3), client(2, 3)];
            own.looked(Looked { before: before.clone(), ..answer(&look, &both) }, listed, now);
            own.heard(&work(&[client(1, 4), client(2, 3)]), now);
            assert_eq!(own.pane(), Some(3), "{listed:?} {before:?}");
            own.heard(&work(&[client(1, 4), client(2, 6)]), now);
            assert_eq!(own.pane(), Some(6), "{listed:?} {before:?}");
        }
    }

    #[test]
    fn the_daemon_s_list_from_before_a_look_does_not_undo_it() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let stale = [client(1, 3)];
        let look = own.came("work", Some(&stale), now);
        own.looked(answer(&look, &[client(1, 5)]), Some(&stale), now);
        own.heard(&work(&stale), now);
        assert_eq!(own.pane(), Some(5));
        own.heard(&work(&[client(1, 4)]), now);
        assert_eq!(own.pane(), Some(4));
    }

    #[test]
    fn the_daemon_s_list_says_where_the_client_is_once_it_is_newer_than_a_look() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3)]), None, now);
        let listed = [client(1, 3)];
        own.heard(&work(&listed), now);
        let went = own.went("work", Some(5), Some(&listed), now).unwrap();
        own.looked(answer(&went, &[client(1, 5)]), Some(&listed), now);
        own.heard(&work(&listed), now + TRUSTED / 2);
        assert_eq!(own.pane(), Some(5));
        own.heard(&work(&listed), now + TRUSTED);
        assert_eq!(own.pane(), Some(3));
    }

    #[test]
    fn a_look_that_comes_back_late_says_nothing() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let came = own.came("work", None, now);
        let went = own.went("work", Some(4), None, now).unwrap();
        own.looked(answer(&went, &[client(1, 4)]), None, now);
        own.looked(answer(&came, &[client(1, 3)]), None, now);
        assert_eq!(own.pane(), Some(4));

        let went = own.went("work", Some(5), None, now).unwrap();
        own.came("play", None, now);
        own.looked(answer(&went, &[client(1, 5)]), None, now);
        assert_eq!(own.pane(), None);
    }

    #[test]
    fn clients_that_came_together_are_told_apart_by_a_go() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3), client(2, 4)]), None, now);
        assert_eq!(own.pane(), None);

        let failed = own.went("work", Some(4), None, now).unwrap();
        assert_eq!(own.pane(), Some(4));
        let both = [client(1, 3), client(2, 4)];
        own.looked(Looked { went: None, ..answer(&failed, &both) }, None, now);
        assert_eq!(own.pane(), None);

        let went = own.went("work", Some(3), None, now).unwrap();
        own.looked(answer(&went, &[client(1, 3), client(2, 4)]), None, now);
        own.heard(&work(&[client(1, 6), client(2, 4)]), now);
        assert_eq!(own.pane(), Some(6));
    }

    #[test]
    fn a_go_takes_back_an_id_that_was_another_s() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let stale = [client(1, 3)];
        let look = own.came("work", Some(&stale), now);
        own.looked(answer(&look, &[client(1, 3), client(3, 4)]), Some(&stale), now);
        assert_eq!(own.pane(), Some(4));
        let went = own.went("work", Some(5), Some(&stale), now).unwrap();
        own.looked(answer(&went, &[client(1, 5), client(3, 4)]), Some(&stale), now);
        own.heard(&work(&[client(1, 6), client(3, 4)]), now);
        assert_eq!(own.pane(), Some(6));
    }

    #[test]
    fn a_go_that_took_another_client_keeps_an_id_luneta_is_sure_of() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3)]), None, now);
        let listed = [client(1, 3), client(2, 4)];
        own.heard(&work(&listed), now);
        let went = own.went("work", Some(5), Some(&listed), now).unwrap();
        assert!(own.looked(answer(&went, &[client(1, 3), client(2, 5)]), Some(&listed), now));
        assert_eq!(own.pane(), Some(3));
        own.heard(&work(&[client(1, 3), client(2, 6)]), now);
        assert_eq!(own.pane(), Some(3));
        own.heard(&work(&[client(1, 7), client(2, 6)]), now);
        assert_eq!(own.pane(), Some(7));

        let went = own.went("work", Some(8), None, now).unwrap();
        let again = own.went("work", Some(9), None, now).unwrap();
        let there = [client(1, 9), client(2, 6)];
        assert!(!own.looked(answer(&went, &there), None, now));
        assert!(!own.looked(answer(&again, &there), None, now));
        assert_eq!(own.pane(), Some(9));
    }

    #[test]
    fn a_go_to_another_session_marks_its_pane_at_once() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        own.came("work", None, now);
        assert_eq!(own.went("play", Some(7), None, now), None);
        let look = own.came("play", Some(&[client(1, 2)]), now);
        assert_eq!((own.pane(), look.went), (Some(7), Some(7)));
    }

    #[test]
    fn luneta_says_why_it_does_not_know_the_pane() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        assert_eq!(own.place(), Err(Unplaced::Waiting));
        let look = own.came("work", None, now);
        assert_eq!(own.place(), Err(Unplaced::Waiting));
        own.looked(answer(&look, &[client(1, 3), client(2, 4)]), None, now);
        assert_eq!(own.place(), Err(Unplaced::Unsure));
        let went = own.went("work", Some(4), None, now).unwrap();
        assert_eq!(own.place(), Ok(("work", 4)));
        own.looked(answer(&went, &[client(1, 3), client(2, 4)]), None, now);
        assert_eq!(own.place(), Ok(("work", 4)));
        own.heard(&work(&[client(1, 3), Client { id: 2, pane: None }]), now + TRUSTED);
        assert_eq!(own.place(), Err(Unplaced::Plugin));
    }

    #[test]
    fn a_go_zellij_did_not_take_the_client_on_keeps_its_id() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let other = [client(1, 3)];
        let look = own.came("work", None, now);
        let both = [client(1, 3), client(2, 3)];
        let before = Some(Before::Listed(other.to_vec()));
        own.looked(Looked { before, ..answer(&look, &both) }, None, now);
        let listed = [client(1, 3), client(2, 4)];
        own.heard(&work(&listed), now);
        assert_eq!(own.went("play", Some(7), None, now), None);
        own.came("play", None, now);
        let look = own.came("work", Some(&listed), now);
        assert_eq!(own.pane(), Some(4));
        own.looked(answer(&look, &listed), Some(&listed), now);
        assert_eq!(own.pane(), Some(4));
        own.heard(&work(&[client(1, 6), client(2, 4)]), now);
        assert_eq!(own.pane(), Some(4));
    }

    fn sure_in_work(now: Instant) -> OwnClient {
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3)]), None, now);
        let rosters = [listed("work", &[client(1, 3)]), listed("play", &[client(1, 2)])];
        assert_eq!(own.heard(&rosters, now + TRUSTED), None);
        own
    }

    fn left(own: &mut OwnClient, rosters: &[Roster], now: Instant) -> Option<String> {
        assert_eq!(own.heard(rosters, now), None);
        assert_eq!(own.astray(), None);
        let look = own.doubted().expect("luneta asks zellij whether its client left");
        assert_eq!(own.doubted(), None);
        assert!(!own.looked(answer(&look, &[]), None, now));
        own.heard(rosters, now)
    }

    #[test]
    fn a_client_moved_by_another_is_the_one_that_came_to_any_session() {
        let now = Instant::now();
        let mut own = sure_in_work(now);
        let later = now + TRUSTED;
        let gone = [listed("work", &[]), listed("play", &[client(1, 2)])];
        assert_eq!(left(&mut own, &gone, later), None);
        assert_eq!((own.astray(), own.session()), (Some("work"), None));
        assert_eq!(own.place(), Err(Unplaced::Waiting));
        let came = [listed("work", &[]), listed("play", &[client(1, 2), client(2, 5)])];
        assert_eq!(own.heard(&came, later), Some("play".to_owned()));
        assert_eq!((own.astray(), own.place()), (None, Ok(("play", 5))));
        own.heard(&[listed("work", &[]), listed("play", &[client(1, 2), client(2, 6)])], now);
        assert_eq!(own.pane(), Some(6));
    }

    #[test]
    fn the_session_a_client_came_to_can_be_listed_before_the_one_it_left() {
        let now = Instant::now();
        let later = now + TRUSTED;
        let play = listed("play", &[client(1, 2), client(2, 5)]);
        let mut own = sure_in_work(now);
        assert_eq!(own.heard(&[listed("work", &[client(1, 3)]), play.clone()], later), None);
        let found = left(&mut own, &[listed("work", &[]), play.clone()], later + TRUSTED / 2);
        assert_eq!((found.as_deref(), own.place()), (Some("play"), Ok(("play", 5))));

        let mut own = sure_in_work(now);
        own.heard(&[listed("work", &[client(1, 3)]), play.clone()], later);
        own.heard(&[listed("work", &[client(1, 3)]), play.clone()], later + TRUSTED);
        assert_eq!(left(&mut own, &[listed("work", &[]), play], later + TRUSTED), None);
        assert_eq!(own.astray(), Some("work"));
    }

    #[test]
    fn a_daemon_list_older_than_the_look_does_not_set_luneta_looking() {
        let now = Instant::now();
        let mut own = sure_in_work(now);
        let stale = [listed("work", &[]), listed("play", &[client(1, 2)])];
        assert_eq!(own.heard(&stale, now + TRUSTED), None);
        let look = own.doubted().unwrap();
        assert!(!own.looked(answer(&look, &[client(1, 4)]), Some(&[][..]), now + TRUSTED));
        assert_eq!((own.astray(), own.place()), (None, Ok(("work", 4))));
        assert_eq!(own.heard(&stale, now + TRUSTED * 3 / 2), None);
        assert_eq!(own.doubted(), None);
    }

    #[test]
    fn a_look_that_does_not_find_the_client_sets_luneta_looking_for_it() {
        let now = Instant::now();
        let mut own = sure_in_work(now);
        let went = own.went("work", Some(4), None, now).unwrap();
        assert!(!own.looked(answer(&went, &[]), None, now));
        assert_eq!(own.astray(), Some("work"));
        let play = listed("play", &[client(1, 2)]);
        let made = [listed("work", &[]), play, listed("new", &[client(1, 0)])];
        assert_eq!(own.heard(&made, now), Some("new".to_owned()));
        assert_eq!(own.place(), Ok(("new", 0)));
    }

    #[test]
    fn luneta_looks_again_only_for_a_client_it_is_sure_of() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3), client(2, 4)]), None, now);
        assert_eq!(own.place(), Err(Unplaced::Unsure));
        assert_eq!(own.again(), None);
        assert_eq!(sure_in_work(now).again().map(|look| look.session), Some("work".to_owned()));
    }

    #[test]
    fn luneta_takes_no_client_for_its_own_where_it_cannot_tell() {
        let now = Instant::now();
        let mut own = sure_in_work(now);
        let play = listed("play", &[client(1, 2), client(2, 5), client(3, 5)]);
        assert_eq!(left(&mut own, &[listed("work", &[]), play], now + TRUSTED), None);
        let one = [listed("work", &[]), listed("play", &[client(1, 2), client(2, 5)])];
        assert_eq!(own.heard(&one, now + TRUSTED), None);
        assert_eq!(own.astray(), Some("work"));

        let mut own = sure_in_work(now);
        left(&mut own, &[listed("work", &[]), listed("play", &[client(1, 2)])], now + TRUSTED);
        let late = [listed("work", &[]), listed("play", &[client(1, 2), client(2, 5)])];
        assert_eq!(own.heard(&late, now + TRUSTED + HUNT), None);
        assert_eq!(own.astray(), Some("work"));
        own.titled();
        assert_eq!(own.astray(), None);
    }

    #[test]
    fn a_session_whose_clients_were_not_listed_hides_none() {
        let now = Instant::now();
        let unlisted = Roster { session: "play".to_owned(), clients: None };
        for play in [listed("play", &[client(1, 2)]), unlisted.clone()] {
            let mut own = OwnClient::default();
            let look = own.came("work", None, now);
            own.looked(answer(&look, &[client(1, 3)]), None, now);
            own.heard(&[listed("work", &[client(1, 3)]), unlisted.clone()], now + TRUSTED);
            left(&mut own, &[listed("work", &[]), play.clone()], now + TRUSTED);
            let came = [listed("work", &[]), play.clone(), listed("other", &[client(1, 4)])];
            let found = own.heard(&came, now + TRUSTED);
            match play.clients {
                Some(_) => assert_eq!((found, own.astray()), (None, Some("work"))),
                None => assert_eq!(own.place(), Ok(("other", 4))),
            }
        }
    }

    #[test]
    fn a_client_luneta_sends_elsewhere_is_not_astray() {
        let now = Instant::now();
        let mut own = sure_in_work(now);
        assert_eq!(own.went("play", None, None, now), None);
        own.heard(&[listed("work", &[]), listed("play", &[client(1, 2)])], now + TRUSTED);
        assert_eq!(own.doubted(), None);
        assert_eq!((own.astray(), own.session()), (None, Some("work")));
    }
}
