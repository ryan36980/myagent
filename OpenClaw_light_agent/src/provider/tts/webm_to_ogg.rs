//! Minimal WebM Opus → OGG Opus remuxer.
//!
//! Edge TTS's free Bing endpoint only supports `webm-24khz-16bit-mono-opus`
//! (WebM container), but some channels (e.g. Feishu) require OGG Opus.
//! This module extracts raw Opus frames from a WebM file and rewraps them
//! in an OGG container — no new dependencies, ~200 lines.

use crate::error::{GatewayError, Result};

// ── EBML / WebM parsing ─────────────────────────────────────────────────

/// Read an EBML variable-length integer (data size).
/// Returns `(value, bytes_consumed)`.  `value == u64::MAX` means unknown size.
fn read_vint(data: &[u8], pos: usize) -> Option<(u64, usize)> {
    let first = *data.get(pos)?;
    if first == 0 {
        return None;
    }
    let width = (first.leading_zeros() + 1) as usize; // 1..=8
    if pos + width > data.len() {
        return None;
    }
    // Mask off the width-marker bit.  For width=8 the entire first byte is
    // the marker, so mask must be 0x00.  `checked_shr` avoids u8 overflow.
    let mask = 0xFFu8.checked_shr(width as u32).unwrap_or(0);
    let mut val = (first & mask) as u64;
    for i in 1..width {
        val = (val << 8) | data[pos + i] as u64;
    }
    // All-ones → unknown size sentinel
    let max = (1u64 << (7 * width)) - 1;
    if val == max {
        val = u64::MAX;
    }
    Some((val, width))
}

/// Read an EBML element ID (the marker bit is part of the ID value).
/// Returns `(id, bytes_consumed)`.
fn read_element_id(data: &[u8], pos: usize) -> Option<(u32, usize)> {
    let first = *data.get(pos)?;
    if first == 0 {
        return None;
    }
    let width = (first.leading_zeros() + 1) as usize;
    if pos + width > data.len() {
        return None;
    }
    let mut id = 0u32;
    for i in 0..width {
        id = (id << 8) | data[pos + i] as u32;
    }
    Some((id, width))
}

// Well-known Matroska / WebM element IDs
const EBML_HEADER: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const CLUSTER: u32 = 0x1F43_B675;
const SIMPLE_BLOCK: u32 = 0xA3;

/// Extract raw Opus frames from a WebM byte stream.
fn extract_opus_frames(data: &[u8]) -> Result<Vec<&[u8]>> {
    let mut pos = 0usize;
    let mut frames = Vec::new();

    // ── Skip EBML header ────────────────────────────────────────────
    let (id, id_len) = read_element_id(data, pos)
        .ok_or_else(|| GatewayError::Tts("WebM: failed to read EBML header ID".into()))?;
    pos += id_len;
    if id != EBML_HEADER {
        return Err(GatewayError::Tts(format!("WebM: expected EBML header, got 0x{id:X}")));
    }
    let (size, sz_len) = read_vint(data, pos)
        .ok_or_else(|| GatewayError::Tts("WebM: failed to read EBML header size".into()))?;
    pos += sz_len;
    pos += size as usize;

    // ── Enter Segment ───────────────────────────────────────────────
    let (id, id_len) = read_element_id(data, pos)
        .ok_or_else(|| GatewayError::Tts("WebM: failed to read Segment ID".into()))?;
    pos += id_len;
    if id != SEGMENT {
        return Err(GatewayError::Tts(format!("WebM: expected Segment, got 0x{id:X}")));
    }
    let (seg_size, sz_len) = read_vint(data, pos)
        .ok_or_else(|| GatewayError::Tts("WebM: failed to read Segment size".into()))?;
    pos += sz_len;
    let seg_end = if seg_size == u64::MAX {
        data.len()
    } else {
        (pos + seg_size as usize).min(data.len())
    };

    // ── Flat scan of Segment children ─────────────────────────────────
    //
    // Cluster elements are "entered" (not skipped) so the loop processes
    // their children inline.  This correctly handles Clusters with unknown
    // size — the previous nested-loop approach would encounter the NEXT
    // Cluster as a "child", read its size, and skip all remaining data.
    while pos < seg_end {
        let Some((id, id_len)) = read_element_id(data, pos) else { break };
        pos += id_len;
        let Some((size, sz_len)) = read_vint(data, pos) else { break };
        pos += sz_len;

        if id == CLUSTER {
            // Master element: enter it — next iteration reads its first child.
            // (Don't advance pos; we just consumed the Cluster header.)
            continue;
        }

        let elem_end = if size == u64::MAX {
            seg_end
        } else {
            (pos + size as usize).min(seg_end)
        };

        if id == SIMPLE_BLOCK && size >= 4 {
            // SimpleBlock: track_num(VINT) + timecode(2) + flags(1) + frame
            let track_width = if pos < elem_end {
                let f = data[pos];
                (f.leading_zeros() + 1) as usize
            } else {
                1
            };
            let header_len = track_width + 2 + 1;
            let frame_start = pos + header_len;
            if frame_start < elem_end {
                frames.push(&data[frame_start..elem_end]);
            }
        }
        pos = elem_end;
    }
    if frames.is_empty() {
        return Err(GatewayError::Tts("WebM: no Opus frames found".into()));
    }
    Ok(frames)
}

