//! The PC-control guard: a global Esc hook and the banner that says so.
//!
//! Computer use moves the real cursor and types into whatever has focus, which
//! means the app's own Esc handling is the wrong place to catch "stop": a DOM
//! keydown listener in the main window only fires when that window has focus,
//! and the whole time the agent is driving another application it doesn't.
//! So Esc is caught here, with a low-level keyboard hook that fires regardless
//! of which window is focused.
//!
//! What Esc does is deliberately NOT "cancel the run". Killing a run because
//! the user pressed Esc once loses whatever else that agent was doing, and the
//! user's objection is narrower than that: they disliked the agent driving
//! *their* screen. So an Esc during PC control aborts the current batch, shows
//! the banner as a warning, and hands the agent an explicit note that the user
//! objected — it can then carry on with the parts that don't need the mouse.
//! Two Escs inside a few seconds does end the run.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Label of the always-on-top banner window (see `lib.rs`).
pub const BANNER: &str = "pc-guard";

/// How long a single Esc stays "armed" before a second one stops the run.
/// Long enough to finish reading the banner, short enough that a deliberate
/// double-tap is still a double-tap.
const ESC_ARM_MS: u64 = 4000;

/// What the agent is told when the user presses Esc. Deliberately about intent
/// rather than mechanics: the model needs to know the user disliked the screen
/// takeover, so it stops taking it and finds another way — not that some token
/// was cancelled, which it would just retry around.
pub const ESC_NOTE: &str = "<system-reminder>The user pressed Esc while you were controlling their screen, and did not want you to. They are at the keyboard now: stop clicking, typing and taking over the screen, and don't try to resume it without asking them first. If you still need something from the screen, say what it is and let them decide. Carry on with everything else that doesn't need the mouse or keyboard.</system-reminder>";

// ───────────────────────────── banner state ─────────────────────────────

/// What the banner is currently showing. The frontend reads this over a Tauri
/// event; nothing polls.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BannerState {
    /// An agent is driving the mouse/keyboard right now.
    pub active: bool,
    /// The user objected: the banner stays up as a warning after the batch
    /// stops, instead of vanishing and leaving the agent to carry on silently.
    pub objected: bool,
    /// Short human summary of the batch, e.g. `click 400,120 → type "hi"`.
    pub summary: String,
    /// Chat the run belongs to, so Esc and Stop name the right one.
    pub task_id: String,
}

static STATE: Mutex<BannerState> = Mutex::new(BannerState {
    active: false,
    objected: false,
    summary: String::new(),
    task_id: String::new(),
});
/// Bumped on every change, so the UI can tell two identical states apart.
static REV: AtomicU64 = AtomicU64::new(0);

/// Mutex poisoning here is never worth propagating: this is display state, and
/// a panic in one tool should not make the banner permanently unavailable.
fn lock() -> std::sync::MutexGuard<'static, BannerState> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn set(f: impl FnOnce(&mut BannerState)) -> BannerState {
    let out = {
        let mut g = lock();
        f(&mut g);
        REV.fetch_add(1, Ordering::SeqCst);
        g.clone()
    };
    emit(&out);
    out
}

/// Push the state to the UI, and let the app show or hide the banner to match.
///
/// Deliberately no Tauri types in this module beyond the harness: a
/// `tauri::AppHandle` reachable from here forces the whole crate to be linked
/// into the test binary in a way that fails to load on Windows (the `windows`
/// crate's two major versions produce incompatible import libs, and only the
/// app binary, not the tests, tolerates that). The app subscribes to the event
/// below and owns the window instead — which is also the tidier split: this
/// module is policy, `lib.rs` is presentation.
fn emit(st: &BannerState) {
    if let Some(h) = harness() {
        (h.bus)(EVENT, serde_json::to_value(st).unwrap_or_default());
    }
}

/// The event the app listens on for banner state changes.
pub const EVENT: &str = "ol://pcguard";

/// The app's `Arc<Harness>`, set once at startup. `None` in tests and before
/// setup, which is exactly when there is nothing to draw anyway.
fn harness_cell() -> &'static OnceLock<std::sync::Arc<super::Harness>> {
    static H: OnceLock<std::sync::Arc<super::Harness>> = OnceLock::new();
    &H
}

fn harness() -> Option<std::sync::Arc<super::Harness>> {
    harness_cell().get().cloned()
}

/// Give the guard a handle to the harness. Called once during app setup.
pub fn set_harness(h: std::sync::Arc<super::Harness>) {
    let _ = harness_cell().set(h);
}

pub fn state() -> BannerState {
    lock().clone()
}

