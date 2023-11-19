/// Appends formatted string to a `String`.
/// <https://github.com/rust-lang/rust-analyzer/blob/62268e474e9165de0cdb08d3794eec4b6ef1c6cd/crates/stdx/src/macros.rs#L13-L20>
macro_rules! format_to {
    ($buf:expr) => ();
    ($buf:expr, $lit:literal $($arg:tt)*) => {
        { use ::std::fmt::Write as _; let _ = ::std::write!($buf, $lit $($arg)*); }
    };
}
pub(crate) use format_to;
