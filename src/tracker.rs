//! The counting rules, independent of where the signals come from.
//!
//! - Time counts while an editor from the list has focus, the screen is unlocked and the
//!   keyboard or mouse were used less than `idle_timeout` ago.
//! - Pauses shorter than the timeout count; once a pause reaches the timeout, the whole pause
//!   is taken off (the block ends at the last input).
//! - A gap between samples much longer than the sample interval (suspend, a frozen process)
//!   is never counted.

use anyhow::Result;

use crate::store::Sink;

/// What the samplers saw at one instant.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observation {
    /// The project and editor of the focused window, when it is an editor from the list
    /// working inside a known project.
    pub work: Option<Work>,
    /// Seconds since the last keyboard or mouse input; `None` when no idle source works.
    pub idle_secs: Option<u64>,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Work {
    pub project: String,
    pub editor: String,
}

#[derive(Debug, Clone)]
struct Current {
    id: i64,
    project: String,
    start: i64,
    end: i64,
}

pub struct Tracker {
    idle_timeout: i64,
    max_gap: i64,
    current: Option<Current>,
    last_tick: Option<i64>,
}

impl Tracker {
    pub fn new(idle_timeout_secs: u64, sample_secs: u64) -> Self {
        Tracker {
            idle_timeout: idle_timeout_secs as i64,
            // Up to a few missed samples is jitter; more than that is a suspend.
            max_gap: (sample_secs as i64 * 4).max(30),
            current: None,
            last_tick: None,
        }
    }

    /// The project being counted right now, if any.
    pub fn current_project(&self) -> Option<&str> {
        self.current.as_ref().map(|c| c.project.as_str())
    }

    /// The id of the block being extended right now, if any.
    pub fn current_block(&self) -> Option<i64> {
        self.current.as_ref().map(|c| c.id)
    }

    pub fn tick(&mut self, now: i64, obs: &Observation, sink: &mut dyn Sink) -> Result<()> {
        let idle = obs.idle_secs.unwrap_or(0) as i64;
        let last_input = now - idle;
        let suspended = self.last_tick.is_some_and(|t| now - t > self.max_gap);
        self.last_tick = Some(now);

        if suspended {
            // The block ends where the last sample before the gap left it.
            self.current = None;
        }

        let active = !obs.locked && idle < self.idle_timeout && obs.work.is_some();
        let project = obs.work.as_ref().map(|w| w.project.as_str());

        // Close the running block when work stops or moves to another project.
        if let Some(cur) = &self.current {
            let same = active && project == Some(cur.project.as_str());
            if !same {
                if idle >= self.idle_timeout {
                    // The pause reached the limit: none of it counts.
                    let end = last_input.clamp(cur.start, cur.end);
                    sink.set_end(cur.id, end)?;
                }
                self.current = None;
            }
        }

        if active {
            let work = obs.work.as_ref().expect("active implies work");
            match &mut self.current {
                Some(cur) => {
                    cur.end = now;
                    sink.set_end(cur.id, now)?;
                }
                None => {
                    let id = sink.open_block(&work.project, &work.editor, now)?;
                    self.current =
                        Some(Current { id, project: work.project.clone(), start: now, end: now });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn work(p: &str) -> Observation {
        Observation {
            work: Some(Work { project: p.into(), editor: "VS Code".into() }),
            idle_secs: Some(0),
            locked: false,
        }
    }

    fn idle(mut o: Observation, secs: u64) -> Observation {
        o.idle_secs = Some(secs);
        o
    }

    /// Runs one observation every 5 s from `from` to `to` (inclusive).
    fn run(t: &mut Tracker, s: &mut Store, from: i64, to: i64, f: impl Fn(i64) -> Observation) {
        let mut now = from;
        while now <= to {
            t.tick(now, &f(now), s).unwrap();
            now += 5;
        }
    }

    #[test]
    fn steady_work_counts_fully() {
        let (mut t, mut s) = (Tracker::new(300, 5), Store::in_memory().unwrap());
        run(&mut t, &mut s, 0, 600, |_| work("shop"));
        assert_eq!(s.project_total("shop").unwrap(), 600);
    }

    #[test]
    fn short_pause_counts() {
        let (mut t, mut s) = (Tracker::new(300, 5), Store::in_memory().unwrap());
        // Typing until 100, then reading without input for 4 minutes, then typing again.
        run(&mut t, &mut s, 0, 400, |now| {
            if now <= 100 { work("shop") } else if now < 340 { idle(work("shop"), (now - 100) as u64) } else { work("shop") }
        });
        assert_eq!(s.project_total("shop").unwrap(), 400);
    }

    #[test]
    fn long_pause_is_taken_off_entirely() {
        let (mut t, mut s) = (Tracker::new(300, 5), Store::in_memory().unwrap());
        // Last input at 100, editor left focused and untouched until 1000.
        run(&mut t, &mut s, 0, 1000, |now| {
            if now <= 100 { work("shop") } else { idle(work("shop"), (now - 100) as u64) }
        });
        assert_eq!(s.project_total("shop").unwrap(), 100);
    }

    #[test]
    fn other_windows_and_lock_do_not_count() {
        let (mut t, mut s) = (Tracker::new(300, 5), Store::in_memory().unwrap());
        run(&mut t, &mut s, 0, 300, |now| {
            if now < 100 {
                work("shop")
            } else if now < 200 {
                Observation { work: None, idle_secs: Some(0), locked: false } // testing in the browser
            } else {
                let mut o = work("shop");
                o.locked = true;
                o
            }
        });
        assert_eq!(s.project_total("shop").unwrap(), 95);
    }

    #[test]
    fn switching_projects_splits_time() {
        let (mut t, mut s) = (Tracker::new(300, 5), Store::in_memory().unwrap());
        run(&mut t, &mut s, 0, 200, |now| if now < 100 { work("shop") } else { work("web") });
        assert_eq!(s.project_total("shop").unwrap(), 95);
        assert_eq!(s.project_total("web").unwrap(), 100);
        assert_eq!(t.current_project(), Some("web"));
    }

    #[test]
    fn suspend_gap_is_not_counted() {
        let (mut t, mut s) = (Tracker::new(300, 5), Store::in_memory().unwrap());
        run(&mut t, &mut s, 0, 100, |_| work("shop"));
        // The laptop sleeps for an hour; input happened right after waking.
        run(&mut t, &mut s, 3700, 3800, |_| work("shop"));
        assert_eq!(s.project_total("shop").unwrap(), 200);
    }
}
