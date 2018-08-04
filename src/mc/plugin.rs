//! A plugin architecture to enable reusing of interfaces and
//! implementation for different Monte Carlo algorithms.

use super::*;

use std::cell::Cell;
use std::default::Default;
use std::time;

/// A `Plugin` is an object that can be used to configure a MonteCarlo
/// simulation.  The plugin will be called regularly, and will have a
/// chance to save data (e.g. collect statistics) and/or terminate the
/// simulation.
pub trait Plugin<MC: MonteCarlo> {
    /// Run and do something.  If the simulation needs to be
    /// terminated, `None` is returned.  If you want to modify
    /// information, you will have to use interior mutability, because
    /// I can't figure out any practical way to borrow `self` mutably
    /// while still giving read access to the `MC`.
    fn run(&self, _mc: &MC, _sys: &MC::System) -> Action { Action::None }
    /// How often we need the plugin to run.  A `None` value means
    /// that this plugin never needs to run.  Note that it is expected
    /// that this period may change any time the plugin is called, so
    /// this should be a cheap call as it may happen frequently.  Also
    /// note that this is an upper, not a lower bound.
    fn run_period(&self) -> Option<u64> { None }
    /// We might be about to die, so please do any cleanup or saving.
    /// Note that the plugin state is stored on each checkpoint.  This
    /// is called in response to `Action::Save` and `Action::Exit`.
    fn save(&self, _mc: &MC, _sys: &MC::System) {}
    /// Log to stdout any interesting data we think our user might
    /// care about.  This is called in response to `Action::Save`,
    /// `Action::Log` and `Action::Exit`.
    fn log(&self, _mc: &MC, _sys: &MC::System) {}
}

/// An action that should be taken based on this plugin's decision.
#[derive(Copy, Clone, Debug, PartialOrd, Ord, PartialEq, Eq)]
pub enum Action {
    /// Nothing special need be done.
    None,
    /// Log interesting information.
    Log,
    /// Save things.
    Save,
    /// Exit the program.
    Exit,
}
impl Action {
    /// Do both of two actions.
    pub fn and(self, other: Action) -> Action {
        ::std::cmp::max(self, other)
    }
}

/// A helper to enable Monte Carlo implementations to easily run their
/// plugins without duplicating code.
#[derive(Serialize, Deserialize, Debug)]
pub struct PluginManager {
    period: Cell<u64>,
    moves: Cell<u64>,
}

impl PluginManager {
    /// Create a plugin manager.
    pub fn new() -> PluginManager {
        PluginManager { period: Cell::new(1), moves: Cell::new(0) }
    }
    /// Run all the plugins, if needed.  This should always be called
    /// with the same set of plugins.  If you want different sets of
    /// plugins, use different managers.
    pub fn run<MC: MonteCarlo>(&self, mc: &MC, sys: &MC::System,
                               plugins: &[&Plugin<MC>]) {
        let moves = self.moves.get() + 1;
        self.moves.set(moves);
        if moves >= self.period.get() {
            self.moves.set(0);
            let mut todo = plugin::Action::None;
            for p in plugins.iter() {
                todo = todo.and(p.run(mc, sys));
            }
            if todo >= plugin::Action::Log {
                for p in plugins.iter() {
                    p.log(mc, sys);
                }
            }
            if todo >= plugin::Action::Save {
                mc.checkpoint();
                for p in plugins.iter() {
                    p.save(mc, sys);
                }
            }
            if todo >= plugin::Action::Exit {
                ::std::process::exit(0);
            }
            // run plugins every trillion iterations minimum
            let mut new_period = 1u64 << 40;
            for p in plugins.iter() {
                if let Some(period) = p.run_period() {
                    if period < new_period {
                        new_period = period;
                    }
                }
            }
            self.period.set(new_period);
        }
    }
}

fn no_time() -> Cell<Option<(time::Instant, u64)>> { Cell::new(None) }

/// A plugin that terminates the simulation after a fixed number of iterations.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Report {
    max_iter: Option<u64>,
    /// This is when and where the simulation started.
    #[serde(skip, default="no_time")]
    start: Cell<Option<(time::Instant, u64)>>,
}

/// The parameter to define the maximum number of iterations.
#[derive(ClapMe, Debug)]
pub struct ReportParams {
    /// The maximum number of iterations to run.
    pub max_iter: Option<u64>,
}

impl Default for ReportParams {
    fn default() -> Self {
        ReportParams {
            max_iter: None,
        }
    }
}

impl From<ReportParams> for Report {
    fn from(params: ReportParams) -> Self {
        Report {
            max_iter: params.max_iter,
            start: Cell::new(Some((time::Instant::now(), 0))),
        }
    }
}
fn duration_from_f64(seconds: f64) -> time::Duration {
    time::Duration::new(seconds as u64, (seconds*1e9) as u32)
}
impl<MC: MonteCarlo> Plugin<MC> for Report {
    fn run(&self, mc: &MC, _sys: &MC::System) -> Action {
        if let Some(maxiter) = self.max_iter {
            if mc.num_moves() >= maxiter {
                return Action::Exit;
            }
        }
        Action::None
    }
    fn run_period(&self) -> Option<u64> { self.max_iter }
    fn log(&self, mc: &MC, _sys: &MC::System) {
        match self.start.get() {
            Some((start_time, start_iter)) => {
                let moves = mc.num_moves();
                let runtime = start_time.elapsed();
                if let Some(max) = self.max_iter {
                    let time_per_move =
                        runtime.as_secs() as f64/(moves - start_iter) as f64;
                    let frac_complete = moves as f64/max as f64;
                    let moves_left = max - moves;
                    let time_left = time_per_move*moves_left as f64;
                    println!("[{}] {}% complete after {} ({} left)",
                             moves,
                             (100.*frac_complete) as isize,
                             ::humantime::format_duration(runtime),
                             ::humantime::format_duration(duration_from_f64(time_left)),
                    );
                } else {
                    println!("[{}] after {}",
                             moves,
                             ::humantime::format_duration(runtime),
                    );
                }
            }
            None => {
                self.start.set(Some((time::Instant::now(), mc.num_moves())));
            }
        }
    }
    fn save(&self, mc: &MC, _sys: &MC::System) {
        let rejects = mc.num_rejected_moves();
        let moves = mc.num_moves();
        println!("Rejected {}/{} = {}% of the moves",
                 rejects, moves, 100.0*rejects as f64/moves as f64);
    }
}


/// A plugin that schedules when to save
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Save {
    next_output: Cell<u64>,
    moves_left: Cell<u64>,
}

/// The parameter to define the save schedule
#[derive(ClapMe, Debug)]
pub struct SaveParams;

impl Default for SaveParams {
    fn default() -> Self { SaveParams }
}
impl From<SaveParams> for Save {
    fn from(_params: SaveParams) -> Self {
        Save {
            next_output: Cell::new(1),
            moves_left: Cell::new(1),
        }
    }
}
impl<MC: MonteCarlo> Plugin<MC> for Save {
    fn run(&self, mc: &MC, _sys: &MC::System) -> Action {
        let next_output = self.next_output.get();
        let moves = mc.num_moves();
        let action = if moves >= next_output {
            self.next_output.set(next_output*2);
            Action::Save
        } else {
            Action::None
        };
        self.moves_left.set(next_output - moves);
        action
    }
    fn run_period(&self) -> Option<u64> {
        Some(self.next_output.get())
    }
}
