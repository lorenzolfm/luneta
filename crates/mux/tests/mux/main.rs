//! The server as its clients reach it: the binary, started in a runtime
//! folder of the test's own, asked over gRPC, with `/bin/sh` in its panes.

#[cfg(test)]
mod clients;
#[cfg(test)]
mod host;
#[cfg(test)]
mod sessions;
