//! Which client of its session is luneta's own, and so the pane you are in.
//!
//! zellij lists the clients of a session and the pane each has the focus on,
//! but says neither which is whose, nor, asked from outside, which pane one
//! client has the focus on. A client keeps the id zellij gave it as it came
//! to its session for as long as it stays there, though, and no other client
//! gets that id meanwhile. So luneta follows its own by its id, once it is
//! sure of it. It is when zellij, asked just after luneta's client came to
//! the session, lists one client; or one that was not listed before then; or,
//! just after luneta sent its client to a pane, one client alone on that pane.
//! A second client, as a plain `zellij attach` is, never moves the mark.
//!
//! The daemon lists the clients of every session every sweep, which can be
//! from before luneta's client came. So as it comes, luneta asks zellij
//! itself, and asks again after it sends the client to a pane: what zellij
//! answered stands until the daemon lists the clients other than it did when
//! luneta asked, or lists them still so once it has had the time to list
//! them again. Where luneta sends its client to a session itself, as it
//! starts or goes to a pane of another, it asks zellij for the session's
//! clients just before, and then until zellij lists one more, which is its
//! own. A move made in zellij, as by its keys, shows once the daemon lists it.

use std::time::{Duration, Instant};

use lunetad::api::Client;

/// How long what luneta saw of its client stands against a list of the
/// daemon's that is still the one it had then. The daemon asks zellij for
/// the clients of every session two seconds after it last asked, so that a
/// list it has after this is from after luneta looked: as when zellij's keys
/// take the client back where it was, or the pane it went to closes.
const TRUSTED: Duration = Duration::from_secs(3);

/// What luneta asks zellij: the clients of `session`, as they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    pub session: String,
    /// Counts the looks luneta asks for, by which one that comes back late is
    /// told from a newer one.
    pub number: u64,
    /// The pane luneta sent its client to just before, if it did.
    pub went: Option<u32>,
}

/// What zellij answered a look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Looked {
    pub number: u64,
    /// The pane luneta's client went to just before zellij was asked: the
    /// look's, unless zellij did not take it there.
    pub went: Option<u32>,
    pub clients: Vec<Client>,
    /// What luneta knew of the session's clients just before it sent its
    /// client there, from elsewhere or as it started, where the look was for
    /// the client's arrival: all of `clients` but luneta's own, as far as
    /// zellij said.
    pub before: Option<Before>,
}

/// What luneta knows of a session's clients before its own comes there, of
/// which none is luneta's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Before {
    /// zellij listed them: the session runs.
    Listed(Vec<Client>),
    /// No session ran by that name, as far as zellij's sockets said: zellij
    /// makes it, and luneta's client is its first. Its server cannot bear a call of
    /// zellij's CLI before it takes that client: every call asks the server
    /// of every session whether it runs, and zellij-server 0.45.1 panics
    /// where one hangs up before then (`lib.rs:1462`, fixed after 0.45.1 in
    /// zellij's 81f56e1), and the session ends with it.
    Made,
    /// zellij did not list them, as of a session whose server is gone or
    /// hangs: a client it lists after may be anyone's.
    Unknown,
}

impl Before {
    /// The clients that were there, as far as zellij said: none of a session
    /// it makes, nor of one it did not list.
    pub fn clients(&self) -> &[Client] {
        match self {
            Before::Listed(clients) => clients,
            Before::Made | Before::Unknown => &[],
        }
    }

    /// Whether the session's server may have yet to take its first client
    /// as luneta's comes: one zellij did not list the clients of.
    pub fn fresh(&self) -> bool {
        !matches!(self, Before::Listed(_))
    }
}

/// Whose word on the pane stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Word {
    /// The daemon's, as it lists the session's clients.
    #[default]
    Daemon,
    /// luneta's own, from a look or a go, until the daemon lists the clients
    /// other than as `listed`, as it did when luneta knew better, or still so
    /// from `until` on, by when it has listed them again.
    Own { listed: Option<Vec<Client>>, until: Instant },
}

/// What was listed of a session's clients before luneta's came to it: none
/// of them is luneta's.
#[derive(Debug)]
struct Others {
    clients: Vec<Client>,
    /// Whether zellij listed them, just before luneta sent its client there.
    /// The daemon's list can be older, and miss a client that came just
    /// before luneta's, which luneta would take for its own.
    by_zellij: bool,
}

