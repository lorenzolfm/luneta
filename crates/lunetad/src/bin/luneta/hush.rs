//! What alsa-lib says as it fails to find the speakers or the microphone. It
//! writes that to stderr, which is the terminal luneta draws on: sixteen lines
//! over its frame each time, on a machine with no sound card, and an agent can
//! have it look for the speakers at any time. cpal returns as much of it as
//! luneta needs, as an error.

/// Has alsa-lib keep to itself what goes wrong on this thread, from here on,
/// which reaches the opening of a stream, but not the thread cpal plays it on.
/// It keeps it in a buffer, which the next call on this thread lets go of, so
/// that a thread that calls this before each opening keeps what one said.
pub fn alsa_errors() {
    // One that makes no buffer leaves alsa-lib writing to stderr, as it was.
    #[cfg(not(target_os = "macos"))]
    let _ = alsa::Output::local_error_handler();
}
