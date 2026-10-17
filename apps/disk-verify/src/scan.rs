//! The surface scan: read every planned sector, compare it with the pattern,
//! and report what was found in batches.
//!
//! It runs on a worker thread and knows nothing about the UI — the caller
//! hands it a sink that receives a [`Report`] every `interval` (and a final
//! one), so a slow UI never slows the disk and a fast disk never floods the UI.
//! Reads go a chunk at a time; a chunk that fails to read is retried sector by
//! sector, so one bad sector costs one sector, not a megabyte.

use std::fmt;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::pattern::{Pattern, Reference};
use crate::ranges::Range;

/// Bytes read per request in the fast path.
pub const CHUNK_BYTES: usize = 1 << 20;
/// O_DIRECT wants the buffer aligned to the logical block size; 4 KiB covers
/// every disk in practice.
const ALIGN: usize = 4096;
/// Issues kept for display; the counters keep counting past it.
pub const MAX_ISSUES: usize = 1000;

/// Positioned reads. `File` is the real one; tests supply a fake disk.
pub trait ReadAt: Send {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

impl ReadAt for File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        FileExt::read_at(self, buf, offset)
    }
}

/// What a run of sectors turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Mismatch,
    Unreadable,
}

/// One bad sector, for the problem list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Issue {
    /// Read fine but holds the wrong bytes.
    Mismatch {
        sector: u64,
        /// First differing byte within the sector.
        at: usize,
        expected: u8,
        found: u8,
        /// How many bytes of the sector differ.
        differing: usize,
    },
    /// The device returned an error for this sector.
    Unreadable { sector: u64, error: String },
}

impl Issue {
    pub fn sector(&self) -> u64 {
        match self {
            Issue::Mismatch { sector, .. } | Issue::Unreadable { sector, .. } => *sector,
        }
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Issue::Mismatch {
                sector,
                at,
                expected,
                found,
                differing,
            } => write!(
                f,
                "{sector}: byte {at} is {found:02x}, want {expected:02x} ({differing} B differ)"
            ),
            Issue::Unreadable { sector, error } => write!(f, "{sector}: unreadable ({error})"),
        }
    }
}

/// Running counts, in sectors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub ok: u64,
    pub mismatched: u64,
    pub unreadable: u64,
}

impl Totals {
    pub fn checked(&self) -> u64 {
        self.ok + self.mismatched + self.unreadable
    }

    pub fn bad(&self) -> u64 {
        self.mismatched + self.unreadable
    }
}

/// How a scan ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    Completed,
    Cancelled,
}

/// What happened since the previous report.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// The next sector the scan will read (the read head, for the map).
    pub pos: u64,
    /// Runs checked since the last report, adjacent same-status runs merged.
    pub done: Vec<(Range, Status)>,
    /// Bad sectors found since the last report (capped at [`MAX_ISSUES`] per scan).
    pub issues: Vec<Issue>,
    /// Cumulative counts.
    pub totals: Totals,
    /// Time since the scan started.
    pub elapsed: Duration,
    /// `Some` on the last report.
    pub end: Option<End>,
}

impl Report {
    fn push_done(&mut self, r: Range, s: Status) {
        if let Some((last, ls)) = self.done.last_mut() {
            if *ls == s && last.end == r.start {
                last.end = r.end;
                return;
            }
        }
        self.done.push((r, s));
    }
}

/// Everything a scan needs.
pub struct Job<R> {
    pub source: R,
    pub sector_size: usize,
    /// Ranges to read, sorted (the complement of the skip list).
    pub plan: Vec<Range>,
    pub pattern: Pattern,
}

