use crate::record::Codec;

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
}
