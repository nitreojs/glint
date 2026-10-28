use std::cell::RefCell;
use std::f32::consts::TAU;
use std::rc::Rc;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use glint_core::geom::rect::Rect;
use glint_core::record::Codec;
use glint_core::record::mix::RATE;
use glint_core::record::negotiate::{Engine, Force, Input, Route};
use glint_core::record::pipeline::{
    Backend, CaptureStats, Encoded, Event, Frames, Outcome, Output, Packet, PacketSink, Parts, Recorder, Role, Stream, Summary, VideoConfig,
    open_audio,
};
use glint_core::record::pointer::{PointerKind, PointerShape};
use glint_core::record::replay::{Limits, ReplaySink};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIKeyedMutex, IDXGIResource};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::Interface;

use super::Params;
use super::encoder::{Composer, sink_writer, video_config};
use super::gpu::Gpu;
use super::packets::{AacEncoder, MfVideoEncoder, Mp4Sink};
use super::win::{INFINITE, RING, adapter, create_device, hns, qpc, texture_desc};
use crate::clip::media::{probe, still};
use crate::rect::WinRect;

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

fn gpu() -> Gpu {
    gpu_sized((640, 360))
}

/// The first adapter's encoder side, for `size` frames.
fn gpu_sized(size: (u32, u32)) -> Gpu {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap();
        Gpu::open(&adapter(0).unwrap(), size).unwrap()
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
    let mut out = Encoded::new(MfVideoEncoder::new(gpu, Force::default()), &config, audio, sink).unwrap();
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
        let mut out = Encoded::new(MfVideoEncoder::new(&gpu, Force::default()), &config, Vec::<(String, AacEncoder)>::new(), Bytes(bytes.clone())).unwrap();
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

// The routes to the encoder, through the real compositor and outputs: a synthetic capture ring
// stands in for Desktop Duplication.

/// Eight patches (sRGB), in four columns and two rows.
const PATCHES: [[u8; 3]; 8] = [
    [255, 0, 0],
    [0, 255, 0],
    [0, 0, 255],
    [255, 255, 255],
    [0, 0, 0],
    [128, 128, 128],
    [230, 120, 40],
    [30, 160, 170],
];

/// The capture side of the ring: one shared texture holding the patches.
struct Ring {
    _device: ID3D11Device,
    km: IDXGIKeyedMutex,
    handle: HANDLE,
}

impl Ring {
    fn new((w, h): (u32, u32)) -> Ring {
        unsafe {
            let (device, ctx) = create_device(&adapter(0).unwrap(), D3D11_CREATE_DEVICE_FLAG(0)).unwrap();
            let r = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
            let desc = texture_desc(r, D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX);
            let mut tex = None;
            device.CreateTexture2D(&desc, None, Some(&mut tex)).unwrap();
            let tex = tex.unwrap();
            let px: Vec<u32> = (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let [r, g, b] = PATCHES[(y * 2 / h * 4 + x * 4 / w) as usize];
                    0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32
                })
                .collect();
            let km: IDXGIKeyedMutex = tex.cast().unwrap();
            km.AcquireSync(0, INFINITE).unwrap();
            ctx.UpdateSubresource(&tex, 0, None, px.as_ptr() as _, w * 4, 0);
            ctx.Flush();
            km.ReleaseSync(0).unwrap();
            let handle = tex.cast::<IDXGIResource>().unwrap().GetSharedHandle().unwrap();
            Ring { _device: device, km, handle }
        }
    }

    /// Hands the image to the compositor again, as capture does with a fresh one.
    fn publish(&self) {
        unsafe {
            self.km.AcquireSync(0, INFINITE).unwrap();
            self.km.ReleaseSync(1).unwrap();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Via {
    SinkWriter,
    Packets,
}

fn params(codec: Codec, (w, h): (u32, u32), path: &Path) -> Params {
    Params {
        sources: Vec::new(),
        region: Rect { x: 0, y: 0, w: w as i32, h: h as i32 },
        fps: 30.0,
        codec,
        qp: 18,
        effort: 50,
        keyframe_secs: 1.0,
        bitrate: None,
        draw_cursor: true,
        clicks: None,
        keys: None,
        audio: Vec::new(),
        track_names: Vec::new(),
        path: path.to_path_buf(),
    }
}

/// A 32×32 opaque magenta pointer.
fn magenta_pointer() -> PointerShape {
    let data = [0xFF, 0x00, 0xFF, 0xFF].repeat(32 * 32);
    PointerShape { kind: PointerKind::Color, width: 32, height: 32, pitch: 32 * 4, data }
}

/// Where the test pointer is drawn.
const POINTER_AT: (i32, i32) = (40, 40);

struct Recorded {
    route: Route,
    notice: Option<&'static str>,
    path: PathBuf,
    fps: f64,
}

/// `frames` of the patches (and with `pointer`, the pointer at [`POINTER_AT`]) through `which`
/// output with the routes `force` leaves, as fast as the encoder takes them.
fn record_route(which: Via, codec: Codec, force: Force, size: (u32, u32), frames: i64, pointer: bool) -> Result<Recorded, String> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let path = temp_mp4(&format!("route-{}", N.fetch_add(1, Ordering::Relaxed)));
    let p = params(codec, size, &path);
    let gpu = gpu_sized(size);
    let ring = Ring::new(size);
    let frame_hns = 10_000_000 / 30;
    let run = |frames_: &mut Composer, out: &mut dyn FnMut(IMFSample, i64) -> Result<(), String>| -> Result<(), String> {
        frames_.set_pointer(&magenta_pointer());
        for i in 0..frames {
            ring.publish();
            frames_.latch(&0)?;
            let sample = loop {
                match frames_.alloc()? {
                    Some(s) => break s,
                    None => std::thread::sleep(std::time::Duration::from_millis(1)),
                }
            };
            frames_.compose(&sample, pointer.then_some(POINTER_AT), &[], None)?;
            out(sample, i * frame_hns)?;
        }
        Ok(())
    };
    let fps = |start: Instant| frames as f64 / start.elapsed().as_secs_f64();
    let (route, notice, fps) = unsafe {
        match which {
            Via::SinkWriter => {
                let (mut out, route, notice, _) = sink_writer(&p, &gpu, force, Vec::new())?;
                let mut frames_ = Composer::new(&p, &[ring.handle], gpu, route)?;
                let start = Instant::now();
                run(&mut frames_, &mut |s, t| out.video(s, t, frame_hns))?;
                out.finish()?;
                (route, notice, fps(start))
            }
            Via::Packets => {
                let enc = MfVideoEncoder::new(&gpu, force);
                let mut out = Encoded::new(enc, &video_config(&p), Vec::<(String, AacEncoder)>::new(), Mp4Sink::create(&path)?)?;
                let (route, notice) = (out.video_encoder().route().unwrap(), out.video_encoder().notice());
                let mut frames_ = Composer::new(&p, &[ring.handle], gpu, route)?;
                let start = Instant::now();
                run(&mut frames_, &mut |s, t| out.video(s, t, frame_hns))?;
                out.finish()?;
                (route, notice, fps(start))
            }
        }
    };
    Ok(Recorded { route, notice, path, fps })
}

/// The middle frame of `path` as RGB, by each decoder here: Media Foundation (as the trimmer
/// and Windows' players read it) and ffmpeg when it's on the PATH (going by the colour tags in
/// the stream or the file's `colr`, as most other players do).
fn decoded(path: &Path, (w, h): (u32, u32)) -> Vec<(&'static str, Vec<[u8; 3]>)> {
    let mut out = Vec::new();
    if let Ok(frame) = still(path, None, |d| d / 2, |w, h| (w, h)) {
        assert_eq!(frame.size, (w, h));
        out.push(("media foundation", frame.bgrx.chunks(4).map(|p| [p[2], p[1], p[0]]).collect()));
    }
    let ffmpeg = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vf", "select=eq(n\\,22),scale=in_color_matrix=auto:in_range=auto:out_range=pc", "-frames:v", "1"])
        .args(["-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
        .output();
    if let Ok(o) = ffmpeg
        && o.status.success()
        && o.stdout.len() == (w * h * 3) as usize
    {
        out.push(("ffmpeg", o.stdout.chunks(3).map(|p| [p[0], p[1], p[2]]).collect()));
    }
    out
}

/// Per decoder: the largest difference (per channel) between the patches as decoded and as
/// drawn, and whether the pointer came out magenta.
fn colour_error(path: &Path, (w, h): (u32, u32)) -> Vec<(&'static str, u8, bool)> {
    let error = |frame: &[[u8; 3]]| {
        let px = |x: u32, y: u32| frame[(y * w + x) as usize];
        let mut worst = 0;
        for (k, want) in PATCHES.iter().enumerate() {
            let (col, row) = (k as u32 % 4, k as u32 / 4);
            let got = px(col * w / 4 + w / 8, row * h / 2 + h / 4);
            for c in 0..3 {
                worst = worst.max(got[c].abs_diff(want[c]));
            }
        }
        let p = px(POINTER_AT.0 as u32 + 16, POINTER_AT.1 as u32 + 16);
        (worst, p[0] > 230 && p[1] < 25 && p[2] > 230)
    };
    decoded(path, (w, h)).into_iter().map(|(by, frame)| {
        let (worst, magenta) = error(&frame);
        (by, worst, magenta)
    }).collect()
}

/// Records one route and checks the file: size, codec, length, colours, the pointer.
fn check_route(which: Via, codec: Codec, force: Force, want: Route, pointer: bool) {
    check_route_sized(which, codec, force, want, pointer, (1280, 720));
}

fn check_route_sized(which: Via, codec: Codec, force: Force, want: Route, pointer: bool, size: (u32, u32)) {
    let r = record_route(which, codec, force, size, 45, pointer).unwrap();
    assert_eq!(r.route, want, "{which:?} {codec:?} {force:?}");
    let info = probe(&r.path).unwrap();
    assert_eq!((info.size, info.codec), (size, Some(codec)));
    assert!((info.duration - 15_000_000).abs() < 700_000, "{} s", info.duration as f64 / 1e7);
    let colours = colour_error(&r.path, size);
    let _ = std::fs::remove_file(&r.path);
    let pointer_ = if pointer { " + pointer" } else { "" };
    let what = format!("{which:?} {codec:?} {:?} {} {}x{}{pointer_}", want.engine, want.input.name(), size.0, size.1);
    eprintln!("{what}: {:.0} fps", r.fps);
    assert!(!colours.is_empty() || codec == Codec::Hevc, "{what}: H.264 always decodes");
    for (by, worst, magenta) in colours {
        eprintln!("    {by}: worst patch error {worst} levels, pointer magenta: {magenta}");
        assert!(worst <= 6, "{what}: {by} reads the patches off by {worst} levels");
        assert_eq!(magenta, pointer, "{what}: the pointer is drawn when it's on");
    }
}

const HW_ARGB: Route = Route { engine: Engine::Hardware, input: Input::Argb };
const HW_NV12: Route = Route { engine: Engine::Hardware, input: Input::Nv12 };
const SW_NV12: Route = Route { engine: Engine::Software, input: Input::Nv12 };
const SW_ARGB: Route = Route { engine: Engine::Software, input: Input::Argb };

fn forced(engine: Option<&str>, input: Option<&str>) -> Force {
    Force::parse(engine, input)
}

#[test]
#[ignore = "needs a hardware video encoder that takes ARGB32 (NVIDIA)"]
fn the_hardware_encoder_takes_argb32_first() {
    for which in [Via::SinkWriter, Via::Packets] {
        for codec in [Codec::H264, Codec::Hevc] {
            check_route(which, codec, Force::default(), HW_ARGB, true);
        }
    }
}

#[test]
#[ignore = "needs a hardware video encoder"]
fn the_hardware_encoder_on_nv12_from_the_video_processor() {
    for which in [Via::SinkWriter, Via::Packets] {
        for codec in [Codec::H264, Codec::Hevc] {
            check_route(which, codec, forced(None, Some("nv12")), HW_NV12, true);
            check_route(which, codec, forced(None, Some("nv12")), HW_NV12, false);
        }
    }
}

#[test]
#[ignore = "needs a GPU with a video processor"]
fn the_software_encoder_on_nv12_from_the_video_processor() {
    for which in [Via::SinkWriter, Via::Packets] {
        check_route(which, Codec::H264, forced(Some("software"), None), SW_NV12, true);
        check_route(which, Codec::H264, forced(Some("software"), None), SW_NV12, false);
        // too small for players to guess BT.709 without the file saying so
        check_route_sized(which, Codec::H264, forced(Some("software"), None), SW_NV12, true, (640, 360));
    }
}

#[test]
#[ignore = "needs a GPU"]
fn the_software_encoder_on_argb32_converted_by_media_foundation() {
    check_route(Via::SinkWriter, Codec::H264, forced(Some("software"), Some("argb")), SW_ARGB, true);
    check_route_sized(Via::SinkWriter, Codec::H264, forced(Some("software"), Some("argb")), SW_ARGB, true, (640, 360));
}

#[test]
#[ignore = "needs a GPU"]
fn a_rate_no_encoder_takes_is_recorded_at_a_whole_fraction_of_it() {
    // Windows' software H.264 encoder refuses more than about 172 fps
    let size = (640, 360);
    let path = temp_mp4("rate");
    let mut p = params(Codec::H264, size, &path);
    p.fps = 240.0;
    let force = forced(Some("software"), Some("nv12"));
    let (out, route, _, fps) = unsafe { sink_writer(&p, &gpu_sized(size), force, Vec::new()) }.unwrap();
    drop(out);
    assert_eq!((route, fps), (SW_NV12, 120.0));
    let mut enc = MfVideoEncoder::new(&gpu_sized(size), force);
    let Stream::Video { config, .. } = glint_core::record::pipeline::VideoEncoder::configure(&mut enc, &video_config(&p)).unwrap() else {
        panic!("not a video stream")
    };
    assert_eq!((enc.fps(), config.fps), (120.0, 120.0), "the packet path records and says the same rate");
    let _ = std::fs::remove_file(&path);
}

#[test]
#[ignore = "needs a GPU"]
fn no_software_hevc_encoder_is_said_plainly() {
    if !unsafe { super::gpu::software_encoders(Codec::Hevc) }.is_empty() {
        eprintln!("this PC has a software HEVC encoder");
        return;
    }
    for which in [Via::SinkWriter, Via::Packets] {
        let Err(e) = record_route(which, Codec::Hevc, forced(Some("software"), None), (640, 360), 3, false) else { panic!("recorded") };
        assert!(e.contains("Windows has no software HEVC encoder") && e.contains("Choose H.264"), "{which:?}: {e}");
    }
}

#[test]
#[ignore = "needs NVIDIA's encoder (its smallest H.264 frame is 146x50)"]
fn a_region_the_hardware_encoder_refuses_goes_to_the_software_one_quietly() {
    for which in [Via::SinkWriter, Via::Packets] {
        let r = record_route(which, Codec::H264, Force::default(), (100, 40), 30, true).unwrap();
        let info = probe(&r.path).unwrap();
        let _ = std::fs::remove_file(&r.path);
        assert_eq!((r.route, r.notice), (SW_NV12, None), "{which:?}");
        assert_eq!((info.size, info.codec), ((100, 40), Some(Codec::H264)));
    }
}

#[test]
#[ignore = "needs a GPU"]
fn the_smallest_region_the_picker_allows_records() {
    for codec in [Codec::H264, Codec::Hevc] {
        let (w, h) = codec.min_size();
        for which in [Via::SinkWriter, Via::Packets] {
            let r = record_route(which, codec, Force::default(), (w as u32, h as u32), 5, false).unwrap();
            let info = probe(&r.path).unwrap();
            let _ = std::fs::remove_file(&r.path);
            assert_eq!((info.size, info.codec), ((w as u32, h as u32), Some(codec)), "{which:?}");
            assert_eq!(r.notice, None);
        }
    }
}

#[test]
#[ignore = "needs a hardware video encoder; prints the cost of converting to NV12"]
fn nv12_conversion_cost_at_3440x1440() {
    let size = (3440, 1440);
    // the conversion alone, on the GPU
    let mut gpu = gpu_sized(size);
    let r = Rect { x: 0, y: 0, w: size.0 as i32, h: size.1 as i32 };
    let (src, dst) = unsafe {
        let mut src = None;
        let bind = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
        gpu.device.CreateTexture2D(&texture_desc(r, bind, D3D11_RESOURCE_MISC_FLAG(0)), None, Some(&mut src)).unwrap();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let mut dst = None;
        gpu.device.CreateTexture2D(&desc, None, Some(&mut dst)).unwrap();
        (src.unwrap(), dst.unwrap())
    };
    let converter = gpu.converter.as_mut().unwrap();
    let wait = |gpu_ctx: &ID3D11DeviceContext, device: &ID3D11Device| unsafe {
        let mut q = None;
        device.CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut q)).unwrap();
        let q = q.unwrap();
        gpu_ctx.End(&q);
        let mut done = windows::core::BOOL(0);
        while done.0 == 0 {
            let _ = gpu_ctx.GetData(&q, Some(&mut done as *mut _ as *mut _), size_of::<windows::core::BOOL>() as u32, 0);
        }
    };
    unsafe { converter.convert(&src, &dst, 0).unwrap() };
    wait(&gpu.ctx, &gpu.device);
    let n = 300;
    let start = Instant::now();
    for _ in 0..n {
        unsafe { converter.convert(&src, &dst, 0).unwrap() };
    }
    wait(&gpu.ctx, &gpu.device);
    let per = start.elapsed().as_secs_f64() / n as f64;
    eprintln!("BGRA -> NV12 at 3440x1440: {:.3} ms per frame ({:.0} fps alone)", per * 1e3, 1.0 / per);
    drop(gpu);

    // whole frames: compose, convert (or not) and encode, as fast as the encoder goes
    for which in [Via::SinkWriter, Via::Packets] {
        for codec in [Codec::H264, Codec::Hevc] {
            for input in ["argb", "nv12"] {
                let r = record_route(which, codec, forced(Some("hardware"), Some(input)), size, 300, false).unwrap();
                let _ = std::fs::remove_file(&r.path);
                eprintln!("{which:?} {codec:?} {input}: {:.0} fps", r.fps);
            }
        }
    }
    let r = record_route(Via::SinkWriter, Codec::H264, forced(Some("software"), None), size, 60, false).unwrap();
    let _ = std::fs::remove_file(&r.path);
    eprintln!("SinkWriter H264 software nv12: {:.0} fps", r.fps);
    let r = record_route(Via::Packets, Codec::H264, forced(Some("software"), None), size, 60, false).unwrap();
    let _ = std::fs::remove_file(&r.path);
    eprintln!("Packets H264 software nv12: {:.0} fps", r.fps);
}


/// Two seconds of the first monitor's top-left 1280×720 through the whole recorder (Desktop
/// Duplication included), with `GLINT_ENCODER`/`GLINT_ENCODER_INPUT` set as given.
fn real_capture(packets: bool, engine: &str, input: &str) -> (Vec<String>, glint_core::clip::info::Info) {
    use glint_core::record::pipeline::Outcome;
    glint_core::log::set_sink(|m| eprintln!("log: {m}"));
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap();
        std::env::set_var("GLINT_ENCODER", engine);
        std::env::set_var("GLINT_ENCODER_INPUT", input);
    }
    let output = unsafe { adapter(0).unwrap().EnumOutputs(0).unwrap() };
    let desktop = Rect::from_win(&unsafe { output.GetDesc() }.unwrap().DesktopCoordinates);
    let path = temp_mp4(&format!("real-{packets}-{engine}-{input}"));
    let mut p = params(Codec::H264, (1280, 720), &path);
    p.region = Rect { x: desktop.x, y: desktop.y, w: 1280, h: 720 };
    p.sources = vec![super::Source { adapter_idx: 0, output_idx: 0, desktop }];
    let (tx, rx) = std::sync::mpsc::channel();
    let done = move |r: Result<Outcome, String>| tx.send(r).unwrap();
    let starting = match packets {
        true => super::start_packets(p, { let path = path.clone(); move || Mp4Sink::create(&path) }, done),
        false => super::start(p, done),
    };
    let rec = starting.and_then(|s| s.wait()).unwrap();
    let notices = rec.notices().to_vec();
    std::thread::sleep(std::time::Duration::from_secs(2));
    rec.stop();
    let Ok(Outcome::Saved(s)) = rec_done(&rx) else { panic!("not saved") };
    let info = probe(&path).unwrap();
    let frame = still(&path, None, |d| d / 2, |w, h| (w, h)).unwrap();
    let _ = std::fs::remove_file(&path);
    unsafe {
        std::env::remove_var("GLINT_ENCODER");
        std::env::remove_var("GLINT_ENCODER_INPUT");
    }
    assert_eq!(frame.size, (1280, 720));
    eprintln!("real capture, packets {packets}, {engine} {input}: {:.2} s, {} dropped, notices {notices:?}", s.duration_s, s.dropped);
    (notices, info)
}

fn rec_done(rx: &std::sync::mpsc::Receiver<Result<glint_core::record::pipeline::Outcome, String>>) -> Result<glint_core::record::pipeline::Outcome, String> {
    rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap()
}

#[test]
#[ignore = "records the first monitor for a few seconds; sets environment variables, so run alone"]
fn the_recorder_takes_the_forced_routes_from_the_environment() {
    for packets in [false, true] {
        for (engine, input, software) in [("", "", false), ("hardware", "nv12", false), ("software", "", true)] {
            let (notices, info) = real_capture(packets, engine, input);
            assert_eq!((info.size, info.codec), ((1280, 720), Some(Codec::H264)));
            assert!((info.duration - 20_000_000).abs() < 3_000_000, "{} s", info.duration as f64 / 1e7);
            assert_eq!(!notices.is_empty(), software, "the software encoder is told about");
        }
    }
}


// Big frames at the monitor's rate through the whole recorder: a scrolling synthetic capture
// paced like Desktop Duplication, with another encoder session on the GPU (afterglow's).

/// A text-like page twice the region's height, copied into a shared ring a few lines further
/// down each frame: what scrolling looks like to the encoder.
struct Scroller {
    size: (u32, u32),
    fps: f64,
    ctx: ID3D11DeviceContext,
    page: ID3D11Texture2D,
    ring: Vec<(ID3D11Texture2D, IDXGIKeyedMutex)>,
    handles: Vec<HANDLE>,
    _device: ID3D11Device,
}

// d3d11 objects are free-threaded; after the move only the capture thread touches them.
unsafe impl Send for Scroller {}

impl Scroller {
    fn new((w, h): (u32, u32), fps: f64) -> Scroller {
        unsafe {
            let (device, ctx) = create_device(&adapter(0).unwrap(), D3D11_CREATE_DEVICE_FLAG(0)).unwrap();
            let mut x = 0x9E37_79B9u32;
            let cols = w.div_ceil(4);
            // 4×4 cells, a quarter of them dark: small text on a light background
            let cells: Vec<u32> = (0..cols * (2 * h).div_ceil(4))
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    let v = if x.is_multiple_of(4) { x >> 24 & 0x7F } else { 0xF0 | (x >> 24 & 0x0F) };
                    0xFF00_0000 | v << 16 | v << 8 | v
                })
                .collect();
            let px: Vec<u32> = (0..w * 2 * h).map(|i| cells[((i / w / 4) * cols + i % w / 4) as usize]).collect();
            let page_rect = Rect { x: 0, y: 0, w: w as i32, h: 2 * h as i32 };
            let data = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as _, SysMemPitch: w * 4, SysMemSlicePitch: 0 };
            let mut page = None;
            device
                .CreateTexture2D(&texture_desc(page_rect, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_FLAG(0)), Some(&data), Some(&mut page))
                .unwrap();
            let r = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
            let (mut ring, mut handles) = (Vec::new(), Vec::new());
            for _ in 0..RING {
                let desc = texture_desc(r, D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX);
                let mut tex = None;
                device.CreateTexture2D(&desc, None, Some(&mut tex)).unwrap();
                let tex = tex.unwrap();
                handles.push(tex.cast::<IDXGIResource>().unwrap().GetSharedHandle().unwrap());
                ring.push((tex.clone(), tex.cast().unwrap()));
            }
            Scroller { size: (w, h), fps, ctx, page: page.unwrap(), ring, handles, _device: device }
        }
    }

    /// Frame `i` into ring texture `slot`, for the compositor.
    fn put(&self, slot: usize, i: u64) -> Result<(), String> {
        let (w, h) = self.size;
        let top = (i * 6 % h as u64) as u32;
        let (tex, km) = &self.ring[slot];
        unsafe {
            km.AcquireSync(0, INFINITE).map_err(|e| e.to_string())?;
            let b = D3D11_BOX { left: 0, top, front: 0, right: w, bottom: top + h, back: 1 };
            self.ctx.CopySubresourceRegion(tex, 0, 0, 0, 0, &self.page, 0, Some(&b));
            km.ReleaseSync(1).map_err(|e| e.to_string())
        }
    }
}

