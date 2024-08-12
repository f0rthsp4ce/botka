pub trait ResultExt<T> {
    fn log_error(&self, module_path: &str, msg: &str) -> &Self;
    fn log_ok(self, module_path: &str, msg: &str) -> Option<T>;
}

impl<T, E: std::fmt::Debug> ResultExt<T> for Result<T, E> {
    fn log_error(&self, module_path: &str, msg: &str) -> &Self {
        if let Err(e) = self {
            log::error!(target: module_path, "{msg}: {e:?}");
        }
        self
    }

    fn log_ok(self, module_path: &str, msg: &str) -> Option<T> {
        match self {
            Ok(v) => Some(v),
            Err(e) => {
                log::error!(target: module_path, "{msg}: {e:?}");
                None
            }
        }
    }
}
