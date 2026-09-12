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
    /// Carbon virtual key code and modifier mask, from config.toml via `bs_startup`.
    ///
    /// Carbon masks are *not* `NSEvent.ModifierFlags`: Carbon uses bits 8-12 (`cmdKey` =
    /// 0x100), AppKit bits 16-23 (`.command` = 0x100000). They share no bit positions, so a raw
    /// `NSEvent` value handed to Carbon is silently the wrong chord.
    private(set) var keyCode: UInt32
    private(set) var modifiers: UInt32

    /// Tells this hotkey's presses from any other's.
    ///
    /// Load-bearing: a Carbon handler on the application target receives *every*
    /// `kEventHotKeyPressed`, not only the one it registered. With two hotkeys and no check,
    /// either chord would fire both actions.
    private let identifier: EventHotKeyID
    private static let signature: OSType = 0x626C_6E64

    private let onPress: () -> Void
    private var hotKeyRef: EventHotKeyRef?
    private var handlerRef: EventHandlerRef?

    init(keyCode: UInt32, modifiers: UInt32, id: UInt32 = 1, onPress: @escaping () -> Void) {
        self.keyCode = keyCode
        self.modifiers = modifiers
        self.identifier = EventHotKeyID(signature: Self.signature, id: id)
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

        // Once only. Installing the same handler on the same target a second time fails, which
        // is what `reinstall` used to run into: the handler outlives any one registration.
        //
        // `GetApplicationEventTarget` rather than `GetEventDispatcherTarget`: Apple's
        // own header calls the dispatcher target something only "exotic apps that need
        // to pick events off the event queue" should touch.
        if handlerRef == nil {
            guard
                InstallEventHandler(
                    GetApplicationEventTarget(), hotKeyEventHandler, 1, &spec, context,
                    &handlerRef) == noErr
            else { return false }
        }

        var ref: EventHotKeyRef?
        let status = RegisterEventHotKey(
            keyCode, modifiers, identifier,
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

    /// Registers again from scratch.
    ///
    /// For the one case that needs it: the chord was Spotlight's when blindspot started — Carbon
    /// says success for a chord another process holds, and the key then never arrives — and it
    /// has since been let go. Re-registering costs nothing and removes the question of whether
    /// the first registration would have started working on its own.
    @discardableResult
    func reinstall() -> Bool {
        if let hotKeyRef {
            UnregisterEventHotKey(hotKeyRef)
            self.hotKeyRef = nil
        }
        return register()
    }

    /// Binds a different chord, with no restart.
    ///
    /// Goes through `reinstall()`, which keeps the one installed Carbon handler and swaps
    /// only the `EventHotKeyRef`. Building a second `HotKey` instead would try to install
    /// that handler again on the same target, which fails — see the note in `register`.
    ///
    /// False means Carbon refused. True does **not** mean the chord will fire: Carbon
    /// reports success for one another application already holds, which is the whole
    /// reason `SpotlightConflict` exists.
    @discardableResult
    func rebind(keyCode: UInt32, modifiers: UInt32) -> Bool {
        guard keyCode != self.keyCode || modifiers != self.modifiers else { return true }
        self.keyCode = keyCode
        self.modifiers = modifiers
        return reinstall()
    }

    /// Fires only for this hotkey's own id — see `identifier` — and says whether it did.
    fileprivate func fire(_ pressed: EventHotKeyID) -> Bool {
        guard pressed.signature == identifier.signature, pressed.id == identifier.id else {
            return false
        }
        onPress()
        return true
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
    var pressed = EventHotKeyID()
    let read = GetEventParameter(
        event, EventParamName(kEventParamDirectObject), EventParamType(typeEventHotKeyID),
        nil, MemoryLayout<EventHotKeyID>.size, nil, &pressed)
    guard read == noErr else { return OSStatus(eventNotHandledErr) }

    let hotKey = Unmanaged<HotKey>.fromOpaque(userData).takeUnretainedValue()
    return MainActor.assumeIsolated {
        // `noErr` only when this hotkey is the one that was pressed. Carbon calls handlers
        // newest first and stops at the first that claims the event, so claiming another
        // hotkey's press would silently swallow it — measured: with two hotkeys installed,
        // the first one registered never fired.
        hotKey.fire(pressed) ? noErr : OSStatus(eventNotHandledErr)
    }
}