impl glint_core::record::pipeline::Capture for Scroller {
    type Image = usize;

    /// A frame every 1/fps s while a ring texture is free (a drop when none is), like capture.
    fn run(
        &mut self,
        stop: &std::sync::atomic::AtomicBool,
        out: &std::sync::mpsc::Sender<Event<usize>>,
        free: &std::sync::mpsc::Receiver<usize>,
    ) -> Result<CaptureStats, String> {
        let mut stats = CaptureStats::default();
        let mut idle: Vec<usize> = (0..self.ring.len()).collect();
        let start = Instant::now();
        for i in 0u64.. {
            let due = start + std::time::Duration::from_secs_f64(i as f64 / self.fps);
            if let Some(d) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(d);
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            idle.extend(free.try_iter());
            let Some(slot) = idle.pop() else {
                stats.dropped += 1;
                continue;
            };
            self.put(slot, i)?;
            if out.send(Event::Frame { image: Some(slot), at: hns(qpc()), cursor: None }).is_err() {
                break;
            }
        }
        Ok(stats)
    }
}

/// Time spent in one call of the encoder thread.
#[derive(Clone, Copy, Debug, Default)]
struct Spent {
    calls: u64,
    total: std::time::Duration,
    worst: std::time::Duration,
}

impl Spent {
    fn time<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let r = f();
        let d = start.elapsed();
        self.calls += 1;
        self.total += d;
        self.worst = self.worst.max(d);
        r
    }
}

