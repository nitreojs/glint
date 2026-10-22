pub mod clicks;
pub mod mix;
pub mod mp4;
pub mod pipeline;
pub mod pointer;
pub mod timeline;
pub mod tracks;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    /// Smallest frame (even sizes) the NVIDIA encoder accepts, measured on an RTX 5060 Ti:
    /// anything smaller fails in SetInputMediaType with MF_E_INVALIDMEDIATYPE.
    pub fn min_size(self) -> (i32, i32) {
        match self {
            Codec::H264 => (34, 34),
            Codec::Hevc => (130, 34),
        }
    }
}
