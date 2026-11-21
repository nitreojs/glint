use std::path::Path;

use glint_core::clip::info::{Info, Track};
use glint_core::log::log;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Dxgi::IDXGIAdapter;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Variant::VT_I8;
use windows::core::{GUID, HSTRING, Interface};

use crate::record::Ctx;

pub(super) const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
pub const ANY: u32 = MF_SOURCE_READER_ANY_STREAM.0 as u32;
pub const ALL: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;

pub struct Com;

impl Com {
    pub fn init() -> Self {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        }
        Com
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

pub(super) fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

pub(super) unsafe fn attributes(n: u32) -> Result<IMFAttributes, String> {
    let mut a = None;
    unsafe { MFCreateAttributes(&mut a, n).ctx("MFCreateAttributes")? };
    Ok(a.unwrap())
}

pub struct Gpu {
    pub mgr: IMFDXGIDeviceManager,
}

impl Gpu {
    pub fn new() -> Result<Self, String> {
        unsafe {
            let mut device = None;
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .ctx("D3D11CreateDevice")?;
            let device = device.unwrap();
            let mt: ID3D11Multithread = device.cast().ctx("ID3D11Multithread")?;
            let _ = mt.SetMultithreadProtected(true);
            let mut token = 0;
            let mut mgr = None;
            MFCreateDXGIDeviceManager(&mut token, &mut mgr).ctx("MFCreateDXGIDeviceManager")?;
            let mgr = mgr.unwrap();
            mgr.ResetDevice(&device, token).ctx("IMFDXGIDeviceManager::ResetDevice")?;
            Ok(Gpu { mgr })
        }
    }
}

pub fn open(path: &Path, gpu: Option<&Gpu>) -> Result<IMFSourceReader, String> {
    unsafe {
        let attrs = attributes(3)?;
        attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1).ctx("attr")?;
        if let Some(g) = gpu {
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).ctx("attr")?;
            attrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &g.mgr).ctx("attr")?;
        }
        MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs).ctx("can't open the video")
    }
}

/// Whether windows has an encoder (hardware or software) that writes `subtype`.
pub fn has_encoder(subtype: GUID) -> bool {
    any_transform(MFT_CATEGORY_VIDEO_ENCODER, None, Some(subtype))
}

/// Whether windows can decode `subtype` (hevc needs the "hevc video extensions" from the store).
pub fn has_decoder(subtype: GUID) -> bool {
    any_transform(MFT_CATEGORY_VIDEO_DECODER, Some(subtype), None)
}

fn any_transform(category: GUID, input: Option<GUID>, output: Option<GUID>) -> bool {
    let info = |g: GUID| MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: g };
    let (input, output) = (input.map(info), output.map(info));
    unsafe {
        let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER;
        let (mut list, mut count) = (std::ptr::null_mut(), 0u32);
        let (i, o) = (input.as_ref().map(|i| i as *const _), output.as_ref().map(|o| o as *const _));
        if MFTEnumEx(category, flags, i, o, &mut list, &mut count).is_err() {
            return false;
        }
        if !list.is_null() {
            for i in 0..count as usize {
                drop(std::ptr::read(list.add(i)));
            }
            windows::Win32::System::Com::CoTaskMemFree(Some(list as _));
        }
        count > 0
    }
}

pub fn rgb_reader(src: &Path, size: (u32, u32)) -> Result<IMFSourceReader, String> {
    let gpu = Gpu::new();
    if let Err(e) = &gpu {
        log(&format!("decoding in software: {e}"));
    }
    rgb_reader_on(src, gpu.as_ref().ok(), size)
}

/// A reader of `src`'s video as RGB32 frames of `size`, decoding on `gpu` when it can (one device
/// can serve many readers); in software otherwise.
pub fn rgb_reader_on(src: &Path, gpu: Option<&Gpu>, size: (u32, u32)) -> Result<IMFSourceReader, String> {
    let on_gpu = gpu.ok_or_else(String::new).and_then(|g| {
        let r = open(src, Some(g))?;
        video_only(&r, MFVideoFormat_RGB32, size)?;
        Ok(r)
    });
    on_gpu.or_else(|e| {
        if !e.is_empty() {
            log(&format!("decoding in software: {e}"));
        }
        let r = open(src, None)?;
        video_only(&r, MFVideoFormat_RGB32, size)?;
        Ok(r)
    })
}