impl std::fmt::Display for Spent {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let mean = self.total.as_secs_f64() * 1e3 / self.calls.max(1) as f64;
        write!(f, "{mean:.3} ms mean, {:.1} ms worst ({} calls)", self.worst.as_secs_f64() * 1e3, self.calls)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Calls {
    latch: Spent,
    alloc: Spent,
    compose: Spent,
    write: Spent,
    /// Allocations that found the pool empty.
    empty: u64,
}

/// The compositor and output, timing each call.
struct Timed<T> {
    inner: T,
    calls: std::sync::Arc<std::sync::Mutex<Calls>>,
}

impl Frames for Timed<Composer> {
    type Image = usize;
    type Frame = IMFSample;

    fn latch(&mut self, image: &usize) -> Result<(), String> {
        let inner = &mut self.inner;
        self.calls.lock().unwrap().latch.time(|| inner.latch(image))
    }

    fn set_pointer(&mut self, shape: &PointerShape) {
        self.inner.set_pointer(shape)
    }

    fn alloc(&mut self) -> Result<Option<IMFSample>, String> {
        let inner = &mut self.inner;
        let mut calls = self.calls.lock().unwrap();
        let r = calls.alloc.time(|| inner.alloc());
        calls.empty += matches!(r, Ok(None)) as u64;
        r
    }

