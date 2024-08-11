pub trait ResultExt<T> {
    fn log_error(&self, msg: &str) -> &Self;
    fn log_ok(self, msg: &str) -> Option<T>;
}

impl<T, E: std::fmt::Debug> ResultExt<T> for Result<T, E> {
    fn log_error(&self, msg: &str) -> &Self {
        if let Err(e) = self {
            log::error!("{msg}: {e:?}");
        }
        self
    }

    fn log_ok(self, msg: &str) -> Option<T> {
        match self {
            Ok(v) => Some(v),
            Err(e) => {
                log::error!("{msg}: {e:?}");
                None
            }
        }
    }
}
