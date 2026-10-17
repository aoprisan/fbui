//! `disk-verify`: check that every sector of a disk holds an expected byte
//! pattern, skipping the sector ranges you choose, with a live surface map.
//!
//! ```text
//! cargo run -p fbui-disk-verify [-- extra.img /dev/sdX ...]
//! ```
//!
//! Pick a disk, enter the pattern (hex, repeated across the disk from byte 0;
//! `00` for a zeroed disk) and any ranges to skip (`0-2047, 1000000+4096`),
//! then Start. The scan only reads. Esc quits. `scripts/build-iso.sh` packs
//! this binary into a bootable ISO that runs it on the first VT.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use fbui::widgets::{Align, Button, Container, Label, List, ProgressBar, TextInput};
use fbui::{App, Proxy, Ui};

use fbui_disk_verify::devices::{self, human_bytes, Target};
use fbui_disk_verify::map::{Cell, SurfaceMap, Swatch};
use fbui_disk_verify::pattern::Pattern;
use fbui_disk_verify::ranges::{self, Range};
use fbui_disk_verify::scan::{self, End, Job, Report, Totals};
use fbui_disk_verify::{duration, thousands};

/// How often the scanner reports to the UI.
const REPORT_EVERY: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
enum Msg {
    Select(usize),
    Pattern(String),
    Skips(String),
    StartStop,
    Rescan,
    Report(Report),
    Failed(String),
}

/// Validated inputs: what Start would scan.
struct Setup<'a> {
    target: &'a Target,
    pattern: Pattern,
    skips: Vec<Range>,
    plan: Vec<Range>,
}

/// A scan in flight.
struct Running {
    cancel: Arc<AtomicBool>,
    /// Sectors the plan reads (the disk minus the skips).
    planned: u64,
    sector_size: usize,
}

#[derive(Default)]
struct Verify {
    /// Paths from the command line, listed before the block devices.
    extra: Vec<String>,
    targets: Vec<Target>,
    selected: Option<usize>,
    pattern: String,
    skips: String,
    proxy: Option<Proxy<Msg>>,
    running: Option<Running>,
    issues: Vec<String>,
    /// Bad sectors past what the issue list keeps.
    unlisted: u64,
}

impl Verify {
    fn target(&self) -> Option<&Target> {
        self.selected.and_then(|i| self.targets.get(i))
    }

    fn set(&self, ui: &mut Ui<Msg>, name: &str, text: impl Into<String>) {
        if let Some(id) = ui.find(name) {
            ui.with::<Label, _>(id, |l| l.set_text(text));
        }
    }

    fn rescan(&mut self, ui: &mut Ui<Msg>) {
        let keep = self.target().map(|t| t.path.clone());
        self.targets = self
            .extra
            .iter()
            .filter_map(|p| devices::from_path(p))
            .collect();
        self.targets.extend(devices::block_devices());
        self.selected = keep
            .and_then(|p| self.targets.iter().position(|t| t.path == p))
            .or(if self.targets.len() == 1 {
                Some(0)
            } else {
                None
            });
        let rows: Vec<String> = if self.targets.is_empty() {
            vec!["no disks found — plug one in and Rescan".into()]
        } else {
            self.targets.iter().map(Target::label).collect()
        };
        let (selected, sectors) = (self.selected, self.target().map_or(0, |t| t.sectors));
        if let Some(id) = ui.find("disks") {
            ui.with::<List<Msg>, _>(id, |l| {
                l.set_rows(rows);
                l.set_selected(selected);
            });
        }
        if let Some(id) = ui.find("map") {
            ui.with::<SurfaceMap, _>(id, |m| m.reset(sectors));
        }
        self.preview(ui);
    }