/// Why luneta does not know the pane its client has the focus on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unplaced {
    /// luneta has yet to follow its client to a session, or zellij has yet
    /// to answer the look for it there.
    Waiting,
    /// zellij answered, and luneta cannot tell its client from the others of
    /// the session: until the daemon lists them again, or luneta sends it to
    /// a pane, which it is then on.
    Unsure,
    /// It has the focus on a plugin's pane, which zellij lists no terminal
    /// for.
    Plugin,
}

/// luneta's client, as it was in the session luneta sent it from to another:
/// where it still is, if zellij does not take it.
#[derive(Debug)]
struct Left {
    session: String,
    id: u32,
    sure: bool,
    pane: Option<u32>,
}

/// luneta's own client of zellij: the session it is in, its id there once
/// luneta is sure of it, and the pane it has the focus on.
#[derive(Debug, Default)]
pub struct OwnClient {
    /// The session it is in, as the sidebar knows it.
    session: Option<String>,
    id: Option<u32>,
    /// Whether zellij's word gave luneta `id`: the one client it listed, or
    /// the one it did not list just before luneta sent its client there. A go
    /// that finds that client on a pane other than the one luneta sent it to
    /// does not take the id back: zellij moved another client, or none. One
    /// luneta took from the daemon's list, or from a go, a go can.
    sure: bool,
    pane: Option<u32>,
    word: Word,
    /// While luneta is not sure of its id: what zellij listed of the session's
    /// clients just before luneta sent its own there, or else what the daemon
    /// listed as luneta's came to it, or first after, if it had not listed
    /// them. Luneta's client is not among them, unless the daemon listed them
    /// after it came.
    before: Option<Others>,
    /// While luneta is not sure of its id: what zellij answered the last look.
    looked: Option<Vec<Client>>,
    /// A session luneta sent its client to, from another, and the pane there,
    /// if it sent it to one.
    going: Option<(String, Option<u32>)>,
    /// Where luneta's client was as luneta sent it to another session, until
    /// zellij lists it there.
    left: Option<Left>,
    /// The number of the last look asked for.
    asked: u64,
    /// The number of the first look asked for in this session: an older one is
    /// of the session the client was in before.
    since: u64,
    /// The number of the newest look taken in.
    taken: u64,
}

impl OwnClient {
    /// The session luneta's client is in, as far as luneta has followed it.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// The pane luneta's client has the focus on, if luneta knows: none while
    /// it is on a plugin's pane.
    pub fn pane(&self) -> Option<u32> {
        self.pane
    }

    /// The session luneta's client is in and the pane it has the focus on
    /// there, while luneta knows both: the pane's id is that session's. Or
    /// else why luneta does not know the pane.
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

