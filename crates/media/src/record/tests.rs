use std::cell::RefCell;
use std::f32::consts::TAU;
use std::rc::Rc;

use glint_core::record::Codec;
use glint_core::record::mix::RATE;
use glint_core::record::pipeline::{Encoded, Output, Packet, PacketSink, Stream, VideoConfig};
use glint_core::record::replay::{Limits, ReplaySink};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::Interface;

use super::packets::{AacEncoder, HwVideoEncoder, Mp4Sink};
use crate::clip::media::probe;

#[derive(Default)]
struct Seen {
    streams: Vec<Stream>,
    packets: Vec<(usize, Packet)>,
}

/// Passes everything on to an [`Mp4Sink`], noting what went by (packets without their data).
struct Spy {
    inner: Mp4Sink,
    seen: Rc<RefCell<Seen>>,
}

impl PacketSink for Spy {
    fn add_stream(&mut self, stream: Stream) -> Result<usize, String> {
        self.seen.borrow_mut().streams.push(stream.clone());
        self.inner.add_stream(stream)
    }

    fn write(&mut self, stream: usize, packet: Packet) -> Result<(), String> {
        self.seen.borrow_mut().packets.push((stream, Packet { data: Vec::new(), ..packet.clone() }));
        self.inner.write(stream, packet)
    }

    fn finalize(&mut self) -> Result<(), String> {
        self.inner.finalize()
    }
}

struct Gpu {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    mgr: IMFDXGIDeviceManager,
}

fn gpu() -> Gpu {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap();
        let (mut device, mut ctx) = (None, None);
        let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            flags,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .unwrap();
        let (device, ctx) = (device.unwrap(), ctx.unwrap());
        let mt: ID3D11Multithread = ctx.cast().unwrap();
        let _ = mt.SetMultithreadProtected(true);
        let (mut token, mut mgr) = (0, None);
        MFCreateDXGIDeviceManager(&mut token, &mut mgr).unwrap();
        let mgr = mgr.unwrap();
        mgr.ResetDevice(&device, token).unwrap();
        Gpu { device, ctx, mgr }
    }
}

/// A `w`×`h` texture filled with `rgba`, as a sample the encoder takes.
fn frame(gpu: &Gpu, (w, h): (u32, u32), rgba: [f32; 4]) -> IMFSample {
    unsafe {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            ..Default::default()
        };
        let mut tex = None;
        gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)).unwrap();
        let tex = tex.unwrap();
        let mut rtv = None;
        gpu.device.CreateRenderTargetView(&tex, None, Some(&mut rtv)).unwrap();
        gpu.ctx.ClearRenderTargetView(&rtv.unwrap(), &rgba);
        let buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &tex, 0, false).unwrap();
        let len = buf.cast::<IMF2DBuffer>().unwrap().GetContiguousLength().unwrap();
        buf.SetCurrentLength(len).unwrap();
        let sample = MFCreateSample().unwrap();
        sample.AddBuffer(&buf).unwrap();
        sample
    }
}

/// `frames` of colour changes at 30 fps and a tone, through the hardware encoders into `sink`.
fn encode<S: PacketSink>(gpu: &Gpu, codec: Codec, keyframe_secs: f64, frames: i64, sink: S) {
    let config = VideoConfig { codec, width: 640, height: 360, fps: 30.0, qp: 23, effort: 50, keyframe_secs, bitrate: None };
    let audio = vec![("music".to_string(), AacEncoder::new().unwrap())];
    let mut out = Encoded::new(HwVideoEncoder::new(&gpu.mgr), &config, audio, sink).unwrap();
    let frame_hns = 10_000_000 / 30;
    let chunk = RATE as usize / 10;
    for i in 0..frames {
        let t = (i % 60) as f32 / 60.0;
        out.video(frame(gpu, (640, 360), [t, 1.0 - t, 0.5, 1.0]), i * frame_hns, frame_hns).unwrap();
        if i % 3 == 0 {
            let at = (i / 3) as usize * chunk;
            let pcm: Vec<i16> = (at..at + chunk)
                .flat_map(|n| {
                    let v = ((n as f32 / RATE as f32 * 440.0 * TAU).sin() * 8000.0) as i16;
                    [v, v]
                })
                .collect();
            out.audio(0, at as i64, &pcm).unwrap();
        }
    }
    out.finish().unwrap();
}

fn temp_mp4(what: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("glint-{what}-{}.mp4", std::process::id()))
}

/// A `w`×`h` frame of random pixels: nothing an encoder can squeeze.
fn noise(gpu: &Gpu, (w, h): (u32, u32), seed: u32) -> IMFSample {
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    let px: Vec<u32> = (0..w * h)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x | 0xFF00_0000
        })
        .collect();
    let sample = frame(gpu, (w, h), [0.0; 4]);
    unsafe {
        let buf = sample.GetBufferByIndex(0).unwrap();
        let dx: IMFDXGIBuffer = buf.cast().unwrap();
        let mut raw = std::ptr::null_mut();
        dx.GetResource(&ID3D11Texture2D::IID, &mut raw).unwrap();
        let tex = ID3D11Texture2D::from_raw(raw);
        gpu.ctx.UpdateSubresource(&tex, 0, None, px.as_ptr() as _, w * 4, 0);
    }
    sample
}

