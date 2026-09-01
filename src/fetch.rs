pub enum Fetch<T> {
    Waiting,
    Ready(T),
    Failed(String),
}

#[allow(clippy::derivable_impls)]
impl<T> Default for Fetch<T> {
    fn default() -> Self {
        Fetch::Waiting
    }
}
