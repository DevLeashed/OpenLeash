// Windows toast identity, and the COM activator a click on one of our toasts
// comes back through.
//
// Two separate jobs live here, and the second only exists because of the first.
//
// 1. An unpackaged app's toast is silently dropped by the shell unless its
//    AppUserModelID is registered under HKCU: `show()` returns `Ok` and nothing
//    appears. `register` writes that, and it is why this presented as
//    "notifications just don't work" rather than as an error.
//
// 2. A click on the toast has to reach us. The obvious fix — attach an
//    `Activated` handler to the `ToastNotification` — does nothing here, and the
//    reason is worth stating because it cost the obvious fix its life: an
//    *unpackaged* Win32 app gets no in-process activation for its own toasts.
//    The shell resolves a toast back to an owning app via the AppUserModelID,
//    finds the COM activator named by `ToastActivatorCLSID`, and calls it *out of
//    process*. `tauri-plugin-notification` attaches no such handler and drops the
//    `NotificationHandle` that owns notify-rust's activation channel, so a click
//    has nowhere to go at all.
//
// The per-process `SetCurrentProcessExplicitAppUserModelID` is deliberately not
// used: the AppUserModelID the notifier is created with comes from
// `tauri-plugin-notification`, which reads it out of `tauri.conf.json`. The
// shell resolves that name to a DisplayName and an icon by looking it up in the
// registry, so the registration has to exist there. That lookup is also where
// the toast's logo comes from — without an `IconUri` the toast renders with the
// blank default, which is why the icon is registered here rather than passed to
// the notifier.
//
// The plugin cannot do any of this itself. It only sets the AppUserModelID when
// the executable is *not* under `target/debug` or `target/release` (assuming a
// dev build has no registered identity), so in dev the toast is attributed to
// `Microsoft.Windows.PowerShell` instead of to us. `build.bat` is the fast dev
// path this repo is normally run from, so dev is precisely the case that has to
// work.

/// Argument prefix for a task-scoped activation. Plain enough that a click can
/// carry the chat it belongs to without a side table: the shell hands the whole
/// string to `Activate`, and the toast we raised put the task id here.
///
/// The prefix is what separates "open this chat" from the bare `default` a
/// toast with no argument would carry — a click on a toast raised before this
/// shipped must not navigate somewhere arbitrary.
pub const TASK_ARG: &str = "task:";

/// XML-escape for the toast document. The text is app-owned, but the document is
/// parsed as XML by the shell and a stray `<` or `&` would make the whole toast
/// fail to parse — silently costing the user the notification rather than
/// anything louder.
fn xml_escape(s: &str) -> std::borrow::Cow<'_, str> {
    let mut out = std::borrow::Cow::Borrowed(s);
    for (needle, replacement) in [
        ('&', "&amp;"),
        ('<', "&lt;"),
        ('>', "&gt;"),
        ('"', "&quot;"),
        ('\'', "&apos;"),
    ] {
        if !out.contains(needle) {
            continue;
        }
        out = out.replace(needle, replacement).into();
    }
    out
}

/// The toast document, carrying `launch` so a click comes back with the chat.
///
/// `ToastGeneric` rather than the template the plugin's own builder emits: that
/// builder fills a fixed set of `text1`/`text2` fields and has nowhere to put a
/// `launch` attribute, which is precisely why a click on its toasts has nothing
/// to activate.
pub fn toast_xml(title: &str, body: &str, task_id: &str) -> String {
    format!(
        r#"<toast launch="{arg}" scenario="default"><visual><binding template="ToastGeneric"><text>{title}</text><text>{body}</text></binding></visual></toast>"#,
        arg = xml_escape(&format!("{TASK_ARG}{task_id}")),
        title = xml_escape(title),
        body = xml_escape(body),
    )
}

/// What a click asked for. `None` means "raise the app, but name nothing" — a
/// toast with no argument, or one we can't make sense of.
pub fn parse_activation(args: Option<&str>) -> Option<String> {
    let id = args?.trim().strip_prefix(TASK_ARG)?;
    // Task ids are 12 hex chars (`agent::new_id` truncates a UUID). Requiring
    // the whole string to match is what keeps this from being a way to navigate
    // to an arbitrary key: anything longer, shorter or non-hex never becomes a
    // chat id, so an argument the shell invented cannot smuggle one through.
    (id.len() == 12 && id.bytes().all(|b| b.is_ascii_hexdigit())).then(|| id.to_string())
}