    /// Validate the inputs.
    fn validate(&self) -> Result<Setup<'_>, String> {
        let target = self.target().ok_or("select a disk")?;
        let pattern = Pattern::parse(&self.pattern)?;
        let skips = ranges::parse_skips(&self.skips, target.sectors)?;
        let plan = ranges::plan(target.sectors, &skips);
        if plan.is_empty() {
            return Err("everything is skipped — nothing to check".into());
        }
        Ok(Setup {
            target,
            pattern,
            skips,
            plan,
        })
    }

    /// Say what Start would do, or what's wrong with the inputs.
    fn preview(&self, ui: &mut Ui<Msg>) {
        if self.running.is_some() {
            return;
        }
        let text = match self.validate() {
            Ok(Setup {
                target: t,
                pattern: p,
                skips,
                plan,
            }) => {
                let check: u64 = plan.iter().map(Range::len).sum();
                let skip: u64 = skips.iter().map(Range::len).sum();
                format!(
                    "Will check {} sectors ({}) for {}{}",
                    thousands(check),
                    human_bytes(check * t.sector_size as u64),
                    p.describe(),
                    if skip > 0 {
                        format!(", skipping {} in {} range(s)", thousands(skip), skips.len())
                    } else {
                        String::new()
                    }
                )
            }
            Err(e) => e,
        };
        self.set(ui, "plan", text);
    }

    fn start(&mut self, ui: &mut Ui<Msg>) {
        let Setup {
            target,
            pattern,
            skips,
            plan,
        } = match self.validate() {
            Ok(v) => v,
            Err(e) => return self.set(ui, "plan", e),
        };
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let (path, sectors, ss) = (target.path.clone(), target.sectors, target.sector_size);
        let planned = plan.iter().map(Range::len).sum();

        if let Some(id) = ui.find("map") {
            ui.with::<SurfaceMap, _>(id, |m| {
                m.reset(sectors);
                for r in &skips {
                    m.mark(*r, Cell::Skipped);
                }
                m.set_head(plan.first().map(|r| r.start));
            });
        }
        self.issues.clear();
        self.unlisted = 0;
        self.show_issues(ui);
        self.show_totals(ui, &Totals::default(), skips.iter().map(Range::len).sum());
        if let Some(id) = ui.find("bar") {
            ui.with::<ProgressBar, _>(id, |b| b.set_fraction(0.0));
        }
        self.set(ui, "stats", format!("Opening {path}…"));
        self.set_button(ui, "Stop");

        let cancel = Arc::new(AtomicBool::new(false));
        self.running = Some(Running {
            cancel: cancel.clone(),
            planned,
            sector_size: ss,
        });
        thread::spawn(move || {
            let (file, _direct) = match scan::open(&path) {
                Ok(v) => v,
                Err(e) => {
                    proxy.send(Msg::Failed(format!("can't open {path}: {e}")));
                    return;
                }
            };
            let job = Job {
                source: file,
                sector_size: ss,
                plan,
                pattern,
            };
            scan::run(job, &cancel, REPORT_EVERY, |r| proxy.send(Msg::Report(r)));
        });
    }

    fn set_button(&self, ui: &mut Ui<Msg>, label: &str) {
        if let Some(id) = ui.find("start") {
            ui.with::<Button<Msg>, _>(id, |b| b.set_label(label));
        }
    }

    fn show_issues(&self, ui: &mut Ui<Msg>) {
        let mut rows = if self.issues.is_empty() {
            vec!["no problems found".to_string()]
        } else {
            self.issues.clone()
        };
        if self.unlisted > 0 {
            rows.push(format!("… and {} more", thousands(self.unlisted)));
        }
        if let Some(id) = ui.find("issues") {
            ui.with::<List<Msg>, _>(id, |l| l.set_rows(rows));
        }
    }

    fn show_totals(&self, ui: &mut Ui<Msg>, t: &Totals, skipped: u64) {
        self.set(
            ui,
            "totals",
            format!(
                "good {}   wrong {}   unreadable {}   skipped {}",
                thousands(t.ok),
                thousands(t.mismatched),
                thousands(t.unreadable),
                thousands(skipped)
            ),
        );
    }

    fn report(&mut self, r: Report, ui: &mut Ui<Msg>) {
        let Some(run) = &self.running else {
            return; // a straggler from a scan already stopped
        };
        let (planned, ss) = (run.planned, run.sector_size);
        let sectors = self.target().map_or(0, |t| t.sectors);

        if let Some(id) = ui.find("map") {
            ui.with::<SurfaceMap, _>(id, |m| {
                for (range, status) in &r.done {
                    m.mark(*range, (*status).into());
                }
                m.set_head(if r.end.is_some() { None } else { Some(r.pos) });
            });
        }
        for issue in &r.issues {
            if self.issues.len() < scan::MAX_ISSUES {
                self.issues.push(issue.to_string());
            }
        }
        self.unlisted = r.totals.bad().saturating_sub(self.issues.len() as u64);
        if !r.issues.is_empty() || r.end.is_some() {
            self.show_issues(ui);
        }

        let checked = r.totals.checked();
        let fraction = if planned == 0 {
            1.0
        } else {
            checked as f32 / planned as f32
        };
        if let Some(id) = ui.find("bar") {
            ui.with::<ProgressBar, _>(id, |b| b.set_fraction(fraction));
        }
        self.show_totals(ui, &r.totals, sectors - planned);

        let secs = r.elapsed.as_secs_f64().max(1e-3);
        let rate = checked as f64 * ss as f64 / secs;
        let stats = match &r.end {
            None => {
                let eta = if checked > 0 {
                    ((planned - checked) as f64 * ss as f64 / rate) as u64
                } else {
                    0
                };
                format!(
                    "{:.1}%   sector {} of {}   {}/s   ETA {}",
                    fraction * 100.0,
                    thousands(r.pos),
                    thousands(sectors),
                    human_bytes(rate as u64),
                    duration(eta)
                )
            }
            Some(end) => {
                let verdict = match (end, r.totals.bad()) {
                    (End::Cancelled, _) => "Stopped".to_string(),
                    (End::Completed, 0) => "PASS — every checked sector matches".to_string(),
                    (End::Completed, n) => format!("FAIL — {} bad sector(s)", thousands(n)),
                };
                format!(
                    "{verdict}   {} checked in {} ({}/s)",
                    human_bytes(checked * ss as u64),
                    duration(r.elapsed.as_secs()),
                    human_bytes(rate as u64)
                )
            }
        };
        self.set(ui, "stats", stats);
        if r.end.is_some() {
            self.running = None;
            self.set_button(ui, "Start");
        }
    }
}