    fn compose(&mut self, frame: &IMFSample, cursor: Option<(i32, i32)>, clicks: &[glint_core::record::clicks::Shape], keys: Option<glint_core::record::keys::KeyFrame>) -> Result<(), String> {
        let inner = &mut self.inner;
        self.calls.lock().unwrap().compose.time(|| inner.compose(frame, cursor, clicks, keys))
    }
}

impl<O: Output<Frame = IMFSample>> Output for Timed<O> {
    type Frame = IMFSample;

    fn video(&mut self, frame: IMFSample, time: i64, duration: i64) -> Result<(), String> {
        let inner = &mut self.inner;
        self.calls.lock().unwrap().write.time(|| inner.video(frame, time, duration))
    }

    fn audio(&mut self, track: usize, at: i64, pcm: &[i16]) -> Result<(), String> {
        self.inner.audio(track, at, pcm)
    }

    fn finish(&mut self) -> Result<(), String> {
        self.inner.finish()
    }
}

/// Drops what an encoder makes.
struct Discard;

impl PacketSink for Discard {
    fn add_stream(&mut self, _: Stream) -> Result<usize, String> {
        Ok(0)
    }

    fn write(&mut self, _: usize, _: Packet) -> Result<(), String> {
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), String> {
        Ok(())
    }
}

enum Out {
    Sink(super::encoder::SinkWriter),
    Packets(Encoded<MfVideoEncoder, AacEncoder, Discard>),
}

impl Output for Out {
    type Frame = IMFSample;

