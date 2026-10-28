//! Just enough of h.264 and hevc bitstreams for a smart cut: what a stream's sequence parameter
//! set asks of a decoder, and access units given other parameter sets in-band.

use crate::record::Codec;

const START: [u8; 4] = [0, 0, 0, 1];

/// The NAL units of an Annex B byte stream, without their start codes.
pub fn units(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut begin = None;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(b) = begin {
                out.push(trim_zeros(&data[b..i]));
            }
            i += 3;
            begin = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(b) = begin {
        out.push(&data[b..]);
    }
    out.retain(|u| !u.is_empty());
    out
}

/// A unit without the zero bytes that belong to the next start code.
fn trim_zeros(unit: &[u8]) -> &[u8] {
    let end = unit.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    &unit[..end]
}

/// The parameter sets of a codec configuration as media foundation gives it (an Annex B stream, or
/// an `avcC` / `hvcC` record), in order.
pub fn parameter_sets(codec: Codec, header: &[u8]) -> Vec<&[u8]> {
    let units = if header.starts_with(&[0, 0, 1]) || header.starts_with(&START) {
        units(header)
    } else {
        record(codec, header).unwrap_or_default()
    };
    units.into_iter().filter(|u| is_parameter_set(codec, u)).collect()
}

fn record(codec: Codec, data: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out = Vec::new();
    let mut at;
    match codec {
        Codec::H264 => {
            at = 6;
            for _ in 0..data.get(5)? & 0x1F {
                out.push(sized(data, &mut at)?);
            }
            let pps = *data.get(at)?;
            at += 1;
            for _ in 0..pps {
                out.push(sized(data, &mut at)?);
            }
        }
        Codec::Hevc => {
            at = 23;
            for _ in 0..*data.get(22)? {
                let n = u16::from_be_bytes([*data.get(at + 1)?, *data.get(at + 2)?]);
                at += 3;
                for _ in 0..n {
                    out.push(sized(data, &mut at)?);
                }
            }
        }
    }
    Some(out)
}

/// The 16-bit length-prefixed unit at `at`, moving past it.
fn sized<'a>(data: &'a [u8], at: &mut usize) -> Option<&'a [u8]> {
    let len = u16::from_be_bytes([*data.get(*at)?, *data.get(*at + 1)?]) as usize;
    let unit = data.get(*at + 2..*at + 2 + len)?;
    *at += 2 + len;
    Some(unit)
}

fn kind(codec: Codec, unit: &[u8]) -> u8 {
    match codec {
        Codec::H264 => unit[0] & 0x1F,
        Codec::Hevc => (unit[0] >> 1) & 0x3F,
    }
}

fn is_parameter_set(codec: Codec, unit: &[u8]) -> bool {
    match codec {
        Codec::H264 => matches!(kind(codec, unit), 7 | 8),
        Codec::Hevc => matches!(kind(codec, unit), 32..=34),
    }
}

fn is_delimiter(codec: Codec, unit: &[u8]) -> bool {
    kind(codec, unit) == if codec == Codec::H264 { 9 } else { 35 }
}

/// The access unit `sample` (Annex B) with `sets` as its parameter sets, after its delimiter and
/// before everything else; any it carried are dropped.
pub fn with_sets(codec: Codec, sample: &[u8], sets: &[&[u8]]) -> Vec<u8> {
    let units = units(sample);
    let mut out = Vec::with_capacity(sample.len() + sets.iter().map(|s| s.len() + 4).sum::<usize>());
    let mut put = |u: &[u8]| {
        out.extend_from_slice(&START);
        out.extend_from_slice(u);
    };
    let delimiter = units.first().filter(|u| is_delimiter(codec, u));
    if let Some(d) = delimiter {
        put(d);
    }
    for s in sets {
        put(s);
    }
    for u in units.iter().skip(delimiter.is_some() as usize).filter(|u| !is_parameter_set(codec, u)) {
        put(u);
    }
    out
}

