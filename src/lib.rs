pub mod auth;
pub mod constraint;
pub mod detect;
pub mod filelock;
pub mod git;
pub mod http;
pub mod manifest;
pub mod ops;
pub mod registry;
pub mod table;

use std::fmt;

/// Error with a human-readable message, ready to show to the user.
#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Prefixes an underlying error with context, e.g. `cannot read aip.json: permission denied`.
pub(crate) trait Context<T> {
    fn context(self, message: impl FnOnce() -> String) -> Result<T>;
}

impl<T, E: fmt::Display> Context<T> for std::result::Result<T, E> {
    fn context(self, message: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|err| Error(format!("{}: {err}", message())))
    }
}

/// 64-bit FNV-1a hash; stable across runs and platforms, for file names.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}