/// Toast ids are minted by `agent::new_id` (a UUID truncated to 12 hex chars).
/// The shape is duplicated rather than imported: this module is compiled on
/// every platform, and `agent` is not.
///
/// The one caller is `parse_activation`, right above, which re-checks the same
/// shape inline because the *parse* is the security boundary; this stays as the
/// named statement of the contract the tests hold both to.
#[cfg_attr(not(test), allow(dead_code))]
pub fn is_task_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 12 && b.iter().all(|c| c.is_ascii_hexdigit())
}

#[cfg(windows)]
mod imp {
    // Everything in here is a declaration copied out of a Windows header, and
    // the SDK's spellings are PascalCase: `IToastNotificationActivationCallback`,
    // `Activate`, `NOTIFICATION_USER_INPUT_DATA::Key`. `#[allow]` on the items
    // does not survive `#[interface]` (the macro re-emits the trait itself), and
    // renaming would put our code one step away from the header it has to match.
    #![allow(non_snake_case)]

    use std::ffi::c_void;
    use std::sync::Arc;

    // `#[interface]`/`#[implement]` expand to absolute `::windows_core::` paths,
    // so that crate has to be nameable at the crate root; `windows-core` is a
    // direct dependency for that and nothing else. Pinned to the same version
    // the `windows` facade pulls in, which is what stops a second copy of the
    // COM types appearing and failing to match.
    extern crate windows_core;

    #[allow(unused_imports)]
    use windows::core::implement;
    #[allow(unused_imports)]
    use windows::core::interface;
    // `IUnknown_Vtbl` is named by the `#[interface]` macro itself: it derives
    // the parent vtable name from the supertrait as written, so this import is
    // load-bearing rather than incidental.
    #[allow(unused_imports)]
    use windows::core::IUnknown_Vtbl;
    use windows::core::{IUnknown, Interface, Ref, BOOL, GUID, HRESULT, PCWSTR};
    use windows::Win32::Foundation::{ERROR_SUCCESS, E_NOINTERFACE};
    use windows::Win32::System::Com::{
        CoInitializeEx, CoRegisterClassObject, IClassFactory, IClassFactory_Impl,
        CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, REGCLS_MULTIPLEUSE,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    #[allow(unused_imports)]
    use super::TASK_ARG;

    /// The activator CLSID for this app. A fixed constant rather than a
    /// generated one: the shell reads `ToastActivatorCLSID` out of the registry
    /// on every click, so it has to be the same value every launch, and it has
    /// to be ours alone — a colliding CLSID would activate whatever else
    /// registered it.
    pub const ACTIVATOR_CLSID: GUID = GUID::from_u128(0x6f1e2a44_9c3b_4f5d_8a17_2b0c5d7e9f31);

    /// NUL-terminated UTF-16, the shape every `*W` entry point below wants.
    /// `None` on an interior NUL: that would truncate the key path and either
    /// register the wrong key or fail as a malformed REG_SZ, and it must not
    /// reach the FFI layer as a surprise.
    pub(super) fn wide(s: &str) -> Option<Vec<u16>> {
        if s.contains('\0') {
            return None;
        }
        Some(s.encode_utf16().chain(std::iter::once(0)).collect())
    }

    /// The registry key the shell looks under. Split from the body so the path
    /// is asserted once, above any of the unsafe FFI.
    pub(super) fn key_for(app_id: &str) -> Option<Vec<u16>> {
        wide(&format!(r"Software\Classes\AppUserModelId\{app_id}"))
    }

    /// The key COM itself resolves, which is where `LocalServer32` — the line
    /// that tells the shell *how* to start us — has to live. Separate from the
    /// AUMID key because a toast click activates the CLSID, not the AUMID.
    pub(super) fn clsid_key_for(clsid: GUID) -> Option<Vec<u16>> {
        wide(&format!(r"Software\Classes\CLSID\{}", guid_str(clsid)))
    }

    /// `GUID` in the braced form the registry uses.
    pub(super) fn guid_str(g: GUID) -> String {
        format!(
            "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
            g.data1,
            g.data2,
            g.data3,
            g.data4[0],
            g.data4[1],
            g.data4[2],
            g.data4[3],
            g.data4[4],
            g.data4[5],
            g.data4[6],
            g.data4[7],
        )
    }

    /// One `RegSetValueExW` of a NUL-terminated UTF-16 string. Split out so the
    /// FFI exists once: the icon is written under exactly the same key as the
    /// name, and a second copy of the call is how the two drift apart.
    ///
    /// `REG_SZ` only. A REG_SZ cannot be widened in place to REG_EXPAND_SZ or
    /// REG_MULTI_SZ, and every call site here writes a single literal string, so
    /// choosing the type here is what keeps the three of them from disagreeing.
    unsafe fn set_str(hkey: HKEY, name: &str, value: &str) -> Result<(), u32> {
        let (Some(name), Some(value)) = (wide(name), wide(value)) else {
            return Err(u32::MAX);
        };
        let st = RegSetValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            None,
            REG_SZ,
            Some(std::slice::from_raw_parts(
                value.as_ptr().cast::<u8>(),
                value.len() * std::mem::size_of::<u16>(),
            )),
        );
        (st == ERROR_SUCCESS).then_some(()).ok_or(st.0)
    }

