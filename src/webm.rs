//! WebM files written by `MediaRecorder` have no duration in their header (it's not known
//! while recording), so players can't seek in them or show their length. [`set_duration`]
//! writes it into the start of the file once the recording is done.
//!
//! Only the head of the file is needed (the EBML header, the start of the Segment, and its
//! Info element, which come before the first Cluster); the rest of the file stays as it is.

const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_POSITION: u32 = 0x53AC;
const INFO: u32 = 0x1549_A966;
const TIMECODE_SCALE: u32 = 0x2A_D7B1;
const DURATION: u32 = 0x4489;
const CLUSTER: u32 = 0x1F43_B675;

/// An element ID (with its length marker, as IDs are written) and its length in bytes.
fn read_id(b: &[u8], at: usize) -> Option<(u32, usize)> {
    let first = *b.get(at)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 {
        return None;
    }
    let bytes = b.get(at..at + len)?;
    Some((bytes.iter().fold(0u32, |v, x| (v << 8) | *x as u32), len))
}

/// An element size (`None`: unknown, as live recordings write the Segment's) and its length.
fn read_size(b: &[u8], at: usize) -> Option<(Option<u64>, usize)> {
    let first = *b.get(at)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let bytes = b.get(at..at + len)?;
    let mut v = (first as u64) & (0xFF >> len);
    for x in &bytes[1..] {
        v = (v << 8) | *x as u64;
    }
    let all_ones = (1u64 << (7 * len)) - 1;
    Some((if v == all_ones { None } else { Some(v) }, len))
}

/// `value` as a size of exactly `len` bytes, if it fits.
fn write_size(value: u64, len: usize) -> Option<Vec<u8>> {
    if !(1..=8).contains(&len) || value >= (1u64 << (7 * len)) - 1 {
        return None;
    }
    let mut out = vec![0u8; len];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = (value >> (8 * (len - 1 - i))) as u8;
    }
    out[0] |= 0x80 >> (len - 1);
    Some(out)
}

fn read_uint(b: &[u8]) -> u64 {
    b.iter().fold(0, |v, x| (v << 8) | *x as u64)
}

/// An element: where it starts, the length of its ID and size, its data and size.
#[derive(Clone, Copy, Debug)]
struct Element {
    id: u32,
    start: usize,
    size_at: usize,
    size_len: usize,
    data: usize,
    size: Option<u64>,
}

impl Element {
    fn end(&self) -> Option<usize> {
        self.size.map(|s| self.data + s as usize)
    }
}

fn element(b: &[u8], at: usize) -> Option<Element> {
    let (id, id_len) = read_id(b, at)?;
    let (size, size_len) = read_size(b, at + id_len)?;
    Some(Element { id, start: at, size_at: at + id_len, size_len, data: at + id_len + size_len, size })
}

/// The children of an element (as far as they are in `b`).
fn children(b: &[u8], parent: &Element) -> Vec<Element> {
    let mut out = Vec::new();
    let end = parent.end().unwrap_or(b.len()).min(b.len());
    let mut at = parent.data;
    while at < end {
        let Some(e) = element(b, at) else { break };
        out.push(e);
        match e.end() {
            Some(next) => at = next,
            None => break,
        }
    }
    out
}

/// The Segment and its Info element.
fn locate(head: &[u8]) -> Result<(Element, Element), String> {
    let ebml = element(head, 0).filter(|e| e.id == EBML).ok_or("not a WebM file")?;
    let at = ebml.end().ok_or("not a WebM file")?;
    let segment = element(head, at).filter(|e| e.id == SEGMENT).ok_or("no Segment in the WebM file")?;
    let info = children(head, &segment)
        .into_iter()
        .take_while(|e| e.id != CLUSTER)
        .find(|e| e.id == INFO)
        .ok_or("no Info before the first Cluster")?;
    if info.end().is_none_or(|end| end > head.len()) {
        return Err("the head of the file is too short".into());
    }
    Ok((segment, info))
}

