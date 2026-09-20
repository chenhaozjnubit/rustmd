//! Receiving documents from Finder.
//!
//! A bundled macOS app is not launched with the double-clicked file in `argv`:
//! Launch Services sends a `kAEOpenDocuments` Apple Event to the running
//! application instead. Without a handler for it, being listed under Finder's
//! "打开方式" would open an empty window — worse than not being listed at all.
//!
//! The event arrives on the main thread and is parked in a queue that the egui
//! update loop drains, so the rest of the program never has to care whether a
//! path came from the command line, a drop, or the Finder.
//!
//! There are two doors a document can come through and both are wired up here:
//! the shared Apple Event manager (for a file opened while rustmd is already
//! running) and three optional methods on the application delegate (for a file
//! that arrives while rustmd is being launched). See the comment above
//! `hook_set_delegate` for why neither one alone is enough.
//!
//! Nothing here is required for the app to run: if the handler cannot be
//! installed, opening files from inside rustmd works exactly as before.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{define_class, msg_send, ClassType};
use objc2_foundation::{NSAppleEventDescriptor, NSArray, NSObject, NSString, NSURL};

/// Documents Launch Services has handed us, waiting to be opened.
static INBOX: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The event handler object, as a raw pointer.
///
/// `NSAppleEventManager` does not retain the object it calls back into, so the
/// one instance has to outlive the process — hence the leak, and hence a raw
/// pointer rather than a `OnceLock<Retained<_>>` (which a static would not
/// accept, since the class is not `Send`).
static HANDLER: AtomicUsize = AtomicUsize::new(0);

/// `kCoreEventClass` = `'aevt'`.
const CORE_EVENT_CLASS: u32 = 0x6165_7674;
/// `kAEOpenDocuments` = `'odoc'`.
const OPEN_DOCUMENTS: u32 = 0x6f64_6f63;
/// `kAEOpenApplication` = `'oapp'`.
const OPEN_APPLICATION: u32 = 0x6f61_7070;
/// `keyDirectObject` = `'----'`.
const KEY_DIRECT_OBJECT: u32 = 0x2d2d_2d2d;

