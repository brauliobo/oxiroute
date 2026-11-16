#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CongestionControl {
    #[default]
    Cubic,
    Bbr,
}

impl CongestionControl {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cubic => "cubic",
            Self::Bbr => "bbr",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Http3Transport {
    pub congestion_control: CongestionControl,
    pub segmentation_offload: bool,
}

impl ConfigDraft {
    #[must_use]
    pub fn effective_http3_transport(&self) -> Http3Transport {
        Http3Transport {
            congestion_control: self.http3_congestion_control.unwrap_or_default(),
            segmentation_offload: self.http3_segmentation_offload.unwrap_or(true),
        }
    }
}