/// The picture's size: the display aperture when the coded frame is padded.
fn display_size(native: &IMFMediaType) -> Result<(u32, u32), String> {
    unsafe {
        let mut size = unpack(native.GetUINT64(&MF_MT_FRAME_SIZE).ctx("frame size")?);
        let mut area = MFVideoArea::default();
        let blob = std::slice::from_raw_parts_mut(&mut area as *mut _ as *mut u8, size_of::<MFVideoArea>());
        for key in [MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_GEOMETRIC_APERTURE] {
            if native.GetBlob(&key, blob, None).is_ok() && area.Area.cx > 0 && area.Area.cy > 0 {
                size = (area.Area.cx as u32, area.Area.cy as u32);
                break;
            }
        }
        Ok(size)
    }
}

/// How long the file plays, in 100 ns units.
fn length(reader: &IMFSourceReader) -> Result<i64, String> {
    unsafe {
        let mut pv =
            reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION).ctx("duration")?;
        let duration = pv.Anonymous.Anonymous.Anonymous.uhVal as i64;
        let _ = PropVariantClear(&mut pv);
        Ok(duration)
    }
}

/// One frame of a video, for a thumbnail.
pub struct Still {
    /// The frame as decoded: `size`, BGRX rows.
    pub size: (u32, u32),
    pub bgrx: Vec<u8>,
    /// The video's own picture size and length (100 ns units).
    pub native: (u32, u32),
    pub duration: i64,
}

/// The frame of `src` shown at `at(length)`, decoded at `fit(native size)` (even sides), on `gpu`
/// when it can.
pub fn still(
    src: &Path,
    gpu: Option<&Gpu>,
    at: impl FnOnce(i64) -> i64,
    fit: impl FnOnce(u32, u32) -> (u32, u32),
) -> Result<Still, String> {
    let reader = open(src, None)?;
    let native = unsafe { reader.GetNativeMediaType(VIDEO, 0) }.map_err(|_| "the file has no video".to_string())?;
    let native = display_size(&native)?;
    let duration = length(&reader)?;
    drop(reader);
    let size = fit(native.0, native.1);
    let reader = rgb_reader_on(src, gpu, size)?;
    let (found, _) = decode_until(&reader, at(duration), None)?;
    let frame = found.ok_or("the video has no frames")?;
    let row = size.0 as usize * 4;
    let bgrx = with_pixels(&frame.sample, size.0, |rows| {
        let mut px = Vec::with_capacity(row * size.1 as usize);
        for y in 0..size.1 as usize {
            px.extend_from_slice(rows.row(y));
        }
        px
    })?;
    Ok(Still { size, bgrx, native, duration })
}

pub fn probe(path: &Path) -> Result<Info, String> {
    unsafe {
        let reader = open(path, None)?;
        let native = reader.GetNativeMediaType(VIDEO, 0).map_err(|_| "the file has no video".to_string())?;
        let size = display_size(&native)?;
        let fps = match native.GetUINT64(&MF_MT_FRAME_RATE).map(unpack) {
            Ok((n, d)) if n > 0 && d > 0 => n as f64 / d as f64,
            _ => 30.0,
        };
        let codec = super::smart::codec(&native);
        let snap = super::smart::snap(&reader, VIDEO, &native);
        let duration = length(&reader)?;
        let tracks = tracks(path, &reader);
        let clicks = glint_core::record::clicks::load(path).unwrap_or_else(|e| {
            log(&format!("clip: the clicks in {} can't be read: {e}", path.display()));
            None
        });
        let keys = glint_core::record::keys::load(path).unwrap_or_else(|e| {
            log(&format!("clip: the keystrokes in {} can't be read: {e}", path.display()));
            None
        });
        Ok(Info { duration, size, fps, codec, tracks, clicks, keys, snap })
    }
}

fn tracks(path: &Path, reader: &IMFSourceReader) -> Vec<Track> {
    let names = glint_core::record::mp4::track_names(path);
    let kinds = glint_core::record::mp4::trak_kinds(path);
    let majors: Vec<bool> = streams(reader).iter().map(|s| s.1 == MFMediaType_Audio).collect();
    let mut audio: Vec<u32> = (0..majors.len() as u32).filter(|&i| majors[i as usize]).collect();
    if reversed(&kinds, &majors) {
        audio.reverse();
    }
    (audio.into_iter().enumerate())
        .map(|(n, stream)| {
            let name = names.get(n).cloned().flatten().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            Track { stream, name: name.unwrap_or_else(|| format!("track {}", n + 1)) }
        })
        .collect()
}

