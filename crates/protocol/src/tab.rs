//! A tab's name, as zellij lists it, and the daemon's clients have it from
//! the daemon.

/// The name of a tab, if someone gave it one: zellij names a tab `Tab #N`
/// until then.
#[must_use]
pub fn named(tab: &str) -> Option<&str> {
    let number = tab.strip_prefix("Tab #");
    let unnamed = number.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    Some(tab).filter(|tab| !unnamed && !tab.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_zellij_numbered_has_no_name() {
        assert_eq!(named("Tab #4"), None);
        assert_eq!(named("  "), None);
        assert_eq!(named("Tab #"), Some("Tab #"));
        assert_eq!(named("Tab #4b"), Some("Tab #4b"));
        assert_eq!(named("notes"), Some("notes"));
    }
}