// NAL types no standard gives a meaning (h.264 24–31, hevc 48–63), each standing for one kind of
// parameter set while it's hidden.
const H264_HIDDEN: u8 = 23;
const HEVC_HIDDEN: u8 = 29;

/// `set` disguised as a NAL unit of an unspecified type, so a muxer that takes the parameter sets out
/// of the samples it's given (media foundation's mp4 sink does) leaves it in; [`reveal`] undoes it.
pub fn hide(codec: Codec, set: &[u8]) -> Vec<u8> {
    let mut out = set.to_vec();
    let t = kind(codec, set);
    out[0] = match codec {
        Codec::H264 => (set[0] & 0xE0) | (t + H264_HIDDEN),
        Codec::Hevc => (set[0] & 0x81) | ((t + HEVC_HIDDEN) << 1),
    };
    out
}

/// Turns back, in place, the parameter sets [`hide`] disguised in `sample`, an access unit of NAL
/// units each after its length in `len_size` bytes; says whether there were any.
pub fn reveal(codec: Codec, sample: &mut [u8], len_size: usize) -> bool {
    let mut at = 0;
    let mut any = false;
    while at + len_size < sample.len() {
        let len = sample[at..at + len_size].iter().fold(0usize, |n, &b| n << 8 | b as usize);
        let head = at + len_size;
        if len == 0 || head + len > sample.len() {
            break;
        }
        let t = kind(codec, &sample[head..]);
        match codec {
            Codec::H264 if (7 + H264_HIDDEN..=8 + H264_HIDDEN).contains(&t) => {
                sample[head] = (sample[head] & 0xE0) | (t - H264_HIDDEN);
                any = true;
            }
            Codec::Hevc if (32 + HEVC_HIDDEN..=34 + HEVC_HIDDEN).contains(&t) => {
                sample[head] = (sample[head] & 0x81) | ((t - HEVC_HIDDEN) << 1);
                any = true;
            }
            _ => {}
        }
        at = head + len;
    }
    any
}

/// Whether the access unit `sample` (Annex B) starts the stream anew: an IDR picture, after which
/// nothing refers to what came before and new parameter sets may take over.
pub fn starts_anew(codec: Codec, sample: &[u8]) -> bool {
    units(sample).iter().any(|u| match codec {
        Codec::H264 => kind(codec, u) == 5,
        Codec::Hevc => matches!(kind(codec, u), 16..=20),
    })
}

/// What a stream's sequence parameter set asks of the encoder that would continue it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    /// `profile_idc`, which is also media foundation's profile number for both codecs.
    pub profile: u32,
}

/// The setup of the stream whose parameter sets `header` holds, when it's one an encoder can
/// match: 8-bit 4:2:0, progressive, in a profile windows' encoders write (h.264 baseline, main or
/// high; hevc main).
pub fn setup(codec: Codec, header: &[u8]) -> Option<Setup> {
    let sps = parameter_sets(codec, header).into_iter().find(|u| kind(codec, u) == if codec == Codec::H264 { 7 } else { 33 })?;
    let raw = unescape(sps);
    match codec {
        Codec::H264 => h264(&raw),
        Codec::Hevc => hevc(&raw),
    }
}

/// `unit` without its emulation prevention bytes.
fn unescape(unit: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(unit.len());
    let mut zeros = 0;
    for &b in unit {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.at / 8)?;
        let b = (byte >> (7 - self.at % 8)) & 1;
        self.at += 1;
        Some(b as u32)
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0, |v, _| Some(v << 1 | self.bit()?))
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.at += n;
        (self.at <= self.data.len() * 8).then_some(())
    }

    /// An exp-Golomb code.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u64 << zeros) as u32 - 1 + self.bits(zeros)?)
    }
}

