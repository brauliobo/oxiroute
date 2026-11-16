#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Coding {
    Gzip,
    Brotli,
    Zstd,
}

impl Coding {
    pub const ALL: [Self; 3] = [Self::Gzip, Self::Brotli, Self::Zstd];

    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Brotli => "br",
            Self::Zstd => "zstd",
        }
    }

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Gzip => 0,
            Self::Brotli => 1,
            Self::Zstd => 2,
        }
    }

    #[must_use]
    pub fn from_token(token: &[u8]) -> Option<Self> {
        match token.to_ascii_lowercase().as_slice() {
            b"gzip" | b"x-gzip" => Some(Self::Gzip),
            b"br" => Some(Self::Brotli),
            b"zstd" => Some(Self::Zstd),
            _ => None,
        }
    }
}