/// Take the paths Finder has asked us to open since the last call.
pub fn take_opened() -> Vec<PathBuf> {
    INBOX.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

// The object the Apple Event manager calls back into.
define_class!(
    // SAFETY: NSObject has no subclassing requirements, this class carries no
    // ivars, and it does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "RustmdOpenHandler"]
    struct OpenHandler;

    impl OpenHandler {
        /// `- (void)handleOpenDocs:(NSAppleEventDescriptor *)event
        ///          withReplyEvent:(NSAppleEventDescriptor *)replyEvent`
        #[unsafe(method(handleOpenDocs:withReplyEvent:))]
        fn handle_open_docs(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            // SAFETY: the descriptor came from the event manager and is only
            // read.
            let paths = unsafe { paths_in_event(event) };
            accept(
                paths,
                &format!(
                    "event {} / {}",
                    fourcc(unsafe { msg_send![event, eventClass] }),
                    fourcc(unsafe { msg_send![event, eventID] }),
                ),
            );
        }
    }
);

/// Install the handler for `kAEOpenDocuments`.
///
/// Safe to call more than once; the handler object is created on the first call
/// and reused after that.
///
/// Where this needs to run is the whole difficulty. There are two launch
/// states to cover:
///
/// * **Already running** — the event is dispatched through the shared manager,
///   so registering any time before it arrives is enough.
/// * **Cold start** — `open -a rustmd foo.md`, or a double-click in Finder,
///   makes Launch Services *launch* the app and hand it the `odoc` event during
///   start-up. AppKit dispatches that event while it is finishing its launch,
///   which happens **before** `App::new` is called, so registering there was
///   silently too late and the file was dropped.
///
/// The constructor below therefore registers as soon as the executable image is
/// loaded — before `main`, before `NSApplication` exists. The manager survives
/// application start-up and keeps the handler, so one early registration covers
/// both cases. `App::new` calls this again as a cheap safety net.
pub fn install() {
    // SAFETY: the Apple Event manager and the handler are both main-thread
    // objects. The first call happens from a module constructor, the later one
    // from the egui application constructor; both are the main thread.
    unsafe {
        let mut handler_ptr = HANDLER.load(Ordering::Relaxed) as *mut AnyObject;
        if handler_ptr.is_null() {
            let handler: Retained<OpenHandler> = msg_send![OpenHandler::class(), new];
            handler_ptr = Retained::as_ptr(&handler) as *mut AnyObject;
            // The event manager does not retain its handler, so it stays alive
            // for as long as the process does.
            std::mem::forget(handler);
            HANDLER.store(handler_ptr as usize, Ordering::Relaxed);
        }

        let Some(manager_class) = objc2::runtime::AnyClass::get(c"NSAppleEventManager") else {
            trace("install: 找不到 NSAppleEventManager");
            return;
        };
        let manager: *mut AnyObject = msg_send![manager_class, sharedAppleEventManager];
        if manager.is_null() {
            trace("install: NSAppleEventManager 未初始化");
            return;
        }

        let selector = Sel::register(c"handleOpenDocs:withReplyEvent:");
        // `odoc` is the one that carries files; `oapp` (open application) is how
        // macOS announces a plain launch, and registering for it as well is how
        // the channel is probed when something is swallowing `odoc`.
        for event_id in [OPEN_DOCUMENTS, OPEN_APPLICATION] {
            let _: () = msg_send![
                manager,
                setEventHandler: handler_ptr,
                andSelector: selector,
                forEventClass: CORE_EVENT_CLASS,
                andEventID: event_id
            ];
        }

        trace("install: 已注册 kAEOpenDocuments 处理器");
    }
}

/// Register the handler the moment the image is loaded.
///
/// This lands in `__mod_init_func`, which dyld runs for every image while it is
/// being loaded — earlier than `main`, and therefore earlier than anything
/// AppKit does. Doing it here is what makes a Finder double-click work when
/// rustmd is not already running.
///
/// Nothing in here may panic: an unwind out of an image initialiser is
/// undefined, so every step reports and returns instead.
#[used]
#[link_section = "__DATA,__mod_init_func"]
static PRELAUNCH_INSTALL: extern "C" fn() = install_at_load;

extern "C" fn install_at_load() {
    trace("prelaunch: 进程已加载，注册 Apple Event 处理器");
    install();
    hook_set_delegate();
}

// ------------------------------------------------------------ app delegate
//
// There are two ways a document reaches us, and the Apple Event manager only
// covers one of them:
//
//   * rustmd already running — the event goes through the shared manager, which
//     is why `install` handles it;
//   * rustmd being launched — AppKit handles the launch event itself and asks
//     the *application delegate* about it, through `application:openURLs:` (or
//     `application:openFiles:`, or `application:openFile:`). winit's delegate
//     implements none of them, so the file is silently dropped.
//
// Registering with the event manager earlier does not help — that was measured,
// not assumed: a handler registered from a module constructor still sees
// nothing on a cold start. The delegate is the only door. These methods add the
// three names AppKit looks for to whatever delegate is installed.

/// The original `-[NSApplication setDelegate:]`, kept so the replacement can
/// call through to it.
static ORIG_SET_DELEGATE: AtomicUsize = AtomicUsize::new(0);

/// Type encoding for `- (BOOL)application:openFile:`.
///
/// `BOOL` is a Rust `bool` on arm64 and a `signed char` on Intel, and the
/// encoding has to match what AppKit expects to read back.
#[cfg(target_arch = "aarch64")]
const OPEN_FILE_ENCODING: &std::ffi::CStr = c"B@:@:";
#[cfg(not(target_arch = "aarch64"))]
const OPEN_FILE_ENCODING: &std::ffi::CStr = c"c@:@:";

/// Type encoding for the two array-taking methods: `void` return, then
/// `NSApplication *` and `NSArray *`.
const ARRAY_METHOD_ENCODING: &std::ffi::CStr = c"v@:@:";

/// Reinterpret a concrete message implementation as the runtime's erased `Imp`.
///
/// `class_addMethod` and `method_setImplementation` only take an untyped
/// function pointer; the signature it will be called with is the one recorded
/// in the method's type encoding, so all that is left is to hand over the
/// address.
///
/// Every call site casts to a named function-pointer type first. Passing the
/// function *item* on its own would silently hand over a zero-sized value
/// instead of an address, which is why the size is checked with a plain
/// `assert_eq!` rather than a `debug_assert_eq!` — this is start-up code, and
/// getting it wrong must not depend on the build profile.
fn imp_of<F: Copy>(f: F) -> objc2::runtime::Imp {
    assert_eq!(
        std::mem::size_of::<F>(),
        std::mem::size_of::<objc2::runtime::Imp>(),
        "只能转递函数指针，不能直接传函数本身"
    );
    // SAFETY: the assertion above pins `F` to the size of a function pointer,
    // and every caller passes one.
    unsafe { std::mem::transmute_copy(&f) }
}

/// Take over `-[NSApplication setDelegate:]`.
///
/// The delegate is installed while the event loop is being built — before
/// `main` gets anywhere near `App::new` — and `setDelegate:` is the one moment
/// where the delegate object is handed to us already alive. Patching its class
/// there covers a launch that has not happened yet.
fn hook_set_delegate() {
    // SAFETY: `NSApplication` and its `setDelegate:` are public API, and the
    // method is looked up and replaced, never called by hand.
    unsafe {
        let Some(cls) = AnyClass::get(c"NSApplication") else {
            trace("hook: 找不到 NSApplication");
            return;
        };
        let sel = Sel::register(c"setDelegate:");
        let method = objc2::ffi::class_getInstanceMethod(cls as *const AnyClass, sel);
        if method.is_null() {
            trace("hook: 找不到 -[NSApplication setDelegate:]");
            return;
        }
        // Without the original there is nothing to call through to, and taking
        // the method over anyway would break the delegate being set at all.
        let Some(original) = objc2::ffi::method_getImplementation(method) else {
            trace("hook: -[NSApplication setDelegate:] 没有实现");
            return;
        };
        ORIG_SET_DELEGATE.store(original as usize, Ordering::Relaxed);
        objc2::ffi::method_setImplementation(method, imp_of(set_delegate_hook as SetDelegate));
        trace("hook: 已接管 -[NSApplication setDelegate:]");
    }
}

extern "C-unwind" fn set_delegate_hook(this: *mut AnyObject, cmd: Sel, delegate: *mut AnyObject) {
    patch_delegate(delegate);
    let original = ORIG_SET_DELEGATE.load(Ordering::Relaxed);
    if original == 0 {
        return;
    }
    // SAFETY: the address came from `method_getImplementation` on the very
    // method being replaced, so it has exactly this signature.
    let call: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) =
        unsafe { std::mem::transmute(original) };
    unsafe { call(this, cmd, delegate) };
}

