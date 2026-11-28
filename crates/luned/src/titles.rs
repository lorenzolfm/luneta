use lune_protocol::said::Said;

use crate::api;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Titled {
    pub topic: Option<Said>,
}

impl Titled {
    pub(crate) fn to_api(&self) -> api::Titled {
        api::Titled {
            topic: self.topic.clone().map(String::from),
        }
    }
}

const GENERIC_TITLE: &str = "Claude Code";

pub(crate) fn titled(title: &str) -> Option<Titled> {
    let mut chars = title.trim().chars();
    let glyph = chars.next()?;
    let agent = matches!(glyph, '✳' | '◐' | '◑' | '✢' | '✶' | '✻' | '✽' | '·')
        || ('\u{2800}'..='\u{28ff}').contains(&glyph);
    let topic = Some(chars.as_str().trim_start()).filter(|topic| *topic != GENERIC_TITLE);
    agent.then(|| Titled {
        topic: topic.map(str::to_owned).and_then(Said::new),
    })
}