/// Read `buf.len()` bytes at `offset`, or fail. A short read past the end of
/// the device is an error: the plan never asks for sectors that don't exist.
fn read_full<R: ReadAt>(src: &R, buf: &mut [u8], offset: u64) -> io::Result<()> {
    let mut got = 0;
    while got < buf.len() {
        match src.read_at(&mut buf[got..], offset + got as u64) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Compare one sector; `Some` describes the first mismatch.
fn check_sector(data: &[u8], want: &[u8], sector: u64) -> Option<Issue> {
    if data == want {
        return None;
    }
    let at = data.iter().zip(want).position(|(a, b)| a != b)?;
    let differing = data.iter().zip(want).filter(|(a, b)| a != b).count();
    Some(Issue::Mismatch {
        sector,
        at,
        expected: want[at],
        found: data[at],
        differing,
    })
}

/// Run the scan to the end (or until `cancel` is set, or `sink` returns
/// `false`), calling `sink` at most every `interval` and once at the end.
pub fn run<R: ReadAt>(
    job: Job<R>,
    cancel: &AtomicBool,
    interval: Duration,
    mut sink: impl FnMut(Report) -> bool,
) -> End {
    let ss = job.sector_size;
    let chunk_sectors = (CHUNK_BYTES / ss).max(1) as u64;
    let chunk_bytes = chunk_sectors as usize * ss;
    let reference = Reference::new(&job.pattern, chunk_bytes);

    // An aligned window into an over-allocated Vec: what O_DIRECT needs,
    // without unsafe.
    let mut raw = vec![0u8; chunk_bytes + ALIGN];
    let skew = raw.as_ptr().align_offset(ALIGN).min(ALIGN);
    let buf = &mut raw[skew..skew + chunk_bytes];

    let start = Instant::now();
    let mut last_emit = start;
    let mut report = Report::default();
    let mut totals = Totals::default();
    let mut kept_issues = 0usize;

    let mut record =
        |report: &mut Report, totals: &mut Totals, sector: u64, issue: Option<Issue>| {
            let status = match &issue {
                None => Status::Ok,
                Some(Issue::Mismatch { .. }) => Status::Mismatch,
                Some(Issue::Unreadable { .. }) => Status::Unreadable,
            };
            match status {
                Status::Ok => totals.ok += 1,
                Status::Mismatch => totals.mismatched += 1,
                Status::Unreadable => totals.unreadable += 1,
            }
            if let Some(issue) = issue {
                if kept_issues < MAX_ISSUES {
                    kept_issues += 1;
                    report.issues.push(issue);
                }
            }
            report.push_done(Range::new(sector, sector + 1), status);
        };

    let mut end = End::Completed;
    'plan: for range in &job.plan {
        let mut s = range.start;
        while s < range.end {
            if cancel.load(Ordering::Relaxed) {
                end = End::Cancelled;
                break 'plan;
            }
            let n = chunk_sectors.min(range.end - s);
            let len = n as usize * ss;
            let offset = s * ss as u64;
            match read_full(&job.source, &mut buf[..len], offset) {
                Ok(()) if buf[..len] == *reference.window(offset, len) => {
                    totals.ok += n;
                    report.push_done(Range::new(s, s + n), Status::Ok);
                }
                Ok(()) => {
                    for i in 0..n {
                        let o = i as usize * ss;
                        let off = offset + o as u64;
                        let issue = check_sector(&buf[o..o + ss], reference.window(off, ss), s + i);
                        record(&mut report, &mut totals, s + i, issue);
                    }
                }
                Err(_) => {
                    // Isolate the bad sectors: one read each.
                    for i in 0..n {
                        let off = offset + i * ss as u64;
                        let one = &mut buf[..ss];
                        let issue = match read_full(&job.source, one, off) {
                            Ok(()) => check_sector(one, reference.window(off, ss), s + i),
                            Err(e) => Some(Issue::Unreadable {
                                sector: s + i,
                                error: e.to_string(),
                            }),
                        };
                        record(&mut report, &mut totals, s + i, issue);
                    }
                }
            }
            s += n;

            let now = Instant::now();
            if now - last_emit >= interval {
                last_emit = now;
                report.pos = s;
                report.totals = totals;
                report.elapsed = now - start;
                if !sink(std::mem::take(&mut report)) {
                    end = End::Cancelled;
                    break 'plan;
                }
            }
        }
    }
    report.pos = job.plan.last().map_or(0, |r| r.end);
    report.totals = totals;
    report.elapsed = start.elapsed();
    report.end = Some(end.clone());
    sink(report);
    end
}

/// What a scan needs to know about a target before it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub sector_size: usize,
    pub sectors: u64,
}

/// Open `path` for scanning: `O_DIRECT` where the device or filesystem allows
/// it (so the scan measures the media, not the page cache), buffered otherwise.
/// Returns the file and whether the direct path was taken.
pub fn open(path: &str) -> io::Result<(File, bool)> {
    use std::os::unix::fs::OpenOptionsExt;
    let direct = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECT)
        .open(path);
    match direct {
        Ok(f) => Ok((f, true)),
        Err(_) => Ok((std::fs::File::open(path)?, false)),
    }
}

