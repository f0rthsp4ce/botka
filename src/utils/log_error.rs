pub trait ResultExt {
    fn log_error(&self, msg: &str) -> &Self;
}

impl<T, E: std::fmt::Debug> ResultExt for Result<T, E> {
    fn log_error(&self, msg: &str) -> &Self {
        if let Err(e) = self {
            log::error!("{msg}: {e:?}");
        }
        self
    }
}