    /// luneta's client came to `session`, whose clients the daemon `listed`
    /// so, at `now`. What it knew of the session the client was in goes, and
    /// zellij is asked for the clients of this one. Where luneta sent it to a
    /// pane here, that is where it is until zellij says. Back in the session
    /// luneta sent it from, where zellij did not take it out of, it is as it
    /// was there, as the look checks.
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
        if going.is_some() {
            self.left = left;
        }
        self.look(went)
    }

    /// luneta sends its client to `pane` of `session`, or to `session` alone,
    /// which the daemon `listed` the clients of so, at `now`. In the session
    /// it is in, the client is on that pane until zellij says, which a look,
    /// once zellij has taken the client there, asks.
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
                self.left = Some(Left { session: left, id, sure: self.sure, pane: self.pane });
            }
            return None;
        }
        let pane = pane?;
        self.pane = Some(pane);
        self.word = Word::Own { listed: listed.map(<[Client]>::to_vec), until: now + TRUSTED };
        Some(self.look(Some(pane)))
    }

    /// The daemon lists the clients of the session luneta's client is in so,
    /// at `now`. Once they are not as they were when luneta last knew better,
    /// or are still so `TRUSTED` after, they say where its client is, if
    /// luneta is sure which it is.
    pub fn heard(&mut self, listed: Option<&[Client]>, now: Instant) {
        if self.session.is_none() {
            return;
        }
        if self.id.is_none() && self.before.is_none() {
            self.before =
                listed.map(|clients| Others { clients: clients.to_vec(), by_zellij: false });
            self.learn(None);
        }
        if let Word::Own { listed: then, until } = &self.word {
            if then.as_deref() == listed && now < *until {
                return;
            }
            self.word = Word::Daemon;
        }
        let Some(listed) = listed else {
            return;
        };
        match self.id {
            Some(id) => {
                if let Some(client) = listed.iter().find(|client| client.id == id) {
                    self.pane = client.pane;
                }
            },
            // The pane luneta sent it to, it may have left since.
            None => self.pane = None,
        }
    }

    /// zellij answered a look so, at `now`, while the daemon `listed` the
    /// clients so. One older than a look taken in already, or of a session
    /// the client has left, says nothing. An id luneta took for its own that
    /// zellij does not list, or, unless luneta is sure of it, not on the pane
    /// luneta's client went to, was another's.
    pub fn looked(&mut self, looked: Looked, listed: Option<&[Client]>, now: Instant) {
        if looked.number < self.since || looked.number <= self.taken {
            return;
        }
        self.taken = looked.number;
        let Looked { went, clients, before, .. } = looked;
        if let Some(before) = before {
            // luneta's client came here, and left where it was sent from.
            self.left = None;
            let others = match before {
                Before::Listed(clients) => Some(clients),
                Before::Made => Some(Vec::new()),
                // zellij did not say who was there: what the daemon listed
                // stands.
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
            return;
        }
        self.id = None;
        self.sure = false;
        self.looked = Some(clients);
        self.pane = went;
        self.learn(went);
    }

    /// Takes the one client that can be luneta's for its own, unless luneta
    /// is sure of it already: of what zellij answered the last look, the one
    /// it listed, or else the one the daemon did not list before luneta's
    /// client came to the session, or else the one alone on the pane luneta's
    /// client `went` to. The pane is that client's, as zellij answered.
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

    fn look(&mut self, went: Option<u32>) -> Look {
        self.asked += 1;
        let session = self.session.clone().expect("luneta looks at the session its client is in");
        Look { session, number: self.asked, went }
    }
}