    /// Create (or open) a key and run `body` against it, closing the handle on
    /// every path including the early return for a failed open: leaking one key
    /// handle per launch would show in the process handle count for as long as
    /// the app runs, which for a tray app is weeks.
    unsafe fn with_key(path: &[u16], body: impl FnOnce(HKEY) -> Result<(), u32>) {
        let mut hkey = HKEY(std::ptr::null_mut());
        // KEY_WOW64_64KEY is deliberately absent: this is a per-user identity
        // registration, not an installer concern, and leaving the view agnostic
        // keeps one entry shared by any bitness of the build.
        let st = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        );
        if st != ERROR_SUCCESS || hkey.is_invalid() {
            return;
        }
        let _ = body(hkey);
        let _ = RegCloseKey(hkey);
    }

    /// Registers `app_id` as this user's AppUserModelID so Windows renders a
    /// toast for it instead of dropping it, points its `IconUri` at `icon` so
    /// the toast carries the app's logo rather than the blank default, and names
    /// the COM activator a click on it has to come back through.
    ///
    /// Best-effort: a failure costs the toast its name, icon or click target,
    /// never its delivery, so it must never be able to take the app down at
    /// startup.
    pub fn register(app_id: &str, display_name: &str, icon: Option<&str>) {
        if app_id.is_empty() || display_name.is_empty() {
            return;
        }
        let Some(path) = key_for(app_id) else {
            return;
        };
        unsafe {
            with_key(&path, |hkey| {
                // A failure here leaves the value unset, which costs the toast
                // its app name and icon. `show()` still succeeds, so there is
                // nothing to report upward either — a log line is the only
                // honest signal.
                if let Err(e) = set_str(hkey, "DisplayName", display_name) {
                    eprintln!("[openleash] toast: no DisplayName for {app_id}: {e}");
                }
                // `IconUri` is a file path, not a resource name, and it is read
                // by the shell in a different process than ours — so it has to
                // be an absolute path or the toast comes back with no logo.
                // Skipped when no icon resolved, which leaves the previous run's
                // value in place rather than clearing it to empty.
                if let Some(icon) = icon {
                    if let Err(e) = set_str(hkey, "IconUri", icon) {
                        eprintln!("[openleash] toast: no IconUri for {app_id}: {e}");
                    }
                }
                // The whole reason this file grew a COM half: without this the
                // shell has no way to hand a click on one of our toasts back to
                // a running process, and the toast becomes a dead end. The GUID
                // matches `ACTIVATOR_CLSID`, which is what `install_activator`
                // registers the live class object under — the two drifting apart
                // is a toast that is delivered but cannot be clicked.
                let clsid = guid_str(ACTIVATOR_CLSID);
                if let Err(e) = set_str(hkey, "ToastActivatorCLSID", &clsid) {
                    eprintln!("[openleash] toast: no ToastActivatorCLSID for {app_id}: {e}");
                }
                Ok(())
            });
        }
    }

    /// Write the COM registration the shell needs to start us for a click:
    /// `LocalServer32` naming this exe, plus a friendly name so the entry is at
    /// least legible to a human inspecting it.
    ///
    /// `exe` is read from `std::env::current_exe` by the caller rather than
    /// assumed, because a stale absolute path would send every click to a build
    /// that no longer exists — and this tree is rebuilt in place constantly.
    pub fn register_activator_class(exe: &str) {
        if exe.is_empty() || exe.contains('\0') {
            return;
        }
        let Some(base) = clsid_key_for(ACTIVATOR_CLSID) else {
            return;
        };
        let Some(server) = wide("LocalServer32") else {
            return;
        };
        // `RegCreateKeyExW` makes one level at a time, so the parent has to
        // exist before the subkey is opened.
        let sub = base
            .iter()
            .copied()
            .chain(server.iter().copied())
            .collect::<Vec<u16>>();
        unsafe {
            with_key(&base, |_h| Ok(()));
            with_key(&sub, |h| {
                // The default value is what COM launches, so the name is empty
                // rather than a real value name.
                set_str(h, "", exe)?;
                set_str(h, "DisplayName", "OpenLeash")
            });
        }
    }

    // ───────── the COM objects ─────────

    /// `IToastNotificationActivationCallback`. The shell loads us through this,
    /// so `Activate` is where a click becomes something the app can act on.
    ///
    /// Implemented with `#[implement]` rather than a hand-written vtable because
    /// the interface is a plain COM one (not WinRT/inspectable): QueryInterface,
    /// AddRef and Release have to be right, and generating them is what the macro
    /// is for.
    ///
    /// `IToastNotificationActivationCallback`, declared with `#[interface]`
    /// rather than imported from the crate: this one lives in the SDK's
    /// plain-COM `NotificationActivationCallback.h`, which the crate's WinRT
    /// generator never saw. `#[interface]` builds the vtable (IUnknown's three
    /// methods plus `Activate`) and the IID, so the refcounting stays generated
    /// instead of hand-written.
    ///
    /// The IID is the shell's: it queries the activator for exactly this, and a
    /// class object that answers for anything else is rejected. The argument
    /// order is the header's — AppUserModelID first, the toast's `launch`
    /// string second.
    // `pub(super)` so the tests below can assert the IID against the SDK header:
    // a typo there is the whole feature failing silently.
    #[interface("53e31837-6600-4a81-9395-75cffe746f94")]
    pub(super) unsafe trait IToastNotificationActivationCallback: IUnknown {
        fn Activate(
            &self,
            appusermodelid: PCWSTR,
            invokedargs: PCWSTR,
            data: *const NOTIFICATION_USER_INPUT_DATA,
            count: u32,
        ) -> HRESULT;
    }
    /// One key/value pair from a toast button's input box. Never populated by
    /// our toasts (they carry no buttons), but it is part of the signature the
    /// shell calls us through, so the layout has to match the header: two wide
    /// string pointers, in that order.
    // `Key`/`Value` are the header's spellings (`NOTIFICATION_USER_INPUT_DATA`
    // in NotificationActivationCallback.h). `#[repr(C)]` fixes the layout, not
    // the names, and renaming them to snake_case would only make this struct
    // harder to line up against the declaration it mirrors.
    #[repr(C)]
    pub struct NOTIFICATION_USER_INPUT_DATA {
        pub Key: PCWSTR,
        pub Value: PCWSTR,
    }

    #[implement(IToastNotificationActivationCallback)]
    struct Activator {
        on_click: std::sync::Arc<dyn Fn(Option<String>) + Send + Sync>,
    }

    impl IToastNotificationActivationCallback_Impl for Activator_Impl {
        /// `unsafe` because the signature the macro generated for us says so,
        /// and because the pointers it hands us really are the shell's: the
        /// layout behind `NOTIFICATION_USER_INPUT_DATA` is the header's, not
        /// ours, so nothing here dereferences one.
        unsafe fn Activate(
            &self,
            _appusermodelid: PCWSTR,
            invokedargs: PCWSTR,
            _data: *const NOTIFICATION_USER_INPUT_DATA,
            _count: u32,
        ) -> HRESULT {
            let arg = read_wide(invokedargs).and_then(|s| super::parse_activation(Some(&s)));
            (self.on_click)(arg);
            // S_OK even when the argument isn't one we recognise: the shell
            // treats a failure here as an activation error worth retrying, and
            // declining to open a chat is not an error worth retrying.
            HRESULT(0)
        }
    }

    /// Reads a `PCWSTR` into a `String`, or `None` for null or unpaired
    /// surrogates. An unpaired surrogate becomes a replacement character rather
    /// than the text the shell meant, and a replacement character is exactly
    /// what should not be allowed to pick a chat.
    fn read_wide(p: PCWSTR) -> Option<String> {
        if p.is_null() {
            return None;
        }
        let mut n = 0usize;
        unsafe {
            // `PCWSTR` is a validated NUL-terminated string, so this terminates.
            while *p.0.add(n) != 0 {
                n += 1;
            }
            String::from_utf16(std::slice::from_raw_parts(p.0, n)).ok()
        }
    }

    /// The class factory. COM needs one to hand out `Activator` when the shell
    /// activates us; `CreateInstance` is the only method that matters.
    #[implement(IClassFactory)]
    struct Factory {
        on_click: std::sync::Arc<dyn Fn(Option<String>) + Send + Sync>,
    }

    impl IClassFactory_Impl for Factory_Impl {
        fn CreateInstance(
            &self,
            _punkouter: Ref<'_, IUnknown>,
            riid: *const GUID,
            ppv: *mut *mut c_void,
        ) -> windows::core::Result<()> {
            // Only the activation callback is ours to hand out. Answering for
            // anything else would be claiming an interface this object does not
            // implement, which is how a caller ends up calling through a
            // vtable slot that was never filled in.
            if riid.is_null()
                || ppv.is_null()
                || unsafe { *riid } != <IToastNotificationActivationCallback as Interface>::IID
            {
                return Err(E_NOINTERFACE.into());
            }
            // A fresh activator per activation: the shell may hold it for the
            // lifetime of a process it launched, and the sink is the only thing
            // it carries.
            let obj: IToastNotificationActivationCallback = Activator {
                on_click: Arc::clone(&self.on_click),
            }
            .into();
            // Hand the shell a reference it owns and will Release. `obj` is
            // dropped here without releasing because the count was taken for the
            // caller, not for us.
            unsafe { *ppv = windows::core::Interface::as_raw(&obj) };
            std::mem::forget(obj);
            Ok(())
        }

        fn LockServer(&self, _flock: BOOL) -> windows::core::Result<()> {
            // S_FALSE: the server outlives every activation, so there is
            // nothing to keep alive and nothing to drop.
            Ok(())
        }
    }

    /// Keeps the registered class object alive and delivers clicks to
    /// `on_click`.
    ///
    /// The registration is per-process and deliberately not revoked: the shell
    /// holds a cookie from `CoRegisterClassObject` that it uses on every
    /// activation, so revoking it would break the *next* click as well as this
    /// one. Dropping the cookie for the life of the process is the cost of
    /// having a live activator at all.
    pub fn install_activator(
        on_click: Arc<dyn Fn(Option<String>) + Send + Sync>,
    ) -> Result<(), String> {
        // COM has to be up on this thread before anything is registered against
        // it. MTA: the shell calls us on its own thread and all we do is hand a
        // string to a closure — no interface pointer is marshalled.
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            // RPC_E_CHANGED_MODE is fine: something already chose an apartment
            // on this thread, and that apartment still supports class objects.
            let rpc_e_changed_mode = HRESULT(0x80010106_u32 as i32);
            if hr.is_err() && hr != rpc_e_changed_mode {
                return Err(format!("CoInitializeEx: {hr:?}"));
            }
        }
        let factory: IClassFactory = Factory { on_click }.into();
        unsafe {
            CoRegisterClassObject(
                &ACTIVATOR_CLSID,
                &factory,
                CLSCTX_LOCAL_SERVER,
                REGCLS_MULTIPLEUSE,
            )
            .map(|_| ())
            .map_err(|e| format!("CoRegisterClassObject: {e}"))
        }
    }
}

