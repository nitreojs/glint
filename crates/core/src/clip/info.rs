use crate::record::Codec;
use crate::record::clicks::Payload;
use crate::record::keys;

#[derive(Clone, Debug)]
pub struct Track {
    pub stream: u32,
    pub name: String,
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
}
