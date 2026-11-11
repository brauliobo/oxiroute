// OxiRoute: the vendored OpenSSL that `openssl-sys` builds must have kernel TLS (`enable-ktls`). Cargo unifies the
// features of build dependencies, so declaring `openssl-src` with `ktls` here is what turns it on for that build; the
// script itself has nothing to do.
fn main() {}