/// Counts what an encoder makes.
#[derive(Default)]
struct Bytes(Rc<RefCell<usize>>);

impl PacketSink for Bytes {
    fn add_stream(&mut self, _: Stream) -> Result<usize, String> {
        Ok(0)
    }

    fn write(&mut self, _: usize, packet: Packet) -> Result<(), String> {
        *self.0.borrow_mut() += packet.data.len();
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), String> {
        Ok(())
    }
}

#[test]
#[ignore = "needs a hardware video encoder"]
fn a_bitrate_target_bounds_the_size_of_even_pure_noise() {
    let gpu = gpu();
    let size = |codec, bitrate| {
        let config = VideoConfig { codec, width: 640, height: 360, fps: 30.0, qp: 18, effort: 50, keyframe_secs: 1.0, bitrate };
        let bytes = Rc::new(RefCell::new(0));
        let mut out = Encoded::new(HwVideoEncoder::new(&gpu.mgr), &config, Vec::<(String, AacEncoder)>::new(), Bytes(bytes.clone())).unwrap();
        let frame_hns = 10_000_000 / 30;
        for i in 0..90 {
            out.video(noise(&gpu, (640, 360), i as u32), i * frame_hns, frame_hns).unwrap();
        }
        out.finish().unwrap();
        drop(out);
        *bytes.borrow()
    };
    for codec in [Codec::H264, Codec::Hevc] {
        // 3 s at 2 Mbit/s is 750 KB
        let target = size(codec, Some(2_000_000));
        assert!((375_000..=900_000).contains(&target), "{codec:?} at 2 Mbit/s: {target} bytes");
        let free = size(codec, None);
        assert!(free > 4 * target, "{codec:?}: qp 18 on noise should be far bigger ({free} vs {target})");
    }
}

/// Two seconds through the packet path into `codec`.mp4.
fn record(codec: Codec) -> (Seen, std::path::PathBuf) {
    let gpu = gpu();
    let path = temp_mp4(&format!("packets-{codec:?}"));
    let seen = Rc::new(RefCell::new(Seen::default()));
    let spy = Spy { inner: Mp4Sink::create(&path).unwrap(), seen: seen.clone() };
    encode(&gpu, codec, 2.0, 60, spy);
    (Rc::into_inner(seen).unwrap().into_inner(), path)
}

fn check(codec: Codec) {
    let (spy, path) = record(codec);
    let info = probe(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!((info.size, info.codec), ((640, 360), Some(codec)));
    assert!((info.duration - 20_000_000).abs() < 1_000_000, "{} s", info.duration as f64 / 1e7);
    assert_eq!(info.tracks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["music"]);

    let Stream::Video { headers, .. } = &spy.streams[0] else { panic!("video first") };
    assert!(!headers.is_empty(), "parameter sets come with the stream");
    let Stream::Audio { headers, .. } = &spy.streams[1] else { panic!("then audio") };
    assert_eq!(headers.len(), 2, "an AudioSpecificConfig for AAC-LC");

    let video: Vec<&Packet> = spy.packets.iter().filter(|(s, _)| *s == 0).map(|(_, p)| p).collect();
    assert_eq!(video.len(), 60, "a packet per frame");
    assert!(video[0].key && video[0].pts == 0, "it opens on a keyframe at 0");
    assert!(video.iter().filter(|p| p.key).count() >= 1);
    assert!(video.windows(2).all(|w| w[1].pts > w[0].pts && w[1].dts == w[1].pts), "display order, no reordering");
    let audio = spy.packets.iter().filter(|(s, _)| *s == 1).count();
    assert!((90..=96).contains(&audio), "{audio} aac frames for 2 s");
}

#[test]
#[ignore = "needs a hardware video encoder"]
fn h264_packets_make_a_playable_mp4() {
    check(Codec::H264);
}

#[test]
#[ignore = "needs a hardware video encoder"]
fn hevc_packets_make_a_playable_mp4() {
    check(Codec::Hevc);
}

#[test]
#[ignore = "needs a hardware video encoder"]
fn the_replay_buffer_saves_its_last_stretch_as_an_mp4() {
    let gpu = gpu();
    let replay = ReplaySink::new(Limits { keep: 600_000_000, max_bytes: 64 << 20 });
    // 5 s at 30 fps, a keyframe every second
    encode(&gpu, Codec::H264, 1.0, 150, replay.clone());
    let clip = replay.lock().last(15_000_000).unwrap();
    assert!((clip.duration - 20_000_000).abs() < 100, "1.5 s back is past the keyframe at 4 s, so from 3 s");
    let path = temp_mp4("replay");
    clip.write(&mut Mp4Sink::create(&path).unwrap()).unwrap();
    let info = probe(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!((info.size, info.codec), ((640, 360), Some(Codec::H264)));
    assert!((info.duration - 20_000_000).abs() < 1_000_000, "{} s", info.duration as f64 / 1e7);
    assert_eq!(info.tracks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["music"]);
}