    fn video(&mut self, frame: IMFSample, time: i64, duration: i64) -> Result<(), String> {
        match self {
            Out::Sink(o) => o.video(frame, time, duration),
            Out::Packets(o) => o.video(frame, time, duration),
        }
    }

    fn audio(&mut self, track: usize, at: i64, pcm: &[i16]) -> Result<(), String> {
        match self {
            Out::Sink(o) => o.audio(track, at, pcm),
            Out::Packets(o) => o.audio(track, at, pcm),
        }
    }

    fn finish(&mut self) -> Result<(), String> {
        match self {
            Out::Sink(o) => o.finish(),
            Out::Packets(o) => o.finish(),
        }
    }
}

/// The recorder on a [`Scroller`]: the sink writer into `params.path`, or with `packets` the
/// packet path into nothing (afterglow's encoder, its buffer aside).
struct Bench {
    params: Params,
    packets: bool,
    calls: std::sync::Arc<std::sync::Mutex<Calls>>,
}

impl Backend for Bench {
    type Capture = Scroller;
    type Frames = Timed<Composer>;
    type Output = Timed<Out>;
    type Audio = super::audio::Source;
    type Mouse = super::mouse::Hook;
    type Keys = super::keys::Hook;
    type Guard = <super::win::Windows as Backend>::Guard;