impl App for Verify {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let muted = ui.theme().palette.muted;
        let root = ui.set_root(Container::column().fill().padding(16.0).gap(10.0));
        ui.add_child(root, Label::new("Disk surface verify").size(22.0).bold());

        let body = ui.add_child(root, Container::row().grow(1.0).gap(16.0));

        // Left: what to scan.
        let left = ui.add_child(body, Container::column().width(380.0).gap(8.0));
        ui.add_child(left, Label::new("Disk").color(muted));
        let disks = ui.add_child(left, Container::column().height(150.0));
        ui.add_named(disks, "disks", List::new(Vec::new()).on_select(Msg::Select));
        ui.add_child(
            left,
            Label::new("Pattern (hex bytes, repeated from byte 0)").color(muted),
        );
        ui.add_named(
            left,
            "pattern",
            TextInput::new()
                .value(self.pattern.clone())
                .placeholder("00, ff, deadbeef…")
                .on_change(Msg::Pattern),
        );
        ui.add_child(
            left,
            Label::new("Skip sectors (A-B, A+N, A, A-)").color(muted),
        );
        ui.add_named(
            left,
            "skips",
            TextInput::new()
                .value(self.skips.clone())
                .placeholder("e.g. 0-2047, 1000000+4096")
                .on_change(Msg::Skips),
        );
        let buttons = ui.add_child(left, Container::row().gap(8.0).align(Align::Center));
        ui.add_named(
            buttons,
            "start",
            Button::new("Start").on_press(|| Msg::StartStop),
        );
        ui.add_named(
            buttons,
            "rescan",
            Button::new("Rescan").secondary().on_press(|| Msg::Rescan),
        );
        ui.add_named(left, "plan", Label::new(" ").color(muted).wrap());

        // Right: what the scan finds.
        let right = ui.add_child(body, Container::column().grow(1.0).gap(8.0));
        ui.add_named(right, "map", SurfaceMap::new());
        let legend = ui.add_child(right, Container::row().gap(6.0).align(Align::Center));
        for (cell, text) in [
            (Cell::Ok, "good"),
            (Cell::Mismatch, "wrong bytes"),
            (Cell::Unreadable, "unreadable"),
            (Cell::Skipped, "skipped"),
            (Cell::Pending, "pending"),
        ] {
            ui.add_child(legend, Swatch(cell));
            ui.add_child(legend, Label::new(format!("{text}   ")).color(muted));
        }
        ui.add_named(right, "bar", ProgressBar::new(0.0));
        ui.add_named(right, "stats", Label::new("Idle"));
        ui.add_named(right, "totals", Label::new(" ").color(muted));
        ui.add_child(right, Label::new("Bad sectors").color(muted));
        let issues = ui.add_child(right, Container::column().height(140.0));
        ui.add_named(issues, "issues", List::new(Vec::new()).row_height(28.0));

        self.rescan(ui);
        self.show_issues(ui);
    }

    fn on_start(&mut self, proxy: Proxy<Msg>) {
        self.proxy = Some(proxy);
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Select(i) => {
                if self.running.is_none() && i < self.targets.len() {
                    self.selected = Some(i);
                    if let Some(id) = ui.find("map") {
                        let n = self.targets[i].sectors;
                        ui.with::<SurfaceMap, _>(id, |m| m.reset(n));
                    }
                }
                self.preview(ui);
            }
            Msg::Pattern(s) => {
                self.pattern = s;
                self.preview(ui);
            }
            Msg::Skips(s) => {
                self.skips = s;
                self.preview(ui);
            }
            Msg::Rescan => {
                if self.running.is_none() {
                    self.rescan(ui);
                }
            }
            Msg::StartStop => match &self.running {
                Some(run) => {
                    run.cancel.store(true, Ordering::Relaxed);
                }
                None => self.start(ui),
            },
            Msg::Report(r) => self.report(r, ui),
            Msg::Failed(e) => {
                self.running = None;
                self.set_button(ui, "Start");
                self.set(ui, "stats", e);
            }
        }
    }

    fn describe_message(&self, msg: &Msg) -> Option<String> {
        Some(match msg {
            // A report's vectors would swamp a trace; summarize it.
            Msg::Report(r) => format!(
                "Report(pos {}, checked {}, bad {}, end {:?})",
                r.pos,
                r.totals.checked(),
                r.totals.bad(),
                r.end
            ),
            m => format!("{m:?}"),
        })
    }
}

fn main() {
    let app = Verify {
        extra: std::env::args().skip(1).collect(),
        pattern: std::env::var("DISK_VERIFY_PATTERN").unwrap_or_else(|_| "00".into()),
        skips: std::env::var("DISK_VERIFY_SKIP").unwrap_or_default(),
        ..Verify::default()
    };
    if let Err(e) = fbui::run(app) {
        eprintln!("disk-verify: {e}");
        std::process::exit(1);
    }
}
