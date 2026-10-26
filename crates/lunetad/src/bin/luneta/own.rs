//! Which client of its session is luneta's own, and so the pane you are in.
//!
//! zellij lists the clients of a session and the pane each has the focus on,
//! but says neither which is whose, nor, asked from outside, which pane one
//! client has the focus on. A client keeps the id zellij gave it as it came
//! to its session for as long as it stays there, though, and zellij gives one
//! that comes the lowest id no other client there has. So luneta follows its
//! own by its id, once it is sure of it. It is when zellij, asked just after
//! luneta's client came to the session, lists one client; or one that was
//! not listed before then; or, just after luneta sent its client to a pane,
//! one client alone on that pane. A second client, as a plain `zellij attach`
//! is, never moves the mark.
//!
//! The daemon lists the clients of every session every sweep, which can be
//! from before luneta's client came. So as it comes, luneta asks zellij
//! itself, and asks again after it sends the client to a pane: what zellij
//! answered stands until the daemon lists the clients other than it did when
//! luneta asked. Where luneta sends its client to a session itself, as it
//! starts or goes to a pane of another, it asks zellij for the session's
//! clients just before, and then until zellij lists one more, which is its
//! own. A move made in zellij, as by its keys, shows once the daemon lists it.

use lunetad::api::Client;

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
    /// What zellij listed of the session's clients just before luneta sent
    /// its client there from elsewhere, if it did: all of `clients` but
    /// luneta's own.
    pub before: Option<Vec<Client>>,
}

/// Whose word on the pane stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Word {
    /// The daemon's, as it lists the session's clients.
    #[default]
    Daemon,
    /// luneta's own, from a look or a go, until the daemon lists the clients
    /// other than as `listed`, as it did when luneta knew better.
    Own { listed: Option<Vec<Client>> },
}

/// luneta's own client of zellij: the session it is in, its id there once
/// luneta is sure of it, and the pane it has the focus on.
#[derive(Debug, Default)]
pub struct OwnClient {
    /// The session it is in, as the sidebar knows it.
    session: Option<String>,
    id: Option<u32>,
    pane: Option<u32>,
    word: Word,
    /// While luneta is not sure of its id: what zellij listed of the session's
    /// clients just before luneta sent its own there, or else what the daemon
    /// listed as luneta's came to it, or first after, if it had not listed
    /// them. Luneta's client is not among them, unless the daemon listed them
    /// after it came.
    before: Option<Vec<Client>>,
    /// While luneta is not sure of its id: what zellij answered the last look.
    looked: Option<Vec<Client>>,
    /// A session luneta sent its client to, from another, and the pane there.
    going: Option<(String, u32)>,
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

    /// luneta's client came to `session`, whose clients the daemon `listed`
    /// so. What it knew of the session the client was in goes, and zellij is
    /// asked for the clients of this one. Where luneta sent it to a pane here,
    /// that is where it is until zellij says.
    pub fn came(&mut self, session: &str, listed: Option<&[Client]>) -> Look {
        let went = self.going.take().filter(|(to, _)| to == session).map(|(_, pane)| pane);
        self.session = Some(session.to_owned());
        self.id = None;
        self.pane = went;
        self.word = Word::Own { listed: listed.map(<[Client]>::to_vec) };
        self.before = listed.map(<[Client]>::to_vec);
        self.looked = None;
        self.since = self.asked + 1;
        self.look(went)
    }

    /// luneta sends its client to `pane` of `session`, or to `session` alone,
    /// which the daemon `listed` the clients of so. In the session it is in,
    /// the client is on that pane until zellij says, which a look, once
    /// zellij has taken the client there, asks.
    pub fn went(
        &mut self,
        session: &str,
        pane: Option<u32>,
        listed: Option<&[Client]>,
    ) -> Option<Look> {
        if self.session.as_deref() != Some(session) {
            self.going = pane.map(|pane| (session.to_owned(), pane));
            return None;
        }
        let pane = pane?;
        self.pane = Some(pane);
        self.word = Word::Own { listed: listed.map(<[Client]>::to_vec) };
        Some(self.look(Some(pane)))
    }