    fn enter(role: Role) -> Self::Guard {
        <super::win::Windows as Backend>::enter(role)
    }

    fn now() -> i64 {
        hns(qpc())
    }

    fn open(self) -> Result<Parts<Self>, String> {
        let p = self.params;
        let size = (p.region.w as u32, p.region.h as u32);
        let capture = Scroller::new(size, p.fps);
        unsafe {
            let gpu = Gpu::open(&adapter(0)?, size)?;
            let (output, route) = match self.packets {
                true => {
                    let out = Encoded::new(MfVideoEncoder::new(&gpu, Force::default()), &video_config(&p), Vec::new(), Discard)?;
                    let route = out.video_encoder().route().ok_or("no route")?;
                    (Out::Packets(out), route)
                }
                false => {
                    let (out, route, _, _) = sink_writer(&p, &gpu, Force::default(), Vec::new())?;
                    (Out::Sink(out), route)
                }
            };
            let frames = Composer::new(&p, &capture.handles, gpu, route)?;
            Ok(Parts {
                capture,
                frames: Timed { inner: frames, calls: self.calls.clone() },
                output: Timed { inner: output, calls: self.calls },
                audio: open_audio(&[], &[]),
                fps: p.fps,
                path: p.path.clone(),
                clicks: None,
                keys: None,
                notices: Vec::new(),
            })
        }
    }
}

/// A running [`Bench`] recorder.
struct Running {
    rec: Recorder,
    done: std::sync::mpsc::Receiver<Result<Outcome, String>>,
    calls: std::sync::Arc<std::sync::Mutex<Calls>>,
}

fn bench_start(params: Params, packets: bool) -> Running {
    glint_core::log::set_sink(|m| eprintln!("log: {m}"));
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Calls::default()));
    let (tx, done) = std::sync::mpsc::channel();
    let bench = Bench { params, packets, calls: calls.clone() };
    let rec = glint_core::record::pipeline::start(bench, move |r| tx.send(r).unwrap()).and_then(|s| s.wait()).unwrap();
    Running { rec, done, calls }
}

