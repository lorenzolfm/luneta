pub enum Fetch<T> {
    Waiting,
    Ready(T),
    Failed(String),
}

#[expect(
    clippy::derivable_impls,
    reason = "`Waiting` is the default by meaning, not by being written first"
)]
impl<T> Default for Fetch<T> {
    fn default() -> Self {
        Fetch::Waiting
    }
}