    /// The daemon listed the clients of the session luneta's client is in
    /// so. Once they are not as they were when luneta last knew better, they
    /// say where its client is, if luneta is sure which it is.
    pub fn heard(&mut self, listed: Option<&[Client]>) {
        if self.session.is_none() {
            return;
        }
        if self.id.is_none() && self.before.is_none() {
            self.before = listed.map(<[Client]>::to_vec);
            self.learn(None);
        }
        if let Word::Own { listed: then } = &self.word {
            if then.as_deref() == listed {
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

    /// zellij answered a look so, while the daemon `listed` the clients so.
    /// One older than a look taken in already, or of a session the client has
    /// left, says nothing. An id luneta took for its own that zellij does not
    /// list, or not on the pane luneta's client went to, was another's.
    pub fn looked(&mut self, looked: Looked, listed: Option<&[Client]>) {
        if looked.number < self.since || looked.number <= self.taken {
            return;
        }
        self.taken = looked.number;
        let Looked { went, clients, before, .. } = looked;
        if before.is_some() {
            self.before = before;
        }
        self.word = Word::Own { listed: listed.map(<[Client]>::to_vec) };
        let on = |client: &Client| went.is_none_or(|pane| client.pane == Some(pane));
        let own = self.id.and_then(|id| clients.iter().find(|client| client.id == id));
        if let Some(own) = own.filter(|own| on(own)) {
            self.pane = own.pane;
            return;
        }
        self.id = None;
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
        let new = |client: &&Client| {
            self.before.as_ref().is_some_and(|before| before.iter().all(|was| was.id != client.id))
        };
        let there = |client: &&Client| went.is_some_and(|pane| client.pane == Some(pane));
        let Some(own) = alone(looked.iter())
            .or_else(|| alone(looked.iter().filter(new)))
            .or_else(|| alone(looked.iter().filter(there)))
        else {
            return;
        };
        self.id = Some(own.id);
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
        let mut own = OwnClient::default();
        let look = own.came("work", None);
        own.looked(answer(&look, &[client(1, 3)]), None);
        assert_eq!(own.pane(), Some(3));
        own.heard(Some(&[client(1, 5), client(2, 5)]));
        assert_eq!(own.pane(), Some(5));
        own.heard(Some(&[client(1, 5), client(2, 7)]));
        assert_eq!(own.pane(), Some(5));
        own.heard(Some(&[client(1, 6), client(2, 7)]));
        assert_eq!(own.pane(), Some(6));
    }

    /// luneta's client came to a session another was in already: it is the
    /// one the daemon did not list before, or that zellij did not list just
    /// before luneta sent its client there, as the daemon may not have yet.
    /// zellij put it on the pane the other has the focus on.
    #[test]
    fn luneta_s_client_is_the_one_that_came_with_it() {
        let other = [client(1, 3)];
        for (listed, before) in [(Some(&other[..]), None), (None, Some(other.to_vec()))] {
            let mut own = OwnClient::default();
            let look = own.came("work", listed);
            let both = [client(1, 3), client(2, 3)];
            own.looked(Looked { before, ..answer(&look, &both) }, listed);
            own.heard(Some(&[client(1, 4), client(2, 3)]));
            assert_eq!(own.pane(), Some(3), "{listed:?}");
            own.heard(Some(&[client(1, 4), client(2, 6)]));
            assert_eq!(own.pane(), Some(6), "{listed:?}");
        }
    }

    /// luneta's client left the session and came back before the daemon
    /// listed its clients again: the daemon lists it where it was, under the
    /// id zellij gave it again, until it lists them anew.
    #[test]
    fn the_daemon_s_list_from_before_a_look_does_not_undo_it() {
        let mut own = OwnClient::default();
        let stale = [client(1, 3)];
        let look = own.came("work", Some(&stale));
        own.looked(answer(&look, &[client(1, 5)]), Some(&stale));
        own.heard(Some(&stale));
        assert_eq!(own.pane(), Some(5));
        own.heard(Some(&[client(1, 4)]));
        assert_eq!(own.pane(), Some(4));
    }

    /// The look after a go comes back before the one asked as the client came
    /// to the session, and one of a session the client has left comes back
    /// once it is in another.
    #[test]
    fn a_look_that_comes_back_late_says_nothing() {
        let mut own = OwnClient::default();
        let came = own.came("work", None);
        let went = own.went("work", Some(4), None).unwrap();
        own.looked(answer(&went, &[client(1, 4)]), None);
        own.looked(answer(&came, &[client(1, 3)]), None);
        assert_eq!(own.pane(), Some(4));

        let went = own.went("work", Some(5), None).unwrap();
        own.came("play", None);
        own.looked(answer(&went, &[client(1, 5)]), None);
        assert_eq!(own.pane(), None);
    }

    /// Two clients came to the session as the daemon last listed none: the
    /// one alone on the pane luneta then sent its client to is luneta's. A go
    /// zellij did not take the client on tells nothing, and the pane it was
    /// sent to is not where it is.
    #[test]
    fn clients_that_came_together_are_told_apart_by_a_go() {
        let mut own = OwnClient::default();
        let look = own.came("work", None);
        own.looked(answer(&look, &[client(1, 3), client(2, 4)]), None);
        assert_eq!(own.pane(), None);

        let failed = own.went("work", Some(4), None).unwrap();
        assert_eq!(own.pane(), Some(4));
        own.looked(Looked { went: None, ..answer(&failed, &[client(1, 3), client(2, 4)]) }, None);
        assert_eq!(own.pane(), None);

        let went = own.went("work", Some(3), None).unwrap();
        own.looked(answer(&went, &[client(1, 3), client(2, 4)]), None);
        own.heard(Some(&[client(1, 6), client(2, 4)]));
        assert_eq!(own.pane(), Some(6));
    }

    /// The daemon's list from before luneta's client came missed a client
    /// that came just before it, which luneta took for its own: the client
    /// alone on the pane luneta sends its own to is luneta's.
    #[test]
    fn a_go_takes_back_an_id_that_was_another_s() {
        let mut own = OwnClient::default();
        let stale = [client(1, 3)];
        let look = own.came("work", Some(&stale));
        own.looked(answer(&look, &[client(1, 3), client(3, 4)]), Some(&stale));
        assert_eq!(own.pane(), Some(4));
        let went = own.went("work", Some(5), Some(&stale)).unwrap();
        own.looked(answer(&went, &[client(1, 5), client(3, 4)]), Some(&stale));
        own.heard(Some(&[client(1, 6), client(3, 4)]));
        assert_eq!(own.pane(), Some(6));
    }

    /// Sent to a pane of another session, luneta's client is there as soon as
    /// it comes to the session, before zellij says.
    #[test]
    fn a_go_to_another_session_marks_its_pane_at_once() {
        let mut own = OwnClient::default();
        own.came("work", None);
        assert_eq!(own.went("play", Some(7), None), None);
        let look = own.came("play", Some(&[client(1, 2)]));
        assert_eq!((own.pane(), look.went), (Some(7), Some(7)));
    }
}