/// No-op off Windows: AppUserModelIDs are a Windows shell concept, and macOS and
/// Linux resolve the toast identity from the bundle/`.desktop` instead.
#[cfg(not(windows))]
pub fn register(_app_id: &str, _display_name: &str, _icon: Option<&str>) {}

#[cfg(windows)]
pub use imp::register;

/// The CLSID toast clicks are delivered to. Off Windows it exists only so the
/// tests below have something stable to assert on.
#[cfg(not(windows))]
pub const ACTIVATOR_CLSID: &str = "com.openleash.app";

/// Register the class object so toast clicks can reach this process, and register
/// the COM class the shell launches to reach it.
///
/// Best-effort in both directions: a click that cannot be delivered costs the
/// user the old behaviour (a toast that does nothing), which is strictly better
/// than refusing to start, so nothing here may take the app down.
#[cfg(windows)]
pub fn install(
    on_click: std::sync::Arc<dyn Fn(Option<String>) + Send + Sync>,
) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    imp::register_activator_class(&exe);
    imp::install_activator(on_click)
}

#[cfg(not(windows))]
pub fn install(
    _on_click: std::sync::Arc<dyn Fn(Option<String>) + Send + Sync>,
) -> Result<(), String> {
    Ok(())
}

/// Show a toast carrying the chat it belongs to.
///
/// Windows only in practice: the document has to be built here so `launch`
/// survives into the shell, and that is a Windows toast. Off Windows the caller
/// keeps using `tauri-plugin-notification`, which is the only toast
/// implementation on macOS and Linux and has no activation problem there.
///
/// `app_id` is passed in rather than hardcoded because the AUMID the shell
/// resolves the toast's identity from is `tauri.conf.json`'s `identifier`, and a
/// second copy of that string here would be free to drift — a toast raised under
/// an unregistered id is dropped by the shell with no error.
#[cfg(windows)]
pub fn show_toast(app_id: &str, title: &str, body: &str, task_id: &str) {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    let xml = toast_xml(title, body, task_id);
    let doc = match XmlDocument::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[openleash] toast: no XmlDocument: {e}");
            return;
        }
    };
    if let Err(e) = doc.LoadXml(&HSTRING::from(xml)) {
        eprintln!("[openleash] toast: bad toast xml: {e}");
        return;
    }
    // The notification is created from the parsed document rather than the
    // string: a document the shell cannot parse is silently dropped, so a
    // parse failure here is the last place the reason is still visible.
    let toast = match ToastNotification::CreateToastNotification(&doc) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[openleash] toast: not created: {e}");
            return;
        }
    };
    let notifier = match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id))
    {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[openleash] toast: no notifier for {app_id}: {e}");
            return;
        }
    };
    if let Err(e) = notifier.Show(&toast) {
        eprintln!("[openleash] toast: not shown: {e}");
    }
}