impl Running {
    fn stop(self) -> (Summary, Calls) {
        self.rec.stop();
        let Ok(Outcome::Saved(s)) = self.done.recv_timeout(std::time::Duration::from_secs(30)).unwrap() else { panic!("not saved") };
        let calls = *self.calls.lock().unwrap();
        (s, calls)
    }
}

/// The user's settings at `size`: H.264, qp 20, `effort`, at `fps`.
fn big_params(size: (u32, u32), fps: f64, effort: u32, path: &Path) -> Params {
    Params { fps, qp: 20, effort, draw_cursor: false, ..params(Codec::H264, size, path) }
}

/// `secs` of the sink writer recording `size` at `fps` and `effort` (through the whole recorder,
/// the effort fitted as for a recording), with afterglow's encoder (packet path, effort 20, same
/// size and rate) running alongside. Returns the recording's and afterglow's summaries.
fn record_big(size: (u32, u32), fps: f64, effort: u32, secs: f64) -> (Summary, Summary) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).unwrap();
    }
    let path = temp_mp4(&format!("big-{effort}"));
    let afterglow = bench_start(big_params(size, fps, 20, Path::new("unused.mp4")), true);
    std::thread::sleep(std::time::Duration::from_secs(1));
    let main = bench_start(big_params(size, fps, effort, &path), false);
    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
    let (s, calls) = main.stop();
    let (beside, _) = afterglow.stop();
    let info = probe(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    eprintln!(
        "{}x{}@{fps} effort {effort} beside afterglow: {} frames, {} dropped ({} with the sample pool empty; afterglow: {}), {:.2} s, file {:.2} s",
        size.0,
        size.1,
        calls.write.calls,
        s.dropped,
        calls.empty,
        beside.dropped,
        s.duration_s,
        info.duration as f64 / 1e7
    );
    eprintln!("  latch {}\n  alloc {}\n  compose {}\n  write {}", calls.latch, calls.alloc, calls.compose, calls.write);
    (s, beside)
}