/// The duration written in the file, in milliseconds.
pub fn duration_ms(head: &[u8]) -> Option<f64> {
    let (_, info) = locate(head).ok()?;
    let kids = children(head, &info);
    let scale = kids.iter().find(|e| e.id == TIMECODE_SCALE).map_or(1_000_000, |e| read_uint(&head[e.data..e.end().unwrap_or(e.data)]));
    let d = kids.iter().find(|e| e.id == DURATION)?;
    let bytes = &head[d.data..d.end()?];
    let units = match bytes.len() {
        4 => f32::from_be_bytes(bytes.try_into().ok()?) as f64,
        8 => f64::from_be_bytes(bytes.try_into().ok()?),
        _ => return None,
    };
    Some(units * scale as f64 / 1e6)
}

/// The head of a WebM file (at least up to the end of its Info element) with its duration
/// set to `ms` milliseconds. The rest of the file follows unchanged.
pub fn set_duration(head: &[u8], ms: f64) -> Result<Vec<u8>, String> {
    if !(ms.is_finite() && ms >= 0.0) {
        return Err("no duration to write".into());
    }
    let (segment, info) = locate(head)?;
    let kids = children(head, &info);
    let scale =
        kids.iter().find(|e| e.id == TIMECODE_SCALE).map_or(1_000_000, |e| read_uint(&head[e.data..e.end().unwrap_or(e.data)])).max(1);
    let units = ms * 1e6 / scale as f64;
    let mut out = head.to_vec();

    // Already there: overwrite it.
    if let Some(d) = kids.iter().find(|e| e.id == DURATION) {
        let end = d.end().ok_or("bad Duration")?;
        match end - d.data {
            4 => out[d.data..end].copy_from_slice(&(units as f32).to_be_bytes()),
            8 => out[d.data..end].copy_from_slice(&units.to_be_bytes()),
            _ => return Err("bad Duration".into()),
        }
        return Ok(out);
    }

    // Add it at the end of Info, which grows (and everything after it moves).
    let info_size = info.size.ok_or("Info of unknown size")?;
    let mut duration = vec![0x44, 0x89, 0x88];
    duration.extend_from_slice(&units.to_be_bytes());
    let new_size = info_size + duration.len() as u64;
    let size_bytes = write_size(new_size, info.size_len).or_else(|| write_size(new_size, 8)).ok_or("Info too large")?;
    let delta = (duration.len() + size_bytes.len() - info.size_len) as u64;
    let info_end = info.end().ok_or("bad Info")?;
    let mut grown = Vec::with_capacity(out.len() + delta as usize);
    grown.extend_from_slice(&out[..info.size_at]);
    grown.extend_from_slice(&size_bytes);
    grown.extend_from_slice(&out[info.data..info_end]);
    grown.extend_from_slice(&duration);
    grown.extend_from_slice(&out[info_end..]);
    out = grown;

    // A Segment of known size grows too.
    if let Some(size) = segment.size {
        let bytes = write_size(size + delta, segment.size_len).ok_or("Segment size doesn't fit")?;
        out[segment.size_at..segment.data].copy_from_slice(&bytes);
    }
    // A SeekHead before Info points at elements (relative to the Segment's data) that moved.
    let info_rel = (info.start - segment.data) as u64;
    for seek_head in children(head, &segment).into_iter().take_while(|e| e.start < info.start).filter(|e| e.id == SEEK_HEAD) {
        for seek in children(head, &seek_head).into_iter().filter(|e| e.id == SEEK) {
            for pos in children(head, &seek).into_iter().filter(|e| e.id == SEEK_POSITION) {
                let end = pos.end().ok_or("bad SeekPosition")?;
                let len = end - pos.data;
                let value = read_uint(&head[pos.data..end]);
                if value > info_rel {
                    let moved = value + delta;
                    if len < 8 && moved >= 1u64 << (8 * len) {
                        return Err("SeekPosition doesn't fit".into());
                    }
                    for i in 0..len {
                        out[pos.data + i] = (moved >> (8 * (len - 1 - i))) as u8;
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(id: u32, data: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = id.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
        let len = if data.len() < 127 { 1 } else { 4 };
        out.extend(write_size(data.len() as u64, len).unwrap());
        out.extend_from_slice(data);
        out
    }

    /// A head like MediaRecorder's: EBML header, a Segment of unknown size, Info without
    /// Duration, Tracks, then the start of a Cluster.
    fn recorder_head(seek_head: bool, known_segment: Option<u64>) -> Vec<u8> {
        let mut head = el(EBML, &el(0x4282, b"webm"));
        head.extend(SEGMENT.to_be_bytes());
        match known_segment {
            Some(s) => head.extend(write_size(s, 8).unwrap()),
            None => head.extend([0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
        }
        let mut info = el(TIMECODE_SCALE, &[0x0F, 0x42, 0x40]);
        info.extend(el(0x4D80, b"Chrome"));
        let info = el(INFO, &info);
        let tracks = el(0x1654_AE6B, &[0xAE, 0x80]);
        if seek_head {
            // Positions: Info at 20 (just after this SeekHead), Tracks after Info.
            let seek = |id: &[u8], pos: u16| {
                let mut s = el(0x53AB, id);
                s.extend(el(SEEK_POSITION, &pos.to_be_bytes()));
                el(SEEK, &s)
            };
            let mut entries = seek(&[0x15, 0x49, 0xA9, 0x66], 0);
            let info_at = 0;
            entries.extend(seek(&[0x16, 0x54, 0xAE, 0x6B], 0));
            let sh_len = el(SEEK_HEAD, &entries).len() as u16;
            let mut entries = seek(&[0x15, 0x49, 0xA9, 0x66], sh_len + info_at);
            entries.extend(seek(&[0x16, 0x54, 0xAE, 0x6B], sh_len + info.len() as u16));
            head.extend(el(SEEK_HEAD, &entries));
        }
        head.extend(info);
        head.extend(tracks);
        head.extend([0x1F, 0x43, 0xB6, 0x75, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xE7, 0x81, 0x00]);
        head
    }

    #[test]
    fn sizes_round_trip() {
        assert_eq!(read_size(&write_size(5, 1).unwrap(), 0), Some((Some(5), 1)));
        assert_eq!(read_size(&write_size(300, 2).unwrap(), 0), Some((Some(300), 2)));
        assert_eq!(read_size(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF], 0), Some((None, 8)));
        assert!(write_size(127, 1).is_none(), "all ones means unknown");
        assert_eq!(read_id(&[0x1A, 0x45, 0xDF, 0xA3], 0), Some((EBML, 4)));
    }

    #[test]
    fn adds_the_duration() {
        let head = recorder_head(false, None);
        assert_eq!(duration_ms(&head), None);
        let fixed = set_duration(&head, 12_345.0).unwrap();
        assert_eq!(fixed.len(), head.len() + 11);
        assert_eq!(duration_ms(&fixed), Some(12_345.0));
        // The elements after Info are intact.
        assert!(fixed.ends_with(&head[head.len() - 19..]));
        // Writing again overwrites it.
        let again = set_duration(&fixed, 99.5).unwrap();
        assert_eq!(again.len(), fixed.len());
        assert_eq!(duration_ms(&again), Some(99.5));
    }

    #[test]
    fn known_segment_size_and_seek_head_follow() {
        let head = recorder_head(true, Some(1_000_000));
        let fixed = set_duration(&head, 2000.0).unwrap();
        assert_eq!(duration_ms(&fixed), Some(2000.0));
        let (segment, info) = locate(&fixed).unwrap();
        assert_eq!(segment.size, Some(1_000_011));
        // The SeekHead's entry for Tracks points at Tracks again.
        let sh = children(&fixed, &segment).into_iter().find(|e| e.id == SEEK_HEAD).unwrap();
        let positions: Vec<u64> = children(&fixed, &sh)
            .iter()
            .flat_map(|s| children(&fixed, s))
            .filter(|e| e.id == SEEK_POSITION)
            .map(|e| read_uint(&fixed[e.data..e.end().unwrap()]))
            .collect();
        assert_eq!(positions[0] as usize, info.start - segment.data, "Info didn't move");
        let tracks_at = segment.data + positions[1] as usize;
        assert_eq!(read_id(&fixed, tracks_at).unwrap().0, 0x1654_AE6B);
    }

    #[test]
    fn rejects_other_files() {
        assert!(set_duration(b"ftypisom", 1.0).is_err());
        assert!(set_duration(&recorder_head(false, None)[..30], 1.0).is_err());
        assert!(set_duration(&recorder_head(false, None), f64::NAN).is_err());
    }
}