/// Add the document-opening methods to a delegate object's class.
///
/// `class_addMethod` refuses to overwrite an existing method, so a delegate
/// that already has one of these keeps it — this only ever fills the holes.
fn patch_delegate(delegate: *mut AnyObject) {
    if delegate.is_null() {
        return;
    }
    let name = class_name_of(delegate);
    // SAFETY: `delegate` is a live object, so its class is a real class and
    // adding methods to it is what the runtime is for.
    unsafe {
        let cls = objc2::ffi::object_getClass(delegate as *const AnyObject) as *mut AnyClass;
        if cls.is_null() {
            return;
        }
        let file = objc2::ffi::class_addMethod(
            cls,
            Sel::register(c"application:openFile:"),
            imp_of(application_open_file as ApplicationOpenFile),
            OPEN_FILE_ENCODING.as_ptr(),
        );
        let files = objc2::ffi::class_addMethod(
            cls,
            Sel::register(c"application:openFiles:"),
            imp_of(application_open_files as ApplicationOpenFiles),
            ARRAY_METHOD_ENCODING.as_ptr(),
        );
        let urls = objc2::ffi::class_addMethod(
            cls,
            Sel::register(c"application:openURLs:"),
            imp_of(application_open_urls as ApplicationOpenUrls),
            ARRAY_METHOD_ENCODING.as_ptr(),
        );
        trace(format!(
            "delegate {name}: openFile:{} openFiles:{} openURLs:{}",
            file.as_bool(),
            files.as_bool(),
            urls.as_bool(),
        ));
    }
}