/// Frames per second the sink writer takes at `size`, `codec` and `effort`, scrolling, as fast
/// as it goes, and the file's size. Configured for 30 fps, where no effort is too slow (the rate
/// it's configured for doesn't change the encoder's speed).
fn sink_writer_throughput(size: (u32, u32), codec: Codec, effort: u32, frames: u64) -> (f64, u64) {
    let path = temp_mp4(&format!("rate-{effort}"));
    let p = Params { codec, ..big_params(size, 30.0, effort, &path) };
    let gpu = gpu_sized(size);
    let scroller = Scroller::new(size, 30.0);
    let frame_hns = 10_000_000 / 30;
    let fps = unsafe {
        let (mut out, route, _, _) = sink_writer(&p, &gpu, Force::default(), Vec::new()).unwrap();
        let mut composer = Composer::new(&p, &scroller.handles, gpu, route).unwrap();
        let start = Instant::now();
        for i in 0..frames {
            scroller.put(0, i).unwrap();
            composer.latch(&0).unwrap();
            let sample = loop {
                match composer.alloc().unwrap() {
                    Some(s) => break s,
                    None => std::thread::sleep(std::time::Duration::from_micros(200)),
                }
            };
            composer.compose(&sample, None, &[], None).unwrap();
            out.video(sample, i as i64 * frame_hns, frame_hns).unwrap();
        }
        out.finish().unwrap();
        frames as f64 / start.elapsed().as_secs_f64()
    };
    let bytes = std::fs::metadata(&path).unwrap().len();
    let _ = std::fs::remove_file(&path);
    (fps, bytes)
}

#[test]
#[ignore = "needs a hardware video encoder; prints how fast it goes at 3440x1440 per effort (see record::effort)"]
fn sink_writer_throughput_at_3440x1440() {
    let size = (3440, 1440);
    for alongside in [false, true] {
        let afterglow = alongside.then(|| bench_start(big_params(size, 144.0, 20, Path::new("unused.mp4")), true));
        for codec in [Codec::H264, Codec::Hevc] {
            for effort in [100, 50, 25] {
                let (fps, bytes) = sink_writer_throughput(size, codec, effort, 600);
                let alongside = if alongside { " beside afterglow" } else { "" };
                eprintln!("{codec:?} effort {effort}{alongside}: {fps:.0} fps, {:.1} MB", bytes as f64 / 1e6);
            }
        }
        if let Some(a) = afterglow {
            a.stop();
        }
    }
}

#[test]
#[ignore = "needs a hardware video encoder; records 3440x1440 at 144 fps for a few seconds"]
fn big_frames_at_144_fps_record_without_drops_beside_afterglow() {
    let (s, beside) = record_big((3440, 1440), 144.0, 100, 8.0);
    assert_eq!((s.dropped, beside.dropped), (0, 0));
}
