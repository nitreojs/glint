//! Afterglow's replay is already encoded. Write the MP4 directly so saving never enters
//! Media Foundation's sink-writer finalization wait.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use containr::mp4::{AudioCodec, MuxOptions, Muxer, PacketIn, SampleEntrySpec, TrackSpec, VisualCodec};
use containr::{ContainerColour, EditEntry, EditList, TrackKind};
use glint_core::clip::nal::parameter_sets;
use glint_core::record::Codec;
use glint_core::record::mix::RATE;
use glint_core::record::pipeline::{Packet, Stream};
use glint_core::record::replay::Clip;
use memchr::memmem::Finder;

const TIMESCALE: u32 = 10_000_000;

pub(super) fn write(mut clip: Clip, path: &Path) -> Result<(), String> {
    clip.packets.sort_unstable_by_key(|(_, packet)| packet.dts);
    let mut bounds = vec![None::<(&Packet, &Packet, i64)>; clip.streams.len()];
    for (index, packet) in &clip.packets {
        let end = packet.pts + packet.duration.max(1);
        let entry = bounds[*index].get_or_insert((packet, packet, end));
        entry.1 = packet;
        entry.2 = entry.2.max(end);
    }
    let file = File::options().read(true).write(true).truncate(true).open(path).map_err(|e| e.to_string())?;
    let options = MuxOptions { movie_timescale: TIMESCALE, ..MuxOptions::default() };
    let mut muxer = Muxer::new(file, options).map_err(|e| e.to_string())?;
    let mut tracks = Vec::with_capacity(clip.streams.len());
    for (stream, bounds) in clip.streams.into_iter().zip(bounds) {
        let Some((first, last, end)) = bounds else {
            tracks.push(None);
            continue;
        };
        let codec = match &stream {
            Stream::Video { config, .. } => Some(config.codec),
            Stream::Audio { .. } => None,
        };
        let mut spec = track_spec(stream, &first.data)?;
        let timescale = spec.timescale;
        let shift = first.dts - clip.start;
        if shift != 0 {
            let mut entries = Vec::with_capacity(2);
            if shift > 0 {
                entries.push(EditEntry {
                    segment_duration: shift as u64,
                    media_time: EditEntry::EMPTY_MEDIA_TIME,
                    media_rate: EditEntry::NORMAL_RATE,
                });
            }
            entries.push(EditEntry {
                segment_duration: (end - clip.start.max(first.dts)).max(1) as u64,
                media_time: ticks((-shift).max(0), timescale),
                media_rate: EditEntry::NORMAL_RATE,
            });
            spec.edit_list = EditList { movie_timescale: TIMESCALE, entries };
        }
        let id = muxer.add_track(spec).map_err(|e| e.to_string())?;
        tracks.push(Some((id, codec, first.dts, last.dts, timescale)));
    }

    let start_code = Finder::new(b"\x00\x00\x01");
    let mut converted = Vec::new();
    for (index, packet) in clip.packets {
        let (id, codec, first, last, timescale) = tracks[index].expect("a packet implies a track");
        let data = match codec {
            Some(codec) => sample(&start_code, codec, &packet.data, &mut converted)?,
            None => &packet.data,
        };
        let packet = PacketIn {
            data,
            pts: ticks(packet.pts - first, timescale),
            dts: ticks(packet.dts - first, timescale),
            duration: (packet.dts == last).then(|| ticks(packet.duration, timescale).max(1) as u64),
            sync: codec.is_none() || packet.key,
        };
        muxer.write_packet(id, &packet).map_err(|e| e.to_string())?;
    }
    muxer.finish().map_err(|e| e.to_string())?.flush().map_err(|e| e.to_string())
}

fn ticks(hns: i64, timescale: u32) -> i64 {
    if timescale == TIMESCALE {
        return hns;
    }

    (hns * i64::from(timescale) + i64::from(TIMESCALE / 2)).div_euclid(i64::from(TIMESCALE))
}

