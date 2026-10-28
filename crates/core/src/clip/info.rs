use crate::record::Codec;
use crate::record::clicks::Payload;
use crate::record::keys;

#[derive(Clone, Debug)]
pub struct Track {
    pub stream: u32,
    pub name: String,
}

/// Why a copy of the video can't start on any frame, so it starts on the keyframe before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snap {
    /// Not h.264 or hevc as windows' encoders write them (8-bit 4:2:0, progressive, a profile they
    /// know), so nothing here can encode a start that fits the rest.
    Setup,
    /// Its frames are stored out of order (b-frames).
    Reordered,
    /// Windows can't decode or encode its codec (hevc needs the store's "hevc video extensions").
    Codec,
}

#[derive(Clone, Debug)]
pub struct Info {
    pub duration: i64,
    pub size: (u32, u32),
    pub fps: f64,
    pub codec: Option<Codec>,
    pub tracks: Vec<Track>,
    pub clicks: Option<Payload>,
    pub keys: Option<keys::Payload>,
    /// Why a cut can't start on its own frame; none when it can (a smart cut).
    pub snap: Option<Snap>,
}