/// `- (BOOL)application:(NSApplication *)sender openFile:(NSString *)filename`
type ApplicationOpenFile =
    unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject) -> bool;
/// `- (void)application:(NSApplication *)sender openFiles:(NSArray<NSString *> *)filenames`
type ApplicationOpenFiles =
    unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject);
/// `- (void)application:(NSApplication *)sender openURLs:(NSArray<NSURL *> *)urls`
type ApplicationOpenUrls =
    unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject);
/// `- (void)setDelegate:(id<NSApplicationDelegate>)delegate`
type SetDelegate = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);

/// AppKit asks with a list of URLs on every macOS this runs on; the older
/// filename forms are answered too, in case it falls back to them.
unsafe extern "C-unwind" fn application_open_urls(
    _this: *mut AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
    urls: *mut AnyObject,
) {
    // SAFETY: AppKit passes an `NSArray<NSURL *>` for this selector.
    let array: &NSArray<NSURL> = unsafe { &*(urls as *const NSArray<NSURL>) };
    let paths: Vec<PathBuf> = array
        .iter()
        .filter_map(|url| url.path().map(|p| PathBuf::from(p.to_string())))
        .collect();
    accept(paths, "application:openURLs:");
}

unsafe extern "C-unwind" fn application_open_files(
    _this: *mut AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
    names: *mut AnyObject,
) {
    // SAFETY: the legacy selector carries an `NSArray<NSString *>`.
    let array: &NSArray<NSString> = unsafe { &*(names as *const NSArray<NSString>) };
    let paths: Vec<PathBuf> = array
        .iter()
        .map(|name| PathBuf::from(name.to_string()))
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    accept(paths, "application:openFiles:");
}

unsafe extern "C-unwind" fn application_open_file(
    _this: *mut AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
    filename: *mut AnyObject,
) -> bool {
    // SAFETY: this selector carries a single `NSString *`.
    let name: &NSString = unsafe { &*(filename as *const NSString) };
    let path = PathBuf::from(name.to_string());
    let takes_it = !path.as_os_str().is_empty();
    if takes_it {
        accept(vec![path], "application:openFile:");
    }
    // A `NO` here lets AppKit try its next option; there is no next option, but
    // claiming a file that cannot be used would be worse.
    takes_it
}

/// Park paths for the update loop, reporting what arrived.
fn accept(paths: Vec<PathBuf>, source: &str) {
    if paths.is_empty() {
        return;
    }
    trace(format!("{source} -> {paths:?}"));
    if let Ok(mut queue) = INBOX.lock() {
        queue.extend(paths);
    }
}