// measured: the mpeg-4 source lists traks in reverse moov order (ffmpeg- and sink-writer-made files alike).
fn reversed(kinds: &[[u8; 4]], audio: &[bool]) -> bool {
    let sound: Vec<bool> = kinds.iter().map(|k| k == b"soun").collect();
    sound != audio || sound.iter().rev().eq(audio)
}

pub(super) fn audio_rate(reader: &IMFSourceReader, stream: u32) -> u32 {
    unsafe {
        reader.GetNativeMediaType(stream, 0).and_then(|t| t.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)).unwrap_or(48_000)
    }
}

/// Float output at the track's own rate: (rate, channels).
pub fn float_native(reader: &IMFSourceReader, stream: u32) -> Result<(u32, usize), String> {
    unsafe {
        let t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ctx("mt")?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float).ctx("mt")?;
        if reader.SetCurrentMediaType(stream, None, &t).is_err() {
            return Err("the audio can't be decoded".into());
        }
        let cur = reader.GetCurrentMediaType(stream).ctx("GetCurrentMediaType")?;
        let rate = cur.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(0);
        let channels = cur.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(0) as usize;
        if rate == 0 || channels == 0 {
            return Err("the audio has no format".into());
        }
        Ok((rate, channels))
    }
}

pub(super) fn float_output(reader: &IMFSourceReader, stream: u32, rate: u32) -> Result<usize, String> {
    let (got, channels) = float_native(reader, stream)?;
    if got != rate {
        return Err(format!("audio tracks at {got} Hz and {rate} Hz can't be mixed"));
    }
    Ok(channels)
}

pub fn floats(sample: &IMFSample, out: &mut Vec<f32>) -> Result<(), String> {
    unsafe {
        let buf = sample.ConvertToContiguousBuffer().ctx("ConvertToContiguousBuffer")?;
        let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
        buf.Lock(&mut data, None, Some(&mut len)).ctx("Lock")?;
        let bytes = std::slice::from_raw_parts(data, len as usize);
        out.clear();
        out.extend(bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
        let _ = buf.Unlock();
        Ok(())
    }
}

pub(super) fn streams(reader: &IMFSourceReader) -> Vec<(u32, GUID, IMFMediaType)> {
    (0..)
        .map_while(|i| unsafe {
            let t = reader.GetNativeMediaType(i, 0).ok()?;
            Some((i, t.GetMajorType().unwrap_or_default(), t))
        })
        .collect()
}

pub(super) fn video_only(reader: &IMFSourceReader, subtype: GUID, size: (u32, u32)) -> Result<(), String> {
    unsafe {
        reader.SetStreamSelection(ALL, false).ctx("SetStreamSelection")?;
        reader.SetStreamSelection(VIDEO, true).ctx("SetStreamSelection")?;
        set_video_output(reader, subtype, size)
    }
}

pub(super) fn set_video_output(reader: &IMFSourceReader, subtype: GUID, size: (u32, u32)) -> Result<(), String> {
    unsafe {
        let t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype).ctx("mt")?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(size.0, size.1)).ctx("mt")?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).ctx("mt")?;
        if subtype == MFVideoFormat_RGB32 {
            t.SetUINT32(&MF_MT_DEFAULT_STRIDE, size.0 * 4).ctx("mt")?;
        }
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ctx("mt")?;
        let set = reader.SetCurrentMediaType(VIDEO, None, &t);
        if let Err(e) = &set
            && e.code() == MF_E_TOPO_CODEC_NOT_FOUND
            && reader
                .GetNativeMediaType(VIDEO, 0)
                .and_then(|n| n.GetGUID(&MF_MT_SUBTYPE))
                .is_ok_and(|g| g == MFVideoFormat_HEVC)
        {
            return Err("windows has no hevc decoder: install “hevc video extensions” from the microsoft store".into());
        }
        set.ctx("the video can't be decoded")?;
        keep_frame_times(reader);
        Ok(())
    }
}

