use super::{AudioEncoder, Output, Packet, PacketSink, VideoConfig, VideoEncoder};

pub struct Encoded<V, A, S> {
    video: V,
    video_stream: usize,
    audio: Vec<(A, usize)>,
    sink: S,
    packets: Vec<Packet>,
}

impl<V: VideoEncoder, A: AudioEncoder, S: PacketSink> Encoded<V, A, S> {
    pub fn new(mut video: V, config: &VideoConfig, audio: Vec<(String, A)>, mut sink: S) -> Result<Self, String> {
        let video_stream = sink.add_stream(video.configure(config)?)?;
        let audio = audio
            .into_iter()
            .map(|(name, mut enc)| {
                let stream = sink.add_stream(enc.configure(&name)?)?;
                Ok((enc, stream))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Encoded { video, video_stream, audio, sink, packets: Vec::new() })
    }
}

fn flush(packets: &mut Vec<Packet>, sink: &mut impl PacketSink, stream: usize) -> Result<(), String> {
    for p in packets.drain(..) {
        sink.write(stream, p)?;
    }
    Ok(())
}

impl<V: VideoEncoder, A: AudioEncoder, S: PacketSink> Output for Encoded<V, A, S> {
    type Frame = V::Frame;

    fn video(&mut self, frame: V::Frame, time: i64, duration: i64) -> Result<(), String> {
        self.video.encode(frame, time, duration, &mut self.packets)?;
        flush(&mut self.packets, &mut self.sink, self.video_stream)
    }

    fn audio(&mut self, track: usize, at: i64, pcm: &[i16]) -> Result<(), String> {
        let (enc, stream) = &mut self.audio[track];
        enc.encode(at, pcm, &mut self.packets)?;
        flush(&mut self.packets, &mut self.sink, *stream)
    }

    fn finish(&mut self) -> Result<(), String> {
        self.video.finish(&mut self.packets)?;
        flush(&mut self.packets, &mut self.sink, self.video_stream)?;
        for (enc, stream) in &mut self.audio {
            enc.finish(&mut self.packets)?;
            flush(&mut self.packets, &mut self.sink, *stream)?;
        }
        self.sink.finalize()
    }
}