/// The runtime name of an object's class, for the trace log.
fn class_name_of(object: *mut AnyObject) -> String {
    // SAFETY: `object` is a live object, so it has a class with a name.
    let name = unsafe { objc2::ffi::object_getClassName(object as *const AnyObject) };
    if name.is_null() {
        return "<未知>".to_string();
    }
    unsafe { std::ffi::CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}

/// Pull the file URLs out of a `kAEOpenDocuments` event.
///
/// The event's direct object is a list of `typeFileURL` descriptors. Anything
/// unexpected is skipped rather than guessed at.
unsafe fn paths_in_event(event: &NSAppleEventDescriptor) -> Vec<PathBuf> {
    let mut out = Vec::new();
    // SAFETY: `paramDescriptorForKeyword:` is a plain getter.
    let list: Option<Retained<NSAppleEventDescriptor>> =
        msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT];
    let Some(list) = list else {
        return out;
    };

    // A single document arrives as a *one-element list*, and `descriptorAtIndex:`
    // does not answer for it: asking the list itself for its file URL is the only
    // form of the query that works. With more than one document the list has to
    // be walked.
    let count: isize = msg_send![&*list, numberOfItems];
    if count <= 1 {
        if let Some(p) = path_from_descriptor(&list) {
            out.push(p);
        }
        return out;
    }
    for i in 0..count {
        let item: Option<Retained<NSAppleEventDescriptor>> =
            msg_send![&*list, descriptorAtIndex: i];
        if let Some(item) = item {
            if let Some(p) = path_from_descriptor(&item) {
                out.push(p);
            }
        }
    }
    out
}

/// One descriptor to one path, via `fileURLValue` when it is a file URL and
/// `stringValue` otherwise.
unsafe fn path_from_descriptor(desc: &NSAppleEventDescriptor) -> Option<PathBuf> {
    let kind: u32 = msg_send![desc, descriptorType];
    trace(format!("  descriptor type = {}", fourcc(kind)));

    let url: Option<Retained<NSURL>> = msg_send![desc, fileURLValue];
    trace(format!("  fileURLValue = {url:?}"));
    if let Some(url) = url {
        let path = url.path();
        trace(format!("  url.path = {path:?}"));
        if let Some(p) = path {
            return Some(PathBuf::from(p.to_string()));
        }
    }
    let text: Option<objc2::rc::Retained<objc2_foundation::NSString>> =
        msg_send![desc, stringValue];
    trace(format!("  stringValue = {text:?}"));
    let s = text?.to_string();
    // Some senders hand over a bare path rather than a URL.
    if let Some(rest) = s.strip_prefix("file://") {
        let decoded = percent_decode(rest);
        if !decoded.is_empty() {
            return Some(PathBuf::from(decoded));
        }
    }
    if s.starts_with('/') {
        return Some(PathBuf::from(s));
    }
    None
}

/// A four-character code as readable text, for the trace log.
fn fourcc(code: u32) -> String {
    let b = code.to_be_bytes();
    if b.iter().all(|c| c.is_ascii_graphic()) {
        String::from_utf8_lossy(&b).into_owned()
    } else {
        format!("0x{code:08x}")
    }
}

/// Whether to trace what the Apple Event channel delivers.
///
/// Off by default. Enabled either by `RUSTMD_DEBUG_OPEN` or by creating
/// `~/Library/Application Support/rustmd/open-debug`; the marker file exists
/// because `open --env` does not reliably hand an environment to a bundled app,
/// and a bundle has no visible standard streams. Remove the marker to go quiet
/// again.
fn debug_open() -> bool {
    if std::env::var_os("RUSTMD_DEBUG_OPEN").is_some() {
        return true;
    }
    support_dir().map_or(false, |d| d.join("open-debug").exists())
}

/// `~/Library/Application Support/rustmd`, where the config also lives.
fn support_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/rustmd"))
}

/// Append a line to the trace log, when tracing is on.
fn trace(msg: impl AsRef<str>) {
    use std::io::Write as _;
    if !debug_open() {
        return;
    }
    let Some(dir) = support_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("open-events.log"))
    {
        let _ = writeln!(f, "{}", msg.as_ref());
    }
}

/// Undo `%XX` escaping in a file URL path.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_escapes_are_decoded() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("%E4%B8%AD%E6%96%87.md"), "中文.md");
        assert_eq!(percent_decode("plain.md"), "plain.md");
        // A dangling escape is left alone rather than dropped.
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn the_inbox_starts_empty_and_drains() {
        // Another test cannot have filled this: `take_opened` is the only
        // reader and it always drains.
        let _ = take_opened();
        assert!(take_opened().is_empty());
    }
}
