pub mod clicks;
pub mod effort;
pub mod keys;
pub mod mix;
pub mod mp4;
pub mod negotiate;
pub mod pipeline;
pub mod pointer;
pub mod replay;
pub mod timeline;
pub mod tracks;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    /// The name users know it by.
    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
        }
    }

    /// Smallest region (even sizes) that records. H.264: Windows' software encoder's limit,
    /// which takes what the GPU's encoder refuses (NVIDIA's wants 146×50, measured on an RTX
    /// 5060 Ti), so it holds on any GPU. HEVC has no software encoder: NVIDIA's limit, measured
    /// on the same card; other GPUs may want more and say so when recording starts.
    pub fn min_size(self) -> (i32, i32) {
        match self {
            Codec::H264 => (34, 34),
            Codec::Hevc => (130, 34),
        }
    }
}
