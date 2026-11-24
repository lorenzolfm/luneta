pub fn alsa_errors() {
    #[cfg(not(target_os = "macos"))]
    let _ = alsa::Output::local_error_handler();
}