/// Turns off the frame rate conversion of the video processor the reader put after the decoder. Left on, it
/// resamples a variable-rate recording onto a steady clock at the file's average rate (measured on a 144 fps
/// game recording averaging 67 fps: every frame at a multiple of 1/67 s, some repeated, others dropped),
/// so frames would no longer come out at their own times.
fn keep_frame_times(reader: &IMFSourceReader) {
    let Ok(ex) = reader.cast::<IMFSourceReaderEx>() else { return };
    for k in 0.. {
        let (mut category, mut transform) = (GUID::zeroed(), None);
        if unsafe { ex.GetTransformForStream(VIDEO, k, Some(&mut category), &mut transform) }.is_err() {
            return;
        }
        let Some(xvp) = transform.filter(|_| category == MFT_CATEGORY_VIDEO_PROCESSOR) else { continue };
        let off = xvp.cast::<IMFTransform>().and_then(|t| unsafe { t.GetAttributes()?.SetUINT32(&MF_XVP_DISABLE_FRC, 1) });
        if let Err(e) = off {
            log(&format!("clip: the video processor keeps converting the frame rate: {e}"));
        }
    }
}

pub fn seek(reader: &IMFSourceReader, t: i64) -> Result<(), String> {
    unsafe {
        let mut pv = PROPVARIANT::default();
        (*pv.Anonymous.Anonymous).vt = VT_I8;
        (*pv.Anonymous.Anonymous).Anonymous.hVal = t.max(0);
        reader.SetCurrentPosition(&GUID::zeroed(), &pv).ctx("seek")
    }
}

pub enum Got {
    Sample { stream: u32, ts: i64, sample: IMFSample },
    End(u32),
}

pub fn read(reader: &IMFSourceReader, stream: u32) -> Result<Got, String> {
    loop {
        let (mut idx, mut flags, mut ts, mut sample) = (0u32, 0u32, 0i64, None);
        unsafe {
            reader
                .ReadSample(stream, 0, Some(&mut idx), Some(&mut flags), Some(&mut ts), Some(&mut sample))
                .ctx("ReadSample")?;
        }
        if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
            return Err("the video couldn't be decoded".into());
        }
        if let Some(sample) = sample {
            return Ok(Got::Sample { stream: idx, ts, sample });
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            return Ok(Got::End(idx));
        }
    }
}

pub(super) fn duration(sample: &IMFSample) -> i64 {
    unsafe { sample.GetSampleDuration().unwrap_or(0) }
}

#[derive(Clone)]
pub struct Decoded {
    pub ts: i64,
    pub dur: i64,
    pub sample: IMFSample,
}

pub fn next_video(reader: &IMFSourceReader) -> Result<Option<Decoded>, String> {
    Ok(match read(reader, VIDEO)? {
        Got::Sample { ts, sample, .. } => Some(Decoded { ts, dur: duration(&sample), sample }),
        Got::End(_) => None,
    })
}

pub fn decode_until(
    reader: &IMFSourceReader,
    t: i64,
    mut best: Option<Decoded>,
) -> Result<(Option<Decoded>, Option<Decoded>), String> {
    loop {
        if best.as_ref().is_some_and(|b| b.dur > 0 && b.ts + b.dur > t) {
            return Ok((best, None));
        }
        match next_video(reader)? {
            None => return Ok((best, None)),
            Some(f) if f.ts <= t => best = Some(f),
            Some(f) if best.is_none() => return Ok((Some(f), None)),
            Some(f) => return Ok((best, Some(f))),
        }
    }
}

pub struct Rows {
    scan0: *const u8,
    pitch: isize,
    width: usize,
}

impl Rows {
    pub fn row(&self, y: usize) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.scan0.offset(y as isize * self.pitch), self.width) }
    }
}

pub fn with_pixels<R>(sample: &IMFSample, width: u32, f: impl FnOnce(&Rows) -> R) -> Result<R, String> {
    unsafe {
        let buf = sample.GetBufferByIndex(0).ctx("GetBufferByIndex")?;
        let width = width as usize * 4;
        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
            let mut scan0 = std::ptr::null_mut();
            let mut pitch = 0;
            b2.Lock2D(&mut scan0, &mut pitch).ctx("Lock2D")?;
            let r = f(&Rows { scan0, pitch: pitch as isize, width });
            let _ = b2.Unlock2D();
            return Ok(r);
        }
        let mut scan0 = std::ptr::null_mut();
        buf.Lock(&mut scan0, None, None).ctx("Lock")?;
        let r = f(&Rows { scan0, pitch: width as isize, width });
        let _ = buf.Unlock();
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_order_is_reversed_unless_the_moov_order_matches() {
        let (v, a) = (*b"vide", *b"soun");
        assert!(reversed(&[v, a, a], &[true, true, false]));
        assert!(!reversed(&[v, a, a], &[false, true, true]));
        assert!(reversed(&[a, v, a], &[true, false, true]));
        assert!(reversed(&[], &[true, true, false]));
    }
}
