use std::io;

pub(crate) fn during(step: &'static str) -> impl FnOnce(io::Error) -> io::Error {
    move |error| io::Error::new(error.kind(), format!("{step}: {error}"))
}
