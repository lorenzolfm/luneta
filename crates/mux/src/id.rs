//! The ids of sessions, tabs, panes and clients, each of a type of its own:
//! counted up from 1, never given again, and read from the protocol where a 0
//! is none given.

use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};

use tonic::Status;

use crate::asked::refused;

macro_rules! id {
    ($id:ident($nonzero:ty, $raw:ty)) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $id($nonzero);

        impl $id {
            /// The id `field` holds, which must be there.
            pub fn parse(field: &str, raw: $raw) -> Result<$id, Status> {
                <$nonzero>::new(raw).map($id).ok_or_else(|| refused(field, "is missing"))
            }

            /// The id after `last`, the last given, which it now is.
            pub fn next(last: &mut Option<$id>) -> $id {
                let next = match *last {
                    None => <$nonzero>::MIN,
                    Some($id(last)) => {
                        last.checked_add(1).expect(concat!(stringify!($id), "s last"))
                    },
                };
                *last = Some($id(next));
                $id(next)
            }

            #[must_use]
            pub fn said(self) -> $raw {
                self.0.get()
            }
        }

        impl fmt::Display for $id {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id!(SessionId(NonZeroU64, u64));
id!(TabId(NonZeroU32, u32));
id!(PaneId(NonZeroU32, u32));
id!(ClientId(NonZeroU64, u64));

#[cfg(test)]
mod tests {
    use super::*;

    /// No id is 0, which the protocol sends where none was given, and a
    /// refusal of it names the field.
    #[test]
    fn an_id_is_never_0() {
        assert_eq!(TabId::parse("tab", 7).unwrap().said(), 7);
        assert_eq!(SessionId::parse("session", 0).unwrap_err().message(), "session is missing");
    }

    /// Ids count up from 1.
    #[test]
    fn ids_count_up_from_1() {
        let mut last = None;
        assert_eq!(PaneId::next(&mut last).said(), 1);
        assert_eq!(PaneId::next(&mut last).said(), 2);
        assert_eq!(last.map(PaneId::said), Some(2));
    }
}