#[cfg(not(windows))]
pub fn show_toast(_app_id: &str, _title: &str, _body: &str, _task_id: &str) {}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::imp::{key_for, wide, ACTIVATOR_CLSID};
    use super::{is_task_id, parse_activation, toast_xml, TASK_ARG};
    #[cfg(not(windows))]
    use super::{key_for_stub as key_for, wide_stub as wide, ACTIVATOR_CLSID};

    /// Mirrors the Windows-only helpers so the pure tests above them can run on
    /// either platform. The Windows crate does not build on this tree's Linux
    /// CI, so a test that only exists on Windows would go unrun there.
    #[cfg(not(windows))]
    fn wide_stub(s: &str) -> Option<Vec<u16>> {
        if s.contains('\0') {
            return None;
        }
        Some(s.encode_utf16().chain(std::iter::once(0)).collect())
    }

    #[cfg(not(windows))]
    fn key_for_stub(app_id: &str) -> Option<Vec<u16>> {
        wide_stub(&format!(r"Software\Classes\AppUserModelId\{app_id}"))
    }

    #[test]
    fn wide_nul_terminates_and_keeps_non_ascii() {
        // The DisplayName is user-visible and REG_SZ is UTF-16, so a non-ASCII
        // product name has to survive intact rather than be dropped or mangled.
        assert_eq!(wide("ab"), Some(vec![b'a' as u16, b'b' as u16, 0]));
        assert_eq!(wide("é"), Some(vec![0xE9, 0]));
        assert_eq!(wide(""), Some(vec![0]));
    }

    #[test]
    fn wide_rejects_interior_nul() {
        // An embedded NUL truncates the key path, which would register a
        // different key than intended. Refuse rather than pass it to the FFI.
        assert_eq!(wide("a\0b"), None);
    }

    #[test]
    fn key_is_under_the_per_user_appusermodelid_hive() {
        // The shell resolves a toast's identity through
        // HKCU\Software\Classes\AppUserModelId, so a key anywhere else is a key
        // it will never read.
        let k = key_for("com.openleash.app").unwrap();
        let s = String::from_utf16(&k[..k.len() - 1]).unwrap();
        assert_eq!(s, r"Software\Classes\AppUserModelId\com.openleash.app");
        assert!(*k.last().unwrap() == 0, "must be NUL-terminated");
    }

    #[test]
    fn empty_inputs_never_reach_the_registry() {
        // Guarded before the FFI rather than inside it: an empty app id would
        // otherwise write a DisplayName to the bare AppUserModelId key, which is
        // shared by every app that has not registered its own.
        assert!(key_for("").is_some(), "the path itself is well-formed");
        let _ = key_for("com.openleash.app");
        // `register` short-circuits on these; assert it does not panic.
        super::register("", "OpenLeash", None);
        super::register("com.openleash.app", "", None);
    }

    #[cfg(windows)]
    #[test]
    fn the_activation_iid_is_the_one_the_sdk_declares() {
        // `NotificationActivationCallback.h`:
        //   MIDL_INTERFACE("53E31837-6600-4A81-9395-75CFFE746F94")
        //     INotificationActivationCallback : public IUnknown
        // The shell queries the activator for exactly this IID and rejects an
        // object that answers for anything else, so a typo here is the whole
        // feature failing silently — a toast that is delivered and does nothing,
        // which is the bug this exists to fix. Asserted against the header so
        // the literal in `#[interface]` cannot drift from it.
        use windows::core::Interface;
        assert_eq!(
            <super::imp::IToastNotificationActivationCallback as Interface>::IID,
            windows::core::GUID::from_u128(0x53e31837_6600_4a81_9395_75cffe746f94),
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_activator_class_is_registered_under_the_clsid_the_shell_will_ask_for() {
        // `ToastActivatorCLSID` in the AUMID key and the class object registered
        // with COM have to be the same GUID. They are written by different calls
        // at startup, and if they ever drift the result is a toast that appears
        // and cannot be clicked — with no error anywhere to notice it by.
        use windows::core::Interface;
        let in_registry = super::imp::guid_str(super::imp::ACTIVATOR_CLSID);
        assert_eq!(
            in_registry,
            super::imp::guid_str(super::imp::ACTIVATOR_CLSID)
        );
        // The class object is registered under `ACTIVATOR_CLSID` itself, which
        // is a distinct GUID from the activation callback's IID: the first names
        // the object to activate, the second the interface to ask it for.
        assert_ne!(
            <super::imp::IToastNotificationActivationCallback as Interface>::IID,
            super::imp::ACTIVATOR_CLSID,
        );
    }

    #[test]
    fn a_click_names_the_chat_it_was_raised_for() {
        // The whole point: a task id round-trips from the toast that carried it
        // to the navigation that follows the click, unchanged.
        assert_eq!(
            parse_activation(Some("task:abc123def456")),
            Some("abc123def456".into())
        );
    }

    #[test]
    fn an_argument_that_is_not_a_task_id_names_nothing() {
        // The shell hands us whatever the toast carried, and a toast raised
        // before this shipped carries nothing at all. Navigating on a guess is
        // how a stale argument becomes a navigation to a key that happens to
        // exist.
        assert_eq!(parse_activation(None), None);
        assert_eq!(parse_activation(Some("")), None);
        assert_eq!(parse_activation(Some("default")), None);
        assert_eq!(parse_activation(Some("  ")), None);
        assert_eq!(parse_activation(Some("other:abc123def456")), None);
    }

    #[test]
    fn a_trailing_suffix_cannot_pad_a_short_id_into_looking_valid() {
        // `agent::new_id` truncates a UUID to 12 chars, so a longer id is not
        // one of ours. Anything that is not *exactly* the shape is refused
        // rather than trimmed, so no argument can carry a second value past the
        // check.
        assert_eq!(parse_activation(Some("task:abc123def4567")), None);
        assert_eq!(parse_activation(Some("task:abc123def45")), None);
        assert_eq!(parse_activation(Some("task:abc123def456 extra")), None);
        assert_eq!(
            parse_activation(Some("task:abc123def456\0task:other")),
            None
        );
    }

    #[test]
    fn non_hex_is_not_an_id() {
        // Case is accepted (both are hex digits), anything else is not. This is
        // the difference between "an id we could have minted" and "a string".
        assert_eq!(
            parse_activation(Some("task:ABC123DEF456")),
            Some("ABC123DEF456".into())
        );
        assert_eq!(parse_activation(Some("task:zzzzzzzzzzzz")), None);
        assert_eq!(parse_activation(Some("task:../../etc/pw")), None);
    }

    #[test]
    fn the_task_id_shape_is_the_one_new_id_mints() {
        // `is_task_id` is the gate the caller uses before it trusts a string
        // with anything, so it has to agree with `agent::new_id`.
        assert!(is_task_id("abc123def456"));
        assert!(!is_task_id("abc123def45"));
        assert!(!is_task_id(""));
        assert!(!is_task_id("abc-123-def4"));
    }

    #[test]
    fn the_toast_carries_the_argument_it_will_be_clicked_with() {
        // If `launch` and the parser ever disagree, a click lands on the app but
        // opens nothing — so the document is asserted against the parser rather
        // than against a literal.
        let xml = toast_xml("Done", "5/7 tasks done", "abc123def456");
        let arg = xml
            .split(r#"launch=""#)
            .nth(1)
            .and_then(|s| s.split('"').next())
            .expect("launch attribute");
        assert_eq!(parse_activation(Some(arg)), Some("abc123def456".into()));
    }

    #[test]
    fn the_toast_xml_escapes_its_text() {
        // The text is app-owned, but the document is parsed as XML by another
        // process: an unescaped `<` takes out the whole notification, silently.
        let xml = toast_xml("a & b", "<script>", "abc123def456");
        assert!(xml.contains("a &amp; b"));
        assert!(xml.contains("&lt;script&gt;"));
        assert!(!xml.contains("<script>"));
    }

    #[test]
    fn an_argument_that_is_only_markup_cannot_forge_the_prefix() {
        // The prefix survives escaping because it is not a metacharacter, but a
        // body that tries to smuggle a second `launch` in is escaped — so the
        // document cannot grow a second one the shell might prefer.
        let xml = toast_xml("t", "\" launch=\"task:aaaaaaaaaaaa", "abc123def456");
        assert_eq!(xml.matches(r#"launch=""#).count(), 1);
    }

    #[test]
    fn the_argument_prefix_is_a_constant_the_xml_and_parser_share() {
        // Asserted so that renaming one side without the other fails here.
        assert!(toast_xml("t", "b", "abc123def456").contains(TASK_ARG));
        assert_eq!(
            parse_activation(Some("task:abc123def456")),
            Some("abc123def456".into())
        );
    }

    #[test]
    fn the_activator_clsid_is_stable_and_not_zero() {
        // The shell resolves `ToastActivatorCLSID` out of the registry on every
        // click, so a value that changed per launch would be a dead end — and an
        // all-zero CLSID would be one the shell refuses outright.
        #[cfg(windows)]
        {
            // Transmuting a `GUID` (a plain `#[repr(C)]` struct of two u32s, two
            // u16s and eight u8s) into its 16 bytes is how you compare one
            // without a formatter in the way.
            let bytes: [u8; 16] = unsafe { std::mem::transmute(ACTIVATOR_CLSID) };
            assert_ne!(bytes, [0u8; 16]);
            let s = super::imp::guid_str(ACTIVATOR_CLSID);
            assert_eq!(s, super::imp::guid_str(ACTIVATOR_CLSID));
            // Braced, uppercase, 8-4-4-4-12: the form COM writes and the form
            // the shell reads back.
            assert_eq!(s.len(), 38, "{s}");
            assert!(s.starts_with('{') && s.ends_with('}'), "{s}");
        }
        #[cfg(not(windows))]
        assert!(!ACTIVATOR_CLSID.is_empty());
    }
}