/// The one client of `clients`, if there is exactly one.
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

    /// What zellij answers `look`, having taken luneta's client where the look
    /// says it went.
    fn answer(look: &Look, clients: &[Client]) -> Looked {
        Looked { number: look.number, went: look.went, clients: clients.to_vec(), before: None }
    }

    /// luneta's client came to the session alone, and so is the one zellij
    /// lists. A plain `zellij attach` that comes after it, and goes elsewhere,
    /// takes the mark nowhere.
    #[test]
    fn a_second_client_never_moves_the_mark() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3)]), None, now);
        assert_eq!(own.pane(), Some(3));
        own.heard(Some(&[client(1, 5), client(2, 5)]), now);
        assert_eq!(own.pane(), Some(5));
        own.heard(Some(&[client(1, 5), client(2, 7)]), now);
        assert_eq!(own.pane(), Some(5));
        own.heard(Some(&[client(1, 6), client(2, 7)]), now);
        assert_eq!(own.pane(), Some(6));
    }

    /// luneta's client came to a session another was in already: it is the
    /// one the daemon did not list before, or that zellij did not list just
    /// before luneta sent its client there, as the daemon may not have yet.
    /// Where zellij could not list them, what the daemon listed stands.
    /// zellij put it on the pane the other has the focus on.
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
            own.heard(Some(&[client(1, 4), client(2, 3)]), now);
            assert_eq!(own.pane(), Some(3), "{listed:?} {before:?}");
            own.heard(Some(&[client(1, 4), client(2, 6)]), now);
            assert_eq!(own.pane(), Some(6), "{listed:?} {before:?}");
        }
    }

    /// luneta's client left the session and came back before the daemon
    /// listed its clients again: the daemon lists it where it was, under the
    /// id zellij gave it again, until it lists them anew.
    #[test]
    fn the_daemon_s_list_from_before_a_look_does_not_undo_it() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let stale = [client(1, 3)];
        let look = own.came("work", Some(&stale), now);
        own.looked(answer(&look, &[client(1, 5)]), Some(&stale), now);
        own.heard(Some(&stale), now);
        assert_eq!(own.pane(), Some(5));
        own.heard(Some(&[client(1, 4)]), now);
        assert_eq!(own.pane(), Some(4));
    }

    /// The sidebar sends luneta's client to a pane, and zellij's keys take it
    /// back where it was before the daemon lists the clients again. The
    /// daemon then lists them as it did before the go, which, once it has had
    /// the time to list them anew, says where the client is.
    #[test]
    fn the_daemon_s_list_says_where_the_client_is_once_it_is_newer_than_a_look() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3)]), None, now);
        let listed = [client(1, 3)];
        own.heard(Some(&listed), now);
        let went = own.went("work", Some(5), Some(&listed), now).unwrap();
        own.looked(answer(&went, &[client(1, 5)]), Some(&listed), now);
        own.heard(Some(&listed), now + TRUSTED / 2);
        assert_eq!(own.pane(), Some(5));
        own.heard(Some(&listed), now + TRUSTED);
        assert_eq!(own.pane(), Some(3));
    }

    /// The look after a go comes back before the one asked as the client came
    /// to the session, and one of a session the client has left comes back
    /// once it is in another.
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

    /// Two clients came to the session as the daemon last listed none: the
    /// one alone on the pane luneta then sent its client to is luneta's. A go
    /// zellij did not take the client on tells nothing, and the pane it was
    /// sent to is not where it is.
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
        own.heard(Some(&[client(1, 6), client(2, 4)]), now);
        assert_eq!(own.pane(), Some(6));
    }

    /// The daemon's list from before luneta's client came missed a client
    /// that came just before it, which luneta took for its own: the client
    /// alone on the pane luneta sends its own to is luneta's.
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
        own.heard(Some(&[client(1, 6), client(3, 4)]), now);
        assert_eq!(own.pane(), Some(6));
    }

    /// luneta is sure of its client, which zellij listed alone. A go that
    /// zellij takes another client on, as one that typed in it last, leaves
    /// luneta's client where it is, and the other never moves the mark.
    #[test]
    fn a_go_that_took_another_client_keeps_an_id_luneta_is_sure_of() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        let look = own.came("work", None, now);
        own.looked(answer(&look, &[client(1, 3)]), None, now);
        let listed = [client(1, 3), client(2, 4)];
        own.heard(Some(&listed), now);
        let went = own.went("work", Some(5), Some(&listed), now).unwrap();
        own.looked(answer(&went, &[client(1, 3), client(2, 5)]), Some(&listed), now);
        assert_eq!(own.pane(), Some(3));
        own.heard(Some(&[client(1, 3), client(2, 6)]), now);
        assert_eq!(own.pane(), Some(3));
        own.heard(Some(&[client(1, 7), client(2, 6)]), now);
        assert_eq!(own.pane(), Some(7));
    }

    /// Sent to a pane of another session, luneta's client is there as soon as
    /// it comes to the session, before zellij says.
    #[test]
    fn a_go_to_another_session_marks_its_pane_at_once() {
        let now = Instant::now();
        let mut own = OwnClient::default();
        own.came("work", None, now);
        assert_eq!(own.went("play", Some(7), None, now), None);
        let look = own.came("play", Some(&[client(1, 2)]), now);
        assert_eq!((own.pane(), look.went), (Some(7), Some(7)));
    }

    /// luneta says why it does not know the pane its client is on: it waits
    /// on zellij, or cannot tell its client from another that came with it
    /// until it sends it to a pane, or the client is on a plugin's pane.
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
        own.heard(Some(&[client(1, 3), Client { id: 2, pane: None }]), now + TRUSTED);
        assert_eq!(own.place(), Err(Unplaced::Plugin));
    }

    /// luneta knows its client among two, and sends it to another session,
    /// which zellij does not take it to. Back in the session it never left,
    /// it is the client it was, which the look finds where it was.
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
        own.heard(Some(&listed), now);
        assert_eq!(own.went("play", Some(7), None, now), None);
        own.came("play", None, now);
        let look = own.came("work", Some(&listed), now);
        assert_eq!(own.pane(), Some(4));
        own.looked(answer(&look, &listed), Some(&listed), now);
        assert_eq!(own.pane(), Some(4));
        own.heard(Some(&[client(1, 6), client(2, 4)]), now);
        assert_eq!(own.pane(), Some(4));
    }
}
