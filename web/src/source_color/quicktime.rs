use super::{Profile, apple_log};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    ops::Range,
    path::Path,
};

struct Atom {
    kind: [u8; 4],
    body: Range<u64>,
}

struct Container(File);

impl Container {
    fn read<const N: usize>(&mut self, at: u64) -> Option<[u8; N]> {
        self.0.seek(SeekFrom::Start(at)).ok()?;
        let mut data = [0; N];
        self.0.read_exact(&mut data).ok()?;
        Some(data)
    }

    fn atom(&mut self, range: Range<u64>) -> Option<Atom> {
        if range.end.checked_sub(range.start)? < 8 {
            return None;
        }
        let header = self.read::<8>(range.start)?;
        let size = u64::from(u32::from_be_bytes(header[..4].try_into().ok()?));
        let (header_len, size) = match size {
            0 => (8, range.end - range.start),
            1 if range.end - range.start >= 16 => {
                (16, u64::from_be_bytes(self.read::<8>(range.start + 8)?))
            }
            1 => return None,
            other => (8, other),
        };
        if size < header_len || size > range.end - range.start {
            return None;
        }
        Some(Atom {
            kind: header[4..].try_into().ok()?,
            body: range.start + header_len..range.start + size,
        })
    }

    fn child(&mut self, mut range: Range<u64>, wanted: &[u8; 4]) -> Option<Atom> {
        for _ in 0..4096 {
            let atom = self.atom(range.clone())?;
            if &atom.kind == wanted {
                return Some(atom);
            }
            range.start = atom.body.end;
        }
        None
    }

    fn profile(&mut self, media: Range<u64>) -> Option<Profile> {
        let handler = self.child(media.clone(), b"hdlr")?.body;
        if handler.end - handler.start < 12 || self.read::<4>(handler.start + 8)? != *b"vide" {
            return None;
        }
        let info = self.child(media, b"minf")?.body;
        let table = self.child(info, b"stbl")?.body;
        let description = self.child(table, b"stsd")?.body;
        // A video sample description starts with flags and count, followed by visual
        // sample entries. Each entry has 78 bytes of fixed fields ahead of extensions.
        let entry = self.atom(description.start.checked_add(8)?..description.end)?;
        let log = self
            .child(entry.body.start.checked_add(78)?..entry.body.end, b"logs")?
            .body;
        let len = usize::try_from(log.end - log.start).ok()?;
        if !(1..=128).contains(&len) {
            return None;
        }
        let mut data = [0; 128];
        self.0.seek(SeekFrom::Start(log.start)).ok()?;
        self.0.read_exact(&mut data[..len]).ok()?;
        apple_log(std::str::from_utf8(&data[..len]).ok()?.trim_matches('\0'))
    }
}

pub(super) fn log_profile(path: &Path) -> Option<Profile> {
    let file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut container = Container(file);
    let mut tracks = container.child(0..size, b"moov")?.body;
    for _ in 0..256 {
        let track = container.atom(tracks.clone())?;
        tracks.start = track.body.end;
        if &track.kind == b"trak"
            && let Some(media) = container.child(track.body, b"mdia")
            && let Some(profile) = container.profile(media.body)
        {
            return Some(profile);
        }
    }
    None
}