/// Size a target: block devices by seeking to their end (works without
/// ioctls), sector size from sysfs (`512` for plain files).
pub fn geometry(path: &str) -> io::Result<Geometry> {
    use std::io::{Seek, SeekFrom};
    let mut f = File::open(path)?;
    let bytes = f.seek(SeekFrom::End(0))?;
    let sector_size = crate::devices::logical_sector_size(path).unwrap_or(512);
    Ok(Geometry {
        sector_size,
        sectors: bytes / sector_size as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ranges::{parse_skips, plan};

    /// An in-memory disk with sectors that fail to read.
    struct Fake {
        data: Vec<u8>,
        ss: usize,
        unreadable: Vec<u64>,
    }

    impl ReadAt for Fake {
        fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
            let first = offset / self.ss as u64;
            let last = (offset + buf.len() as u64 - 1) / self.ss as u64;
            if self.unreadable.iter().any(|s| (first..=last).contains(s)) {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            let o = offset as usize;
            let n = buf.len().min(self.data.len().saturating_sub(o));
            buf[..n].copy_from_slice(&self.data[o..o + n]);
            Ok(n)
        }
    }

    fn disk(pattern: &Pattern, sectors: usize, ss: usize) -> Vec<u8> {
        (0..sectors * ss).map(|i| pattern.at(i as u64)).collect()
    }

    fn scan(fake: Fake, plan: Vec<Range>, pattern: Pattern) -> (Report, Vec<Report>) {
        let cancel = AtomicBool::new(false);
        let mut reports = Vec::new();
        let ss = fake.ss;
        run(
            Job {
                source: fake,
                sector_size: ss,
                plan,
                pattern,
            },
            &cancel,
            Duration::ZERO,
            |r| {
                reports.push(r);
                true
            },
        );
        let last = reports.last().unwrap().clone();
        (last, reports)
    }

    #[test]
    fn clean_disk_is_all_ok() {
        let p = Pattern::parse("deadbeef").unwrap();
        let sectors = 5000; // more than one chunk
        let fake = Fake {
            data: disk(&p, sectors, 512),
            ss: 512,
            unreadable: vec![],
        };
        let (last, _) = scan(fake, plan(sectors as u64, &[]), p);
        assert_eq!(last.end, Some(End::Completed));
        assert_eq!(last.totals.ok, sectors as u64);
        assert_eq!(last.totals.bad(), 0);
    }

    #[test]
    fn finds_corruption_and_read_errors_and_honours_skips() {
        let p = Pattern::parse("00").unwrap();
        let sectors = 6000u64;
        let mut data = disk(&p, sectors as usize, 512);
        data[100 * 512 + 7] = 0x41; // corrupt sector 100, byte 7
        data[3000 * 512 + 1] = 0x99; // inside the skipped range: never seen
        let fake = Fake {
            data,
            ss: 512,
            unreadable: vec![4500],
        };
        let skips = parse_skips("2048-4095", sectors).unwrap();
        let (last, reports) = scan(fake, plan(sectors, &skips), p);

        assert_eq!(last.totals.mismatched, 1);
        assert_eq!(last.totals.unreadable, 1);
        assert_eq!(last.totals.checked(), sectors - 2048);

        let issues: Vec<Issue> = reports.iter().flat_map(|r| r.issues.clone()).collect();
        assert_eq!(
            issues[0],
            Issue::Mismatch {
                sector: 100,
                at: 7,
                expected: 0,
                found: 0x41,
                differing: 1
            }
        );
        assert!(matches!(issues[1], Issue::Unreadable { sector: 4500, .. }));
        // The skipped range was never reported as checked.
        let done: Vec<(Range, Status)> = reports.iter().flat_map(|r| r.done.clone()).collect();
        assert!(done.iter().all(|(r, _)| r.end <= 2048 || r.start >= 4096));
        assert!(done.contains(&(Range::new(100, 101), Status::Mismatch)));
        assert!(done.contains(&(Range::new(4500, 4501), Status::Unreadable)));
    }

    #[test]
    fn pattern_is_anchored_to_the_disk_not_the_range() {
        // A 3-byte pattern doesn't divide 512, so a scan that restarted the
        // pattern at each range would mis-compare after a skip.
        let p = Pattern::parse("010203").unwrap();
        let sectors = 64u64;
        let fake = Fake {
            data: disk(&p, sectors as usize, 512),
            ss: 512,
            unreadable: vec![],
        };
        let skips = parse_skips("1, 10-12", sectors).unwrap();
        let (last, _) = scan(fake, plan(sectors, &skips), p);
        assert_eq!(last.totals.ok, sectors - 4);
        assert_eq!(last.totals.bad(), 0);
    }

    #[test]
    fn cancel_stops_the_scan() {
        let p = Pattern::parse("00").unwrap();
        let fake = Fake {
            data: disk(&p, 10_000, 512),
            ss: 512,
            unreadable: vec![],
        };
        let cancel = AtomicBool::new(false);
        let mut n = 0;
        let end = run(
            Job {
                source: fake,
                sector_size: 512,
                plan: plan(10_000, &[]),
                pattern: p,
            },
            &cancel,
            Duration::ZERO,
            |_| {
                n += 1;
                if n == 2 {
                    cancel.store(true, Ordering::Relaxed);
                }
                true
            },
        );
        assert_eq!(end, End::Cancelled);
    }
}