/// Show the banner for a batch that is about to run on the real desktop.
pub fn begin(task_id: &str, summary: &str) {
    *esc_task().lock().unwrap_or_else(|e| e.into_inner()) = Some(task_id.to_string());
    set(|s| {
        s.active = true;
        s.task_id = task_id.to_string();
        s.summary = summary.to_string();
        // A new batch clears the standing objection: the user let the agent
        // try again, and leaving the red state up would misreport this run.
        s.objected = false;
    });
}

/// The batch finished on its own. The banner goes away unless the user has
/// objected, in which case it stays as a warning.
pub fn end() {
    let objected = lock().objected;
    *esc_task().lock().unwrap_or_else(|e| e.into_inner()) = None;
    set(|s| {
        s.active = false;
        if !objected {
            s.summary.clear();
            s.task_id.clear();
        }
    });
}

/// The user pressed Esc while the agent had the mouse. Records the objection
/// and returns the chat to tell, if a run is still going.
pub fn objection() -> Option<String> {
    let st = set(|s| {
        s.active = false;
        s.objected = true;
    });
    (!st.task_id.is_empty()).then_some(st.task_id)
}

/// Clear the standing objection (new chat, or the run ended for good).
pub fn clear() {
    *esc_task().lock().unwrap_or_else(|e| e.into_inner()) = None;
    ESC_PENDING.store(false, Ordering::SeqCst);
    set(|s| {
        s.objected = false;
        s.summary.clear();
        s.task_id.clear();
    });
}

// ───────────────────────────── the global Esc hook ─────────────────────────────

/// Set while the low-level hook is installed. A `WH_KEYBOARD_LL` hook cannot
/// be uninstalled on Windows, so it stays for the process lifetime and is
/// gated on this instead. An idle hook that returns immediately costs nothing
/// and keeps Esc working when the next run starts.
static HOOKED: AtomicBool = AtomicBool::new(false);
/// The chat an Esc should be reported against. `None` while no run is driving.
static ESC_TASK: OnceLock<Mutex<Option<String>>> = OnceLock::new();
/// Set when an objection is waiting to be consumed by the runner. Touched
/// directly (no accessor): unlike `ESC_TASK` this needs no lazy init.
static ESC_PENDING: AtomicBool = AtomicBool::new(false);
/// Timestamp of the last Esc, for the "press twice to stop" rule.
static LAST_ESC: AtomicU64 = AtomicU64::new(0);

fn esc_task() -> &'static Mutex<Option<String>> {
    ESC_TASK.get_or_init(|| Mutex::new(None))
}

/// Install the global Esc hook. Safe to call more than once.
pub fn install_esc_hook() {
    if HOOKED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("ol-esc-hook".into())
        .spawn(hook_thread)
        .expect("can't start the Esc hook thread");
}

/// True when the agent should be told the user objected. Consumes the flag, so
/// the note lands on exactly one turn.
pub fn take_objection() -> bool {
    ESC_PENDING.swap(false, Ordering::SeqCst)
}

/// Whether this Esc is the second in a quick pair, meaning "stop the run".
pub fn second_esc() -> bool {
    let now = now_ms();
    let last = LAST_ESC.swap(now, Ordering::SeqCst);
    last != 0 && now.saturating_sub(last) < ESC_ARM_MS
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Handle one Esc from the hook thread. Returns true if the run should stop.
///
/// This runs on the hook thread, which must return fast and cannot await, so
/// the note is queued onto the async runtime rather than pushed into the inbox
/// here: a blocked hook is one Windows silently drops, leaving Esc dead.
fn on_esc() -> bool {
    let task = esc_task().lock().unwrap_or_else(|e| e.into_inner()).clone();
    // This process-wide hook remains installed while idle. Escape is only a
    // PC-control objection when a computer-use batch owns the screen; otherwise
    // ordinary app/startup Escape presses must not leave a ghost warning. If a
    // prior objection left its warning behind after that batch ended, Escape
    // dismisses it instead of arming a stop for a nonexistent run.
    let Some(task) = task else {
        if lock().objected {
            clear();
        }
        return false;
    };
    let stop = second_esc();
    ESC_PENDING.store(true, Ordering::SeqCst);
    objection();
    if let Some(h) = harness() {
        tauri::async_runtime::spawn(async move {
            h.note(&task, "main", ESC_NOTE.to_string()).await;
            if stop {
                super::runner::interrupt(&h, &task).await;
            }
        });
    }
    stop
}

// ─────────────────────── the hook itself (Windows) ───────────────────────

/// Runs the low-level keyboard hook, pumping messages for the process
/// lifetime. Windows requires the installing thread to pump, and a
/// `WH_KEYBOARD_LL` hook can only be removed by the thread that installed it —
/// so this thread never returns and the hook stays gated on `ESC_TASK`.
#[cfg(windows)]
fn hook_thread() {
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        // `SetWindowsHookExW` returns a `Result`: an `Err` here means no hook,
        // so Esc-while-unfocused is unavailable. The banner's Stop button and
        // the in-app Esc still work, so this is degraded, not broken.
        let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_proc), None, 0) else {
            return;
        };
        // Never dropped: a closed hook handle would silently unhook us. The
        // thread lives for the process, so the handle is simply left open.
        let _ = hook;
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(windows)]
extern "system" fn ll_proc(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        // A negative nCode means the hook is being asked something (removal);
        // only HC_ACTION is a real key event.
        if code == HC_ACTION as i32 && wparam.0 as u32 == WM_KEYDOWN {
            let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            // `on_esc` returns true for the second Esc, which ends the run.
            let _ = kb.vkCode == 0x1B && on_esc();
        }
        // Always let the Esc through — the point is to object, not to steal the
        // user's keypress from whatever they have focused.
        CallNextHookEx(None, code, wparam, lparam)
    }
}

