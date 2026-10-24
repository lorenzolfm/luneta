//! The daemon and its CLI as a user runs them: the two binaries, and the
//! variables of the environment between them.

// `cfg(test)`, which this crate is built with anyway, so that clippy takes
// the helpers in `host` for test code, as it takes a `#[test]`, and lets them
// unwrap as clippy.toml lets the tests.
#[cfg(test)]
mod agents;
#[cfg(test)]
mod folder;
#[cfg(test)]
mod host;
#[cfg(test)]
mod sessions;
