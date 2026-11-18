use clap::ValueEnum;

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Binary {
    Oxiroute,
    Oxr,
}

impl Binary {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Oxiroute => "oxiroute",
            Self::Oxr => "oxr",
        }
    }
}