fn h264(sps: &[u8]) -> Option<Setup> {
    let mut b = Bits { data: sps, at: 8 };
    let profile = b.bits(8)?;
    b.skip(16)?;
    b.ue()?;
    let (mut chroma, mut luma_depth, mut chroma_depth) = (1, 0, 0);
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        chroma = b.ue()?;
        if chroma == 3 {
            b.bit()?;
        }
        luma_depth = b.ue()?;
        chroma_depth = b.ue()?;
        b.bit()?;
        if b.bit()? == 1 {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if b.bit()? == 1 {
                    scaling_list(&mut b, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    b.ue()?;
    match b.ue()? {
        0 => {
            b.ue()?;
        }
        1 => {
            b.bit()?;
            se(&mut b)?;
            se(&mut b)?;
            for _ in 0..b.ue()? {
                se(&mut b)?;
            }
        }
        _ => {}
    }
    b.ue()?;
    b.bit()?;
    b.ue()?;
    b.ue()?;
    let progressive = b.bit()? == 1;
    let fits = matches!(profile, 66 | 77 | 100) && chroma == 1 && luma_depth == 0 && chroma_depth == 0 && progressive;
    fits.then_some(Setup { profile })
}

fn se(b: &mut Bits) -> Option<i32> {
    let v = b.ue()?;
    Some(if v & 1 == 1 { v.div_ceil(2) as i32 } else { -((v / 2) as i32) })
}

fn scaling_list(b: &mut Bits, size: usize) -> Option<()> {
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..size {
        if next != 0 {
            next = (last + se(b)? + 256) % 256;
        }
        last = if next == 0 { last } else { next };
    }
    Some(())
}

fn hevc(sps: &[u8]) -> Option<Setup> {
    let mut b = Bits { data: sps, at: 16 };
    b.bits(4)?;
    let sub_layers = b.bits(3)?;
    b.bit()?;
    b.bits(3)?;
    let profile = b.bits(5)?;
    b.skip(32 + 48 + 8)?;
    let mut present = Vec::new();
    for _ in 0..sub_layers {
        present.push((b.bit()?, b.bit()?));
    }
    if sub_layers > 0 {
        b.skip(2 * (8 - sub_layers as usize))?;
    }
    for (profile, level) in present {
        b.skip(88 * profile as usize + 8 * level as usize)?;
    }
    b.ue()?;
    let chroma = b.ue()?;
    if chroma == 3 {
        b.bit()?;
    }
    b.ue()?;
    b.ue()?;
    if b.bit()? == 1 {
        for _ in 0..4 {
            b.ue()?;
        }
    }
    let (luma_depth, chroma_depth) = (b.ue()?, b.ue()?);
    (profile == 1 && chroma == 1 && luma_depth == 0 && chroma_depth == 0).then_some(Setup { profile })
}

#[cfg(test)]
mod tests {
    use super::*;

    // x264's sps and pps for 64×64 high (8-bit, then 10-bit high 10), nvenc's hevc vps, sps and
    // pps for 1920×1080 main, and its sps for main 10.
    const H264_SPS: [u8; 24] = [
        0x67, 0x64, 0x00, 0x0a, 0xac, 0xb2, 0x08, 0x4d, 0x80, 0x88, 0x00, 0x00, 0x03, 0x00, 0x08, 0x00, 0x00, 0x03, 0x01, 0xe0, 0x78, 0x91,
        0x32, 0x40,
    ];
    const H264_PPS: [u8; 6] = [0x68, 0xeb, 0xc3, 0xcb, 0x22, 0xc0];
    const H264_10_SPS: [u8; 24] = [
        0x67, 0x6e, 0x00, 0x0a, 0xa6, 0xcd, 0x94, 0x42, 0x6c, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40, 0x00, 0x00, 0x0f, 0x03, 0xc4, 0x89,
        0x65, 0x80,
    ];
    const HEVC_VPS: [u8; 24] = [
        0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x78, 0x97,
        0x02, 0x40,
    ];
    const HEVC_SPS: [u8; 58] = [
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x78, 0xa0, 0x03, 0xc0, 0x80,
        0x11, 0x07, 0xcb, 0x96, 0x5d, 0x29, 0x08, 0x46, 0x45, 0xfd, 0x0c, 0x04, 0x04, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00,
        0x79, 0x80, 0x17, 0x79, 0x44, 0x00, 0x02, 0xdc, 0x6c, 0x00, 0x00, 0xf4, 0x24, 0x04,
    ];
    const HEVC_PPS: [u8; 7] = [0x44, 0x01, 0xc1, 0xad, 0xf0, 0x33, 0x24];
    const HEVC_10_SPS: [u8; 58] = [
        0x42, 0x01, 0x01, 0x02, 0x20, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x78, 0xa0, 0x03, 0xc0, 0x80,
        0x11, 0x07, 0xca, 0xd9, 0x65, 0xd2, 0x90, 0x84, 0x64, 0x5f, 0xd0, 0xc0, 0x40, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40, 0x00, 0x00, 0x07,
        0x98, 0x01, 0x77, 0x94, 0x40, 0x00, 0x2d, 0xc6, 0xc0, 0x00, 0x0f, 0x42, 0x40, 0x40,
    ];

    fn annex_b(units: &[&[u8]]) -> Vec<u8> {
        units.iter().flat_map(|u| START.iter().chain(u.iter()).copied()).collect()
    }

    #[test]
    fn units_split_on_three_and_four_byte_start_codes() {
        let data = [0, 0, 0, 1, 9, 0xF0, 0, 0, 1, 0x65, 1, 2, 0, 0, 0, 1, 0x41, 0, 0, 3, 1];
        assert_eq!(units(&data), [&[9, 0xF0][..], &[0x65, 1, 2], &[0x41, 0, 0, 3, 1]]);
        assert!(units(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn parameter_sets_come_from_annex_b_or_a_configuration_record() {
        let header = annex_b(&[&H264_SPS, &H264_PPS]);
        assert_eq!(parameter_sets(Codec::H264, &header), [&H264_SPS[..], &H264_PPS]);
        let mut avcc = vec![1, 0x64, 0, 0x0a, 0xFF, 0xE1];
        avcc.extend_from_slice(&(H264_SPS.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&H264_SPS);
        avcc.push(1);
        avcc.extend_from_slice(&(H264_PPS.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&H264_PPS);
        assert_eq!(parameter_sets(Codec::H264, &avcc), [&H264_SPS[..], &H264_PPS]);

        let mut hvcc = vec![0u8; 22];
        hvcc[0] = 1;
        hvcc.push(3);
        for (kind, set) in [(32u8, &HEVC_VPS[..]), (33, &HEVC_SPS), (34, &HEVC_PPS)] {
            hvcc.extend_from_slice(&[0x80 | kind, 0, 1]);
            hvcc.extend_from_slice(&(set.len() as u16).to_be_bytes());
            hvcc.extend_from_slice(set);
        }
        assert_eq!(parameter_sets(Codec::Hevc, &hvcc), [&HEVC_VPS[..], &HEVC_SPS, &HEVC_PPS]);
        assert!(parameter_sets(Codec::Hevc, &hvcc[..30]).is_empty(), "a cut-off record has none");
    }

    #[test]
    fn the_setup_takes_8_bit_420_progressive_in_known_profiles() {
        assert_eq!(setup(Codec::H264, &annex_b(&[&H264_SPS, &H264_PPS])), Some(Setup { profile: 100 }));
        assert_eq!(setup(Codec::Hevc, &annex_b(&[&HEVC_VPS, &HEVC_SPS, &HEVC_PPS])), Some(Setup { profile: 1 }));
        assert_eq!(setup(Codec::H264, &annex_b(&[&H264_10_SPS])), None, "high 10");
        let mut ten_bit = H264_10_SPS;
        ten_bit[1] = 100;
        assert_eq!(setup(Codec::H264, &annex_b(&[&ten_bit])), None, "10-bit, whatever the profile says");
        let mut high444 = H264_SPS;
        high444[1] = 244;
        assert_eq!(setup(Codec::H264, &annex_b(&[&high444])), None, "a profile windows doesn't encode");
        assert_eq!(setup(Codec::Hevc, &annex_b(&[&HEVC_10_SPS])), None, "main 10");
        let mut ten_bit = HEVC_10_SPS;
        ten_bit[3] = 0x01;
        assert_eq!(setup(Codec::Hevc, &annex_b(&[&ten_bit])), None, "10-bit, whatever the profile says");
        assert_eq!(setup(Codec::H264, &annex_b(&[&H264_PPS])), None, "no sps");
    }

    #[test]
    fn an_access_unit_takes_new_parameter_sets_after_its_delimiter() {
        let old_sps = [0x67, 1];
        let au = annex_b(&[&[9, 0xF0], &old_sps, &[0x68, 2], &[6, 5], &[0x65, 7, 7]]);
        let out = with_sets(Codec::H264, &au, &[&H264_SPS, &H264_PPS]);
        assert_eq!(out, annex_b(&[&[9, 0xF0], &H264_SPS, &H264_PPS, &[6, 5], &[0x65, 7, 7]]));
        let bare = annex_b(&[&[0x26, 1, 0xAF], &[0x02, 1, 5]]);
        assert_eq!(with_sets(Codec::Hevc, &bare, &[&HEVC_VPS]), annex_b(&[&HEVC_VPS, &[0x26, 1, 0xAF], &[0x02, 1, 5]]));
    }

    /// `units` each after its length in 4 bytes, as an mp4 stores them.
    fn length_prefixed(units: &[&[u8]]) -> Vec<u8> {
        units.iter().flat_map(|u| (u.len() as u32).to_be_bytes().into_iter().chain(u.iter().copied())).collect()
    }

    #[test]
    fn hidden_parameter_sets_come_back_as_they_were() {
        for (codec, sets, slice) in [
            (Codec::H264, vec![&H264_SPS[..], &H264_PPS], &[0x65u8, 0x88, 0x84][..]),
            (Codec::Hevc, vec![&HEVC_VPS[..], &HEVC_SPS, &HEVC_PPS], &[0x26, 0x01, 0xAF][..]),
        ] {
            let hidden: Vec<Vec<u8>> = sets.iter().map(|s| hide(codec, s)).collect();
            for (h, s) in hidden.iter().zip(&sets) {
                assert!(!is_parameter_set(codec, h) && h[1..] == s[1..], "{codec:?}: only the type changes");
            }
            let mut units: Vec<&[u8]> = hidden.iter().map(Vec::as_slice).collect();
            units.push(slice);
            let mut sample = length_prefixed(&units);
            assert!(reveal(codec, &mut sample, 4));
            let mut want = sets.clone();
            want.push(slice);
            assert_eq!(sample, length_prefixed(&want));
            assert!(!reveal(codec, &mut sample, 4), "{codec:?}: nothing hidden twice");
        }
        let mut cut_off = length_prefixed(&[&hide(Codec::H264, &H264_SPS)]);
        cut_off.truncate(10);
        assert!(!reveal(Codec::H264, &mut cut_off, 4), "a unit running past the end is left alone");
    }

    #[test]
    fn only_idr_pictures_start_the_stream_anew() {
        assert!(starts_anew(Codec::H264, &annex_b(&[&[9, 0xF0], &H264_SPS, &[0x65, 0x88]])));
        assert!(!starts_anew(Codec::H264, &annex_b(&[&[0x41, 0x9A]])), "a p slice");
        assert!(!starts_anew(Codec::H264, &annex_b(&[&[6, 6, 1, 0x80], &[0x41, 0x9A]])), "an i slice after a recovery point");
        assert!(starts_anew(Codec::Hevc, &annex_b(&[&[0x26, 0x01, 0xAF]])), "idr_w_radl");
        assert!(!starts_anew(Codec::Hevc, &annex_b(&[&[0x2A, 0x01, 0xAF]])), "cra");
        assert!(!starts_anew(Codec::Hevc, &annex_b(&[&[0x02, 0x01, 0xD0]])), "trail_r");
    }
}
