//! Per-call budgets on Luau's interrupt.
//!
//! Each call into a VM pushes a [`Frame`] with a deadline: its own budget from now, or the
//! enclosing call's deadline if that is earlier ("a nested call keeps the earlier deadline").
//! A process-wide watchdog thread raises the VM's [`Alarm`] when the innermost deadline passes.
//! The interrupt's fast path loads only a process-wide count of raised alarms, so a call within
//! budget pays one load of a static per safepoint. Once an alarm is up, the interrupt finds its
//! VM's clock, confirms the deadline against the clock, marks the frame tripped and raises
//! `tern: <hook> exceeded <n> ms`; a tripped frame raises at every later check in that call, so
//! `pcall` can't recover.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::os::raw::{c_int, c_void};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mlua::{Lua, ffi};

/// The budget class of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Budget {
    /// Host halves, including the entry: 2 s.
    Host,
    /// Window halves, including the entry: 50 ms.
    Window,
    /// Chrome formatters and `available`: 4 ms.
    Formatter,
}

impl Budget {
    /// The budget in milliseconds, as the trip message prints it.
    pub fn millis(self) -> u64 {
        match self {
            Budget::Host => 2000,
            Budget::Window => 50,
            Budget::Formatter => 4,
        }
    }

    /// The budget as a duration.
    pub fn duration(self) -> Duration {
        Duration::from_millis(self.millis())
    }
}

/// How many alarms are up across the process. The interrupt's fast path reads only this, so a
/// call within budget pays one load of a static per safepoint.
static RAISED: AtomicUsize = AtomicUsize::new(0);

/// The flag the watchdog raises for one VM. `generation` tells a current arming from a stale
/// one, so a call that ended is never tripped by its old deadline.
#[derive(Debug, Default)]
struct Alarm {
    expired: AtomicBool,
    generation: AtomicU64,
}

impl Alarm {
    fn arm(self: &Arc<Self>, deadline: Instant) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        if deadline <= Instant::now() {
            self.raise();
        } else {
            self.clear();
            Watchdog::global().schedule(deadline, Arc::clone(self), generation);
        }
    }

    fn disarm(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.clear();
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::Acquire) == generation
    }

    fn is_raised(&self) -> bool {
        self.expired.load(Ordering::Acquire)
    }

    fn raise(&self) {
        if !self.expired.swap(true, Ordering::AcqRel) {
            RAISED.fetch_add(1, Ordering::Release);
        }
    }

    fn clear(&self) {
        if self.expired.swap(false, Ordering::AcqRel) {
            RAISED.fetch_sub(1, Ordering::Release);
        }
    }
}

struct Entry {
    deadline: Instant,
    generation: u64,
    alarm: Arc<Alarm>,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline
    }
}
impl Eq for Entry {}
impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Entry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.deadline.cmp(&other.deadline)
    }
}

/// One thread for the whole process that raises alarms at their deadlines.
struct Watchdog {
    heap: Mutex<BinaryHeap<Reverse<Entry>>>,
    wake: Condvar,
}