fn track_spec(stream: Stream, first_packet: &[u8]) -> Result<TrackSpec, String> {
    match stream {
        Stream::Video { config, headers, matrix } => {
            let (codec, configuration) = video_config(config.codec, &headers, first_packet)?;
            let width = u16::try_from(config.width).map_err(|_| "video width exceeds the MP4 limit")?;
            let height = u16::try_from(config.height).map_err(|_| "video height exceeds the MP4 limit")?;
            let entry = SampleEntrySpec::Visual { codec, config: configuration, width, height };
            let mut spec = TrackSpec::new(TrackKind::Video, TIMESCALE, entry);
            spec.display_width = u32::from(width) << 16;
            spec.display_height = u32::from(height) << 16;
            spec.colour = Some(ContainerColour {
                primaries: Some(1),
                transfer: Some(1),
                matrix: Some(matrix.code() as u8),
                full_range: Some(false),
            });

            Ok(spec)
        }
        Stream::Audio { name, headers } => {
            let [asc_a, asc_b] = headers[..] else { return Err("expected an AAC-LC audio configuration".into()) };
            // ES descriptor, decoder configuration (192 kbit/s), AudioSpecificConfig and SLConfig.
            let config = vec![
                0, 0, 0, 0, 3, 25, 0, 1, 0, 4, 17, 0x40, 0x15, 0, 0, 0, 0, 2, 0xEE, 0, 0, 2, 0xEE, 0,
                5, 2, asc_a, asc_b, 6, 1, 2,
            ];
            let entry = SampleEntrySpec::Audio { codec: AudioCodec::Aac, config, channels: 2, sample_rate: RATE };
            let mut spec = TrackSpec::new(TrackKind::Audio, RATE, entry);
            spec.handler_name = name;

            Ok(spec)
        }
    }
}

fn nal_type(codec: Codec, unit: &[u8]) -> u8 {
    match codec {
        Codec::H264 => unit[0] & 31,
        Codec::Hevc => (unit[0] >> 1) & 63,
    }
}

fn video_config(codec: Codec, headers: &[u8], first_packet: &[u8]) -> Result<(VisualCodec, Vec<u8>), String> {
    let mut sets = parameter_sets(codec, headers);
    if sets.is_empty() {
        sets = parameter_sets(codec, first_packet);
    }
    let get = |kind| sets.iter().copied().find(|unit| nal_type(codec, unit) == kind);
    let mut config = Vec::new();
    let append = |set: &[u8], config: &mut Vec<u8>| -> Result<(), String> {
        let len = u16::try_from(set.len()).map_err(|_| "video parameter set exceeds the MP4 limit")?;
        config.extend_from_slice(&len.to_be_bytes());
        config.extend_from_slice(set);
        Ok(())
    };
    match codec {
        Codec::H264 => {
            let sps = get(7).ok_or("missing AVC sequence parameter set")?;
            let pps = get(8).ok_or("missing AVC picture parameter set")?;
            let [_, profile, compatibility, level] = *sps.first_chunk().ok_or("short AVC sequence parameter set")?;
            config.extend([1, profile, compatibility, level, 0xFF, 0xE1]);
            append(sps, &mut config)?;
            config.push(1);
            append(pps, &mut config)?;
            if matches!(profile, 100 | 110 | 122 | 144) {
                // Afterglow's encoders produce 8-bit 4:2:0.
                config.extend([0xFD, 0xF8, 0xF8, 0]);
            }

            Ok((VisualCodec::Avc, config))
        }
        Codec::Hevc => {
            let sps = get(33).ok_or("missing HEVC sequence parameter set")?;
            let mut rbsp = [0; 15];
            let mut written = 0;
            let mut zeros = 0;
            for &byte in sps {
                if zeros >= 2 && byte == 3 {
                    zeros = 0;
                    continue;
                }
                rbsp[written] = byte;
                written += 1;
                if written == rbsp.len() {
                    break;
                }
                zeros = if byte == 0 { zeros + 1 } else { 0 };
            }
            if written != rbsp.len() {
                return Err("short HEVC sequence parameter set".into());
            }
            let layers = ((rbsp[2] >> 1) & 7) + 1;
            let nested = rbsp[2] & 1;
            config.push(1);
            config.extend_from_slice(&rbsp[3..]);
            config.extend([0xF0, 0, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0, (layers << 3) | (nested << 2) | 3, 3]);
            for kind in 32..=34 {
                let set = get(kind).ok_or("missing HEVC parameter set")?;
                config.extend([0x80 | kind, 0, 1]);
                append(set, &mut config)?;
            }

            Ok((VisualCodec::Hevc, config))
        }
    }
}

fn sample<'a>(finder: &Finder<'_>, codec: Codec, data: &[u8], output: &'a mut Vec<u8>) -> Result<&'a [u8], String> {
    let mut start = finder.find(data).filter(|&at| at <= 1).ok_or("expected an Annex B video packet")? + 3;
    output.clear();
    loop {
        let rest = &data[start..];
        let next = finder.find(rest);
        let end = next.map_or(rest.len(), |at| rest[..at].iter().rposition(|&b| b != 0).map_or(0, |at| at + 1));
        let unit = &rest[..end];
        if !unit.is_empty()
            && !matches!((codec, nal_type(codec, unit)), (Codec::H264, 7 | 8) | (Codec::Hevc, 32..=34))
        {
            output.extend_from_slice(&(unit.len() as u32).to_be_bytes());
            output.extend_from_slice(unit);
        }
        let Some(next) = next else { break };
        start += next + 3;
    }
    if output.is_empty() {
        return Err("video packet has no sample data".into());
    }

    Ok(output)
}