/// No global keyboard hook off Windows: the banner's Stop button and the app's
/// own Esc still work, so computer use is guarded, just less conveniently.
#[cfg(not(windows))]
fn hook_thread() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// `banner_tracks_a_batch` and `objection_stays_as_a_warning` both read and
    /// write the one process-global `STATE`, so running them concurrently lets
    /// one test's `end()` land between the other's `begin()` and its `state()`.
    /// `clear()` also resets `ESC_PENDING`, which the Esc tests below assert on.
    /// That is a harness race, not a behaviour difference: a real banner has one
    /// agent driving it, and the runner already serialises `begin`/`end` per
    /// batch. Serialising the tests restores the property they mean to check
    /// without changing a line of the code under test.
    static STATE_TEST: Mutex<()> = Mutex::new(());

    /// Every test here touches process-global state, so they take this rather
    /// than only the two banner ones -- a partial fix just moves the flake.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        STATE_TEST.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The banner is what tells the user their PC is being driven. These are the
    /// states it must be able to show; a regression here is silent and nasty.
    #[test]
    fn banner_tracks_a_batch() {
        let _guard = guard();
        clear();
        begin("t1", "click 1,2");
        let s = state();
        assert!(s.active && !s.objected && s.task_id == "t1" && s.summary == "click 1,2");
        end();
        // A batch that ended on its own leaves nothing behind.
        let s = state();
        assert!(!s.active && !s.objected && s.task_id.is_empty() && s.summary.is_empty());
    }

    #[test]
    fn objection_stays_as_a_warning() {
        let _guard = guard();
        clear();
        begin("t2", "type hi");
        assert_eq!(
            objection().as_deref(),
            Some("t2"),
            "the runner needs the task to tell"
        );
        // Ended, but the objection must survive: the agent is still running and
        // the user needs to see that it was told to stop.
        end();
        let s = state();
        assert!(!s.active && s.objected && s.task_id == "t2");
        // A new batch is a fresh start and clears it.
        begin("t2", "click 3,4");
        let s = state();
        assert!(s.active && !s.objected);
        end();
        clear();
    }

    /// Two Escs in quick succession mean "stop", but a lone one only objects.
    #[test]
    fn double_esc_stops() {
        let _guard = guard();
        LAST_ESC.store(0, Ordering::SeqCst);
        assert!(!second_esc(), "the first Esc can only object");
        assert!(second_esc(), "the second, immediately after, stops the run");
        // A slow second press is a fresh objection, not a stop.
        LAST_ESC.store(now_ms().saturating_sub(ESC_ARM_MS + 500), Ordering::SeqCst);
        assert!(!second_esc());
    }

    #[test]
    fn idle_escape_does_not_show_a_pc_control_warning() {
        let _guard = guard();
        clear();
        LAST_ESC.store(0, Ordering::SeqCst);

        assert!(!on_esc(), "idle Escape must not stop a run");
        assert!(
            !take_objection(),
            "idle Escape must not notify a future run"
        );
        let s = state();
        assert!(!s.active && !s.objected && s.task_id.is_empty());
        assert_eq!(
            LAST_ESC.load(Ordering::SeqCst),
            0,
            "idle Escape must not arm double-Escape"
        );

        begin("stale", "click 1,2");
        assert!(objection().is_some());
        end();
        assert!(
            state().objected,
            "a completed objection should remain dismissible"
        );
        assert!(
            !on_esc(),
            "Escape dismisses an old warning but does not stop a run"
        );
        assert!(!state().objected && state().task_id.is_empty());
    }

    #[test]
    fn objection_is_delivered_once() {
        let _guard = guard();
        ESC_PENDING.store(true, Ordering::SeqCst);
        assert!(take_objection());
        assert!(
            !take_objection(),
            "the note must not land on every later turn"
        );
    }
}