impl Watchdog {
    fn global() -> &'static Watchdog {
        static WATCHDOG: OnceLock<&'static Watchdog> = OnceLock::new();
        WATCHDOG.get_or_init(|| {
            let watchdog: &'static Watchdog = Box::leak(Box::new(Watchdog {
                heap: Mutex::new(BinaryHeap::new()),
                wake: Condvar::new(),
            }));
            std::thread::Builder::new()
                .name("plugin budgets".into())
                .spawn(move || watchdog.run())
                .expect("cannot start the plugin budget watchdog");
            watchdog
        })
    }

    fn schedule(&self, deadline: Instant, alarm: Arc<Alarm>, generation: u64) {
        let mut heap = self.heap.lock().unwrap_or_else(|e| e.into_inner());
        let earliest = heap.peek().map(|Reverse(e)| e.deadline);
        heap.push(Reverse(Entry {
            deadline,
            generation,
            alarm,
        }));
        if earliest.is_none_or(|d| deadline < d) {
            self.wake.notify_one();
        }
    }

    /// Raises each live alarm at its deadline. macOS lets a timed wait overshoot by up to half
    /// its length (at most ~10 ms), so the watchdog waits half the remaining time, repeatedly,
    /// and spins through the last [`SPIN`]. Stale entries (their call ended) are dropped as soon
    /// as they reach the top.
    fn run(&self) {
        const SPIN: Duration = Duration::from_micros(100);
        let mut heap = self.heap.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            while heap
                .peek()
                .is_some_and(|Reverse(e)| !e.alarm.is_current(e.generation))
            {
                heap.pop();
            }
            let Some(Reverse(next)) = heap.peek() else {
                heap = self.wake.wait(heap).unwrap_or_else(|e| e.into_inner());
                continue;
            };
            let now = Instant::now();
            if next.deadline <= now {
                if let Some(Reverse(entry)) = heap.pop()
                    && entry.alarm.is_current(entry.generation)
                {
                    entry.alarm.raise();
                }
                continue;
            }
            let remaining = next.deadline - now;
            if remaining > SPIN {
                heap = self
                    .wake
                    .wait_timeout(heap, remaining / 2)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            } else {
                let deadline = next.deadline;
                drop(heap);
                while Instant::now() < deadline {
                    std::hint::spin_loop();
                }
                heap = self.heap.lock().unwrap_or_else(|e| e.into_inner());
            }
        }
    }
}

/// One call in progress.
#[derive(Debug)]
struct Frame {
    deadline: Instant,
    tripped: bool,
    message: String,
}

/// A VM's call stack of budgets. The interrupt reaches it through the main thread's
/// thread-data pointer.
#[derive(Debug, Default)]
pub(crate) struct CallClock {
    alarm: Arc<Alarm>,
    frames: RefCell<Vec<Frame>>,
}

impl CallClock {
    /// Installs the interrupt on `lua` and ties this clock to the VM. The VM keeps the clock
    /// alive in its app data for as long as the Lua state exists.
    pub(crate) fn install(lua: &Lua) -> mlua::Result<Rc<CallClock>> {
        let clock = Rc::new(CallClock::default());
        lua.set_app_data(Rc::clone(&clock));
        let data = Rc::as_ptr(&clock) as *mut c_void;
        // SAFETY: `exec_raw` hands us the VM's current state; we only set the main thread's
        // thread data (unused by mlua) and the interrupt callback (mlua's `set_interrupt` is
        // never used on this VM). The pointee lives in the state's app data, so it outlives
        // every interrupt.
        unsafe {
            lua.exec_raw::<()>((), |state| {
                ffi::lua_setthreaddata(ffi::lua_mainthread(state), data);
                (*ffi::lua_callbacks(state)).interrupt = Some(interrupt);
            })?;
        }
        Ok(clock)
    }

    /// Starts a call to `hook` under `budget`. Returns the frame depth to pass to [`Self::exit`].
    pub(crate) fn enter(&self, hook: &str, budget: Budget) -> usize {
        let mut frames = self.frames.borrow_mut();
        let mut deadline = Instant::now() + budget.duration();
        if let Some(outer) = frames.last() {
            deadline = deadline.min(outer.deadline);
        }
        frames.push(Frame {
            deadline,
            tripped: false,
            message: format!("tern: {hook} exceeded {} ms", budget.millis()),
        });
        self.alarm.arm(deadline);
        frames.len()
    }

    /// Ends the call started at `depth` (and any frame above it a panic left behind). Returns
    /// whether it tripped its budget.
    pub(crate) fn exit(&self, depth: usize) -> bool {
        let mut frames = self.frames.borrow_mut();
        frames.truncate(depth);
        let tripped = frames.pop().is_some_and(|f| f.tripped);
        match frames.last() {
            Some(outer) => self.alarm.arm(outer.deadline),
            None => self.alarm.disarm(),
        }
        tripped
    }

