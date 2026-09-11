import AppKit
import Carbon.HIToolbox

/// The global hotkey, via Carbon's `RegisterEventHotKey`.
///
/// Carbon rather than a `CGEventTap` because the tap needs Accessibility permission,
/// and a permission dialog on first run is a far worse introduction than a launcher
/// that simply works. The API looks legacy but is not deprecated: in the macOS 26.5
/// SDK it carries `AVAILABLE_MAC_OS_X_VERSION_10_0_AND_LATER`, a macro that expands to
/// nothing at all, and compiling against it under `-swift-version 6` produces no
/// warning. It also requires no TCC grant, which is the entire point.
@MainActor
final class HotKey {
    /// Carbon virtual key code and modifier mask, from config.toml via `bs_settings`.
    ///
    /// Carbon masks are *not* `NSEvent.ModifierFlags`: Carbon uses bits 8-12 (`cmdKey` =
    /// 0x100), AppKit bits 16-23 (`.command` = 0x100000). They share no bit positions, so a raw
    /// `NSEvent` value handed to Carbon is silently the wrong chord.
    let keyCode: UInt32
    let modifiers: UInt32

    private static let identifier = EventHotKeyID(signature: 0x626C_6E64, id: 1)

    private let onPress: () -> Void
    private var hotKeyRef: EventHotKeyRef?
    private var handlerRef: EventHandlerRef?

    init(keyCode: UInt32, modifiers: UInt32, onPress: @escaping () -> Void) {
        self.keyCode = keyCode
        self.modifiers = modifiers
        self.onPress = onPress
    }

    /// `isolated` so the deinit runs on the main actor and may legitimately read the
    /// two `EventHotKeyRef`s, which are `OpaquePointer`s and therefore not `Sendable`.
    /// The alternative — marking them `nonisolated(unsafe)` — would assert the access
    /// is safe instead of arranging for it to be.
    ///
    /// Removing the handler before the object dies is what keeps the unretained `self`
    /// in `userData` from dangling.
    isolated deinit {
        if let handlerRef { RemoveEventHandler(handlerRef) }
        if let hotKeyRef { UnregisterEventHotKey(hotKeyRef) }
    }

    /// Returns false if either Carbon call failed. A false here means no hotkey, which
    /// means no launcher, so the caller is expected to surface it rather than shrug.
    @discardableResult
    func register() -> Bool {
        guard hotKeyRef == nil else { return true }

        var spec = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard),
            eventKind: UInt32(kEventHotKeyPressed)
        )

        // Unretained: this object is owned by the app delegate for the process
        // lifetime, and `deinit` removes the handler before it could dangle. Retaining
        // here would instead leak the object forever.
        let context = Unmanaged.passUnretained(self).toOpaque()

        // `GetApplicationEventTarget` rather than `GetEventDispatcherTarget`: Apple's
        // own header calls the dispatcher target something only "exotic apps that need
        // to pick events off the event queue" should touch.
        guard
            InstallEventHandler(
                GetApplicationEventTarget(), hotKeyEventHandler, 1, &spec, context,
                &handlerRef) == noErr
        else { return false }

        var ref: EventHotKeyRef?
        let status = RegisterEventHotKey(
            keyCode, modifiers, Self.identifier,
            GetApplicationEventTarget(), 0, &ref)

        guard status == noErr, let ref else {
            if let handlerRef {
                RemoveEventHandler(handlerRef)
                self.handlerRef = nil
            }
            return false
        }
        hotKeyRef = ref
        return true
    }

    fileprivate func fire() {
        onPress()
    }
}

/// The C callback. A function pointer carries no Swift isolation and no captured
/// context, so this is a file-scope `nonisolated` function that recovers the `HotKey`
/// from `userData`.
///
/// Apple documents these APIs only as "not thread safe" and never states which thread
/// delivers the callback; main-thread delivery is universal convention rather than a
/// written contract. So the thread is checked rather than asserted —
/// `MainActor.assumeIsolated` traps if the assumption is ever wrong, and trapping in
/// front of the user is worse than declining one keypress.
private nonisolated func hotKeyEventHandler(
    _ callRef: EventHandlerCallRef?,
    _ event: EventRef?,
    _ userData: UnsafeMutableRawPointer?
) -> OSStatus {
    guard let userData, Thread.isMainThread else {
        return OSStatus(eventNotHandledErr)
    }
    let hotKey = Unmanaged<HotKey>.fromOpaque(userData).takeUnretainedValue()
    return MainActor.assumeIsolated {
        hotKey.fire()
        return noErr
    }
}