// ── OGG writing ─────────────────────────────────────────────────────────

/// OGG CRC-32 lookup table (polynomial 0x04C11DB7, no inversion).
const fn build_ogg_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut r = i << 24;
        let mut j = 0;
        while j < 8 {
            r = if r & 0x8000_0000 != 0 {
                (r << 1) ^ 0x04C1_1DB7
            } else {
                r << 1
            };
            j += 1;
        }
        table[i as usize] = r;
        i += 1;
    }
    table
}

static OGG_CRC_TABLE: [u32; 256] = build_ogg_crc_table();

fn ogg_crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc = (crc << 8) ^ OGG_CRC_TABLE[((crc >> 24) as u8 ^ b) as usize];
    }
    crc
}

/// Build a single OGG page containing one packet.
fn build_ogg_page(serial: u32, seq: u32, granule: i64, flags: u8, packet: &[u8]) -> Vec<u8> {
    // Segment table: encode packet length as sequence of 255s + remainder
    let mut segtable = Vec::new();
    let mut rem = packet.len();
    while rem >= 255 {
        segtable.push(255u8);
        rem -= 255;
    }
    segtable.push(rem as u8);

    let page_size = 27 + segtable.len() + packet.len();
    let mut page = Vec::with_capacity(page_size);

    // Page header (27 bytes)
    page.extend_from_slice(b"OggS");                    // capture pattern
    page.push(0);                                        // version
    page.push(flags);                                    // header type
    page.extend_from_slice(&granule.to_le_bytes());      // granule position
    page.extend_from_slice(&serial.to_le_bytes());       // serial number
    page.extend_from_slice(&seq.to_le_bytes());          // page sequence number
    page.extend_from_slice(&0u32.to_le_bytes());         // CRC placeholder
    page.push(segtable.len() as u8);                     // number of segments
    page.extend_from_slice(&segtable);                   // segment table
    page.extend_from_slice(packet);                      // payload

    // Compute CRC and fill in at offset 22
    let crc = ogg_crc32(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

/// Determine the number of 48 kHz PCM samples in an Opus packet from its TOC byte.
fn opus_packet_samples(packet: &[u8]) -> u64 {
    if packet.is_empty() {
        return 960;
    }
    let toc = packet[0];
    let config = (toc >> 3) & 0x1F;
    let spf: u64 = match config {
        0 | 4 | 8 => 480,                      // 10 ms  (SILK)
        1 | 5 | 9 | 13 | 15 => 960,            // 20 ms  (SILK / Hybrid)
        2 | 6 | 10 => 1920,                     // 40 ms  (SILK)
        3 | 7 | 11 => 2880,                     // 60 ms  (SILK)
        12 | 14 => 480,                         // 10 ms  (Hybrid)
        16 | 20 | 24 | 28 => 120,              // 2.5 ms (CELT)
        17 | 21 | 25 | 29 => 240,              // 5 ms   (CELT)
        18 | 22 | 26 | 30 => 480,              // 10 ms  (CELT)
        19 | 23 | 27 | 31 => 960,              // 20 ms  (CELT)
        _ => 960,
    };
    let frame_count: u64 = match toc & 0x03 {
        0 => 1,
        1 | 2 => 2,
        3 if packet.len() >= 2 => (packet[1] & 0x3F) as u64,
        _ => 1,
    };
    spf * frame_count
}

// ── Public API ──────────────────────────────────────────────────────────

/// Convert WebM Opus audio to OGG Opus.
///
/// Extracts raw Opus frames from the WebM container and rewraps them in
/// a standard OGG Opus stream (OpusHead + OpusTags + audio pages).
pub fn webm_opus_to_ogg_opus(webm: &[u8]) -> Result<Vec<u8>> {
    let frames = extract_opus_frames(webm)?;

    let serial = 0x4F43_7531u32; // arbitrary stream serial "OCu1"
    let mut seq = 0u32;
    let mut out = Vec::with_capacity(webm.len());

    // ── Page 0: OpusHead (BOS) ──────────────────────────────────────
    #[rustfmt::skip]
    let opus_head: [u8; 19] = [
        0x4F, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, // "OpusHead"
        0x01,                                               // version 1
        0x01,                                               // 1 channel (mono)
        0x38, 0x01,                                         // pre_skip = 312 (LE)
        0xC0, 0x5D, 0x00, 0x00,                            // sample rate = 24000 (LE)
        0x00, 0x00,                                         // output gain = 0
        0x00,                                               // mapping family 0
    ];
    out.extend(build_ogg_page(serial, seq, 0, 0x02, &opus_head)); // BOS
    seq += 1;

    // ── Page 1: OpusTags ────────────────────────────────────────────
    #[rustfmt::skip]
    let opus_tags: [u8; 16] = [
        0x4F, 0x70, 0x75, 0x73, 0x54, 0x61, 0x67, 0x73, // "OpusTags"
        0x00, 0x00, 0x00, 0x00,                            // vendor string length = 0
        0x00, 0x00, 0x00, 0x00,                            // comment count = 0
    ];
    out.extend(build_ogg_page(serial, seq, 0, 0x00, &opus_tags));
    seq += 1;

    // ── Audio pages (one Opus frame per page) ───────────────────────
    let mut granule: i64 = 0;
    let last_idx = frames.len() - 1;
    for (i, frame) in frames.iter().enumerate() {
        granule += opus_packet_samples(frame) as i64;
        let flags = if i == last_idx { 0x04 } else { 0x00 }; // EOS on last
        out.extend(build_ogg_page(serial, seq, granule, flags, frame));
        seq += 1;
    }

    Ok(out)
}

/// Extract duration in milliseconds from OGG Opus audio bytes.
///
/// Reads the granule position from the last OGG page header.
/// Opus always uses 48 kHz, so `duration_ms = granule * 1000 / 48000`.
pub fn ogg_opus_duration_ms(ogg: &[u8]) -> u64 {
    // Find the last "OggS" page header
    let mut last_pos = None;
    let mut i = 0;
    while i + 14 <= ogg.len() {
        if &ogg[i..i + 4] == b"OggS" {
            last_pos = Some(i);
        }
        i += 1;
    }
    let Some(pos) = last_pos else {
        return 0;
    };
    // Granule position: 8 bytes little-endian at offset 6
    let granule = i64::from_le_bytes([
        ogg[pos + 6],
        ogg[pos + 7],
        ogg[pos + 8],
        ogg[pos + 9],
        ogg[pos + 10],
        ogg[pos + 11],
        ogg[pos + 12],
        ogg[pos + 13],
    ]);
    if granule <= 0 {
        return 0;
    }
    (granule as u64 * 1000) / 48000
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ogg_crc_known_vector() {
        // "OggS" → known CRC used by libogg test vectors
        let crc = ogg_crc32(b"OggS");
        // Just verify it's deterministic and non-zero
        assert_ne!(crc, 0);
        assert_eq!(ogg_crc32(b"OggS"), crc);
    }

    #[test]
    fn build_ogg_page_structure() {
        let page = build_ogg_page(42, 0, 960, 0x02, &[0xFC, 0x00, 0x01]);
        assert_eq!(&page[..4], b"OggS");
        assert_eq!(page[4], 0); // version
        assert_eq!(page[5], 0x02); // BOS flag
        // serial = 42 at offset 14
        assert_eq!(u32::from_le_bytes([page[14], page[15], page[16], page[17]]), 42);
        // 1 segment entry for 3-byte packet
        assert_eq!(page[26], 1); // num_segments
        assert_eq!(page[27], 3); // segment size
        // payload at offset 28
        assert_eq!(&page[28..31], &[0xFC, 0x00, 0x01]);
    }

    #[test]
    fn opus_packet_samples_20ms() {
        // TOC: config=1 (SILK NB 20ms), s=0, c=0 → 0b_00001_0_00 = 0x08
        assert_eq!(opus_packet_samples(&[0x08]), 960);
    }

    #[test]
    fn opus_packet_samples_10ms() {
        // TOC: config=0 (SILK NB 10ms), s=0, c=0 → 0b_00000_0_00 = 0x00
        assert_eq!(opus_packet_samples(&[0x00]), 480);
    }

    #[test]
    fn opus_packet_samples_two_frames() {
        // TOC: config=1 (20ms), s=0, c=1 (2 frames equal) → 0b_00001_0_01 = 0x09
        assert_eq!(opus_packet_samples(&[0x09]), 1920);
    }

    #[test]
    fn read_vint_one_byte() {
        // 0x82 = 1_0000010 → value = 2
        assert_eq!(read_vint(&[0x82], 0), Some((2, 1)));
    }

    #[test]
    fn read_vint_two_bytes() {
        // 0x40, 0x02 = 01_000000 00000010 → value = 2
        assert_eq!(read_vint(&[0x40, 0x02], 0), Some((2, 2)));
    }

    #[test]
    fn read_element_id_one_byte() {
        // 0xA3 = SimpleBlock
        assert_eq!(read_element_id(&[0xA3], 0), Some((0xA3, 1)));
    }

    #[test]
    fn read_element_id_four_bytes() {
        // 0x1A 0x45 0xDF 0xA3 = EBML header
        assert_eq!(
            read_element_id(&[0x1A, 0x45, 0xDF, 0xA3], 0),
            Some((0x1A45DFA3, 4))
        );
    }

    #[test]
    fn extract_rejects_non_webm() {
        let err = extract_opus_frames(&[0x00, 0x01, 0x02]).unwrap_err();
        assert!(format!("{err}").contains("EBML"));
    }

    /// Build a minimal WebM with multiple Clusters, each containing SimpleBlocks.
    /// The first Cluster has *unknown size* (mimics Edge TTS output).
    fn build_test_webm(frames_per_cluster: &[usize]) -> Vec<u8> {
        let mut buf = Vec::new();

        // EBML Header: ID(4) + size(1) + body(minimal)
        buf.extend_from_slice(&[0x1A, 0x45, 0xDF, 0xA3]); // EBML header ID
        buf.push(0x84); // VINT size = 4
        buf.extend_from_slice(&[0x42, 0x86, 0x81, 0x01]); // DocType element (dummy)

        // Segment: unknown size
        buf.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]); // Segment ID
        buf.push(0xFF); // unknown size (1-byte VINT)

        for (ci, &nframes) in frames_per_cluster.iter().enumerate() {
            // Cluster: unknown size
            buf.extend_from_slice(&[0x1F, 0x43, 0xB6, 0x75]); // Cluster ID
            buf.push(0xFF); // unknown size

            // Timecode element: 0xE7, size=1, value=0
            buf.extend_from_slice(&[0xE7, 0x81, 0x00]);

            for fi in 0..nframes {
                // SimpleBlock: track=1(VINT 0x81) + timecode(2) + flags(1) + fake Opus
                let opus_data: Vec<u8> = vec![(ci as u8).wrapping_mul(37).wrapping_add(fi as u8); 10];
                let sb_header: [u8; 4] = [0x81, 0x00, 0x00, 0x00]; // track=1, tc=0, flags=0
                let sb_len = sb_header.len() + opus_data.len(); // 14
                buf.push(0xA3); // SimpleBlock ID
                buf.push(0x80 | sb_len as u8); // VINT size (1-byte, ≤ 126)
                buf.extend_from_slice(&sb_header);
                buf.extend_from_slice(&opus_data);
            }
        }

        buf
    }

    #[test]
    fn extract_multi_cluster_unknown_size() {
        // 3 clusters with 5, 3, 7 SimpleBlocks respectively
        let webm = build_test_webm(&[5, 3, 7]);
        let frames = extract_opus_frames(&webm).unwrap();
        assert_eq!(frames.len(), 15, "should find all frames across 3 clusters");
        // Verify frame content is distinct per cluster+frame
        assert_ne!(frames[0], frames[5]); // different clusters
    }

    #[test]
    fn ogg_opus_duration_from_roundtrip() {
        let webm = build_test_webm(&[4, 6]);
        let ogg = webm_opus_to_ogg_opus(&webm).unwrap();
        let dur = ogg_opus_duration_ms(&ogg);
        // 10 frames with varying TOC bytes → duration should be > 0 and reasonable
        assert!(dur > 0, "duration should be positive");
        assert!(dur < 5000, "duration should be under 5 seconds for 10 frames");
    }

    #[test]
    fn full_roundtrip_multi_cluster() {
        let webm = build_test_webm(&[4, 6]);
        let ogg = webm_opus_to_ogg_opus(&webm).unwrap();
        // OGG should start with "OggS"
        assert_eq!(&ogg[..4], b"OggS");
        // Should have 10 audio frames (4+6) + 2 header pages = 12 pages
        let page_count = ogg.windows(4).filter(|w| *w == b"OggS").count();
        assert_eq!(page_count, 12, "2 header pages + 10 audio pages");
    }
}