    /// The interrupt's slow path, taken once the alarm is up. Returns the message to raise, as a
    /// pointer and length into the innermost frame (valid until that frame is popped), or `None`
    /// if the alarm was stale.
    fn check(&self) -> Option<(*const u8, usize)> {
        let mut frames = self.frames.try_borrow_mut().ok()?;
        let Some(frame) = frames.last_mut() else {
            self.alarm.clear();
            return None;
        };
        if !frame.tripped {
            if Instant::now() < frame.deadline {
                // A stale raise that crossed a disarm; the real deadline is still ahead.
                self.alarm.clear();
                return None;
            }
            frame.tripped = true;
        }
        Some((frame.message.as_ptr(), frame.message.len()))
    }
}

impl Drop for CallClock {
    fn drop(&mut self) {
        self.alarm.disarm();
    }
}

/// Ends a call's frame even if the call unwinds.
pub(crate) struct FrameGuard<'a> {
    clock: &'a CallClock,
    depth: usize,
}

impl<'a> FrameGuard<'a> {
    pub(crate) fn enter(clock: &'a CallClock, hook: &str, budget: Budget) -> Self {
        FrameGuard {
            depth: clock.enter(hook, budget),
            clock,
        }
    }

    /// Ends the call; returns whether it tripped its budget.
    pub(crate) fn finish(self) -> bool {
        let tripped = self.clock.exit(self.depth);
        std::mem::forget(self);
        tripped
    }
}

impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        self.clock.exit(self.depth);
    }
}

/// Luau's interrupt: called at safepoints (loop back edges, calls and returns), and with
/// `gc >= 0` at GC steps, where raising isn't allowed. The fast path is kept free of register
/// spills; everything else is in [`interrupt_raised`].
unsafe extern "C-unwind" fn interrupt(state: *mut ffi::lua_State, gc: c_int) {
    if gc < 0 && RAISED.load(Ordering::Relaxed) != 0 {
        // SAFETY: forwarded from Luau's interrupt.
        unsafe { interrupt_raised(state) }
    }
}

/// The interrupt once some alarm in the process is up: raises if it is this VM's and its
/// deadline has passed.
#[cold]
#[inline(never)]
unsafe extern "C-unwind" fn interrupt_raised(state: *mut ffi::lua_State) {
    // SAFETY: the main thread's thread data is the VM's `CallClock` (see `install`), alive
    // while the state is.
    let clock = unsafe { ffi::lua_getthreaddata(ffi::lua_mainthread(state)) } as *const CallClock;
    if clock.is_null() {
        return;
    }
    // SAFETY: as above; only shared access, on the VM's own thread.
    let clock = unsafe { &*clock };
    if !clock.alarm.is_raised() {
        return;
    }
    let Some((message, len)) = clock.check() else {
        return;
    };
    // SAFETY: no Rust value with a destructor is live here, so the Lua error may unwind (or
    // longjmp) through this frame. The message points into the innermost frame, which stays
    // put until the call returns to `Vm::call`.
    unsafe {
        ffi::lua_rawcheckstack(state, 1);
        ffi::lua_pushlstring(state, message.cast(), len);
        ffi::lua_error(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets() {
        assert_eq!(Budget::Host.millis(), 2000);
        assert_eq!(Budget::Window.millis(), 50);
        assert_eq!(Budget::Formatter.millis(), 4);
    }

    #[test]
    fn watchdog_raises_only_the_current_generation() {
        let alarm = Arc::new(Alarm::default());
        alarm.arm(Instant::now() + Duration::from_millis(5));
        alarm.disarm();
        alarm.arm(Instant::now() + Duration::from_millis(30));
        std::thread::sleep(Duration::from_millis(15));
        assert!(!alarm.is_raised(), "the disarmed arming must not fire");
        std::thread::sleep(Duration::from_millis(40));
        assert!(alarm.is_raised());
        alarm.disarm();
    }

    #[test]
    fn nested_frames_keep_the_earlier_deadline() {
        let clock = CallClock::default();
        let outer = clock.enter("outer", Budget::Formatter);
        let inner = clock.enter("inner", Budget::Host);
        let (outer_deadline, inner_deadline) = {
            let frames = clock.frames.borrow();
            (frames[0].deadline, frames[1].deadline)
        };
        assert_eq!(inner_deadline, outer_deadline);
        assert!(!clock.exit(inner));
        assert!(!clock.exit(outer));
        assert!(!clock.alarm.is_raised());
    }
}
