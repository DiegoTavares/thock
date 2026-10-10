import LocalAuthentication
import SwiftUI
import ThockKit
import UIKit
import WidgetKit

enum Appearance: String, CaseIterable, Identifiable {
    case dark
    case light
    case system

    var id: String { rawValue }

    var title: String {
        switch self {
        case .dark: return "Dark"
        case .light: return "Light"
        case .system: return "Match iPhone"
        }
    }

    var scheme: ColorScheme? {
        switch self {
        case .dark: return .dark
        case .light: return .light
        case .system: return nil
        }
    }
}

enum AppSheet: Identifiable, Equatable {
    case capture(entry: String, preset: CaptureDestination?)
    case journal
    case clip
    case ask
    case receipts
    case you
    case calendar
    /// A prose section open for a paragraph edit (V37 §6): the card's
    /// heading line and the paragraph being edited, or `nil` for a new one.
    case section(note: NoteID, card: Int, editing: Int?)

    var id: String {
        switch self {
        case .capture(let entry, _): return "capture-\(entry)"
        case .journal: return "journal"
        case .clip: return "clip"
        case .ask: return "ask"
        case .receipts: return "receipts"
        case .you: return "you"
        case .calendar: return "calendar"
        case .section(_, let card, _): return "section-\(card)"
        }
    }
}

struct Toast: Equatable, Identifiable {
    let id = UUID()
    var text: String
    /// The button's title; `Undo` unless the toast offers something else.
    var action = "Undo"
    var undo: (() -> Void)?

    static func == (left: Toast, right: Toast) -> Bool {
        left.id == right.id
    }
}

@MainActor
@Observable
final class AppModel {
    enum Phase {
        case loading
        case welcome
        case ready
    }

    var phase: Phase = .loading
    /// Bumped whenever the store changes, so screens re-read it.
    var revision = 0
    var syncState: SyncState = .notConnected
    var diagnostics = SyncDiagnostics()
    var sheet: AppSheet?
    var toast: Toast?
    var isUnlocked = false
    var selectedDay: VaultDay = .today()
    var selectedWeek: VaultWeek = .current()
    /// Where the canvases' pages are centred. Only a jump moves them, never
    /// a swipe, so the pages hold still while one is under the finger.
    private(set) var dayPagesAnchor: VaultDay = .today()
    private(set) var weekPagesAnchor: VaultWeek = .current()
    /// The week canvas takes the day canvas's slot (V37 §5, N1).
    var showingWeek = false
    /// The backlog is a canvas over the pager, never a sheet (V38 §4).
    var showingBacklog = false
    var pairingError: String?
    var isPairing = false
    var deskAwake = true
    var appearance: Appearance {
        didSet { ThockEnvironment.defaults.set(appearance.rawValue, forKey: "appearance") }
    }

    /// Today's questions and answers (V35 §5.5).
    private(set) var askTurns: [AskTurn] = []
    /// The turn being answered, and the one line that says what the agent
    /// is doing about it.
    private(set) var askingTurn: Int64?
    private(set) var askActivity = ""
    private(set) var askRunningLow = false
    @ObservationIgnored private var askTask: Task<Void, Never>?
    @ObservationIgnored private var askGrace = UIBackgroundTaskIdentifier.invalid

    private(set) var store: VaultStore?
    private var engine: SyncEngine?
    private var world: LocalWorld?
    private let secrets = KeychainSecretStore()
    private var observers: [NSObjectProtocol] = []
    private var toastTask: Task<Void, Never>?
    private var pendingRemoval: (item: PlannerItem, note: NoteID)?
    private var pendingBacklogRemoval: BacklogTask?
    private var pendingInboxGesture: (path: String, gesture: InboxGesture)?
    private var isAuthenticating = false
    /// Set on launch and on every return from the background.
    private var needsGreeting = true
    private var entryAfterUnlock: EntryPoint?

    init() {
        appearance = ThockEnvironment.defaults.string(forKey: "appearance").flatMap(Appearance.init(rawValue:)) ?? .dark
    }

    var session: VaultSession? {
        store.map { VaultSession(store: $0) }
    }

    var isPractice: Bool {
        store?.meta("backend") == SimulatedDesk.backendName
    }

    var isReadOnly: Bool {
        _ = revision
        return store?.isReadOnly ?? false
    }

    var waitingForDesk: Int {
        _ = revision
        return store?.waitingForDeskCount ?? 0
    }

    // MARK: Launch

    func boot() async {
        guard phase == .loading else { return }
        do {
            let store = try ThockEnvironment.openStore()
            self.store = store
            observers.append(NotificationCenter.default.addObserver(forName: VaultStore.didChange, object: store, queue: .main) { [weak self] _ in
                Task { @MainActor in self?.storeChanged() }
            })
            observers.append(NotificationCenter.default.addObserver(forName: SyncEngine.stateDidChange, object: nil, queue: .main) { [weak self] _ in
                Task { @MainActor in await self?.refreshSyncState() }
            })
            observers.append(NotificationCenter.default.addObserver(forName: SyncEngine.writeRefused, object: nil, queue: .main) { [weak self] note in
                let path = note.object as? String ?? "a note"
                Task { @MainActor in self?.show("A change to \(path) couldn't be sent and was dropped.") }
            })
            observers.append(NotificationCenter.default.addObserver(forName: .thockEntryPoint, object: nil, queue: .main) { [weak self] _ in
                Task { @MainActor in self?.takePendingEntry() }
            })
            if store.isConnected {
                try await connect(store: store)
                phase = .ready
            } else {
                phase = .welcome
            }
        } catch {
            pairingError = "Thock could not open its notebook on this phone."
            phase = .welcome
        }
        becameActive()
        await applyLaunchArguments()
    }

    private func connect(store: VaultStore) async throws {
        let transport: SyncTransport
        if store.meta("backend") == SimulatedDesk.backendName {
            let world = LocalWorld(url: ThockEnvironment.practiceWorldURL)
            self.world = world
            try await world.start()
            deskAwake = await world.desk.isAwake
            transport = world.backend
        } else if let base = store.meta("backend").flatMap(URL.init(string:)) {
            transport = HTTPTransport(base: base, appVersion: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.1")
        } else {
            throw PairingError.badLink
        }
        await install(SyncEngine(store: store, transport: transport, secrets: secrets))
        await refreshSyncState()
    }

    /// The only place `engine` changes. The previous engine's feed loop is
    /// stopped first: left running, it would keep syncing over its old
    /// transport while reading the new pairing's Keychain secrets.
    private func install(_ newEngine: SyncEngine?) async {
        await engine?.stop()
        engine = newEngine
        await newEngine?.start()
    }

    private func storeChanged() {
        revision += 1
        scheduleWidgetReload()
        // A write just landed locally; send it on without waiting for the feed.
        Task { await engine?.sync() }
    }

    @ObservationIgnored private var widgetReload: Task<Void, Never>?

    /// A first pull changes hundreds of notes one by one, and each reload
    /// spends the widgets' daily budget, so a burst becomes one reload.
    private func scheduleWidgetReload() {
        widgetReload?.cancel()
        widgetReload = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            guard !Task.isCancelled else { return }
            WidgetCenter.shared.reloadAllTimelines()
        }
    }

    private func refreshSyncState() async {
        guard let engine else {
            syncState = .notConnected
            return
        }
        syncState = await engine.state
        diagnostics = await engine.diagnostics
        // Going read-only, or back, changes what every screen offers.
        revision += 1
    }

    // MARK: Connecting

    /// Pairs from the link in the desk's QR, scanned or pasted.
    func pair(text: String) async {
        guard let store, let link = PairingLink(text) else {
            pairingError = "That isn't a Thock code. Scan the one your desk is showing."
            return
        }
        isPairing = true
        pairingError = nil
        defer { isPairing = false }
        // Pairing rewrites the store and the Keychain, so the current engine
        // is set aside (no feed, no nudges from store changes) until it is
        // replaced or, if pairing fails, put back.
        let previous = engine
        await install(nil)
        do {
            guard let base = URL(string: link.backend), base.scheme != nil else { throw PairingError.badLink }
            let transport = HTTPTransport(base: base)
            let engine = SyncEngine(store: store, transport: transport, secrets: secrets)
            try await engine.pair(link: link, deviceName: UIDevice.current.name)
            await install(engine)
            await finishConnecting()
        } catch {
            await install(previous)
            pairingError = Self.sentence(for: error)
        }
    }

    /// The practice notebook: a sample vault with a simulated desk behind it,
    /// all on this phone.
    func startPractice() async {
        guard let store else { return }
        isPairing = true
        pairingError = nil
        defer { isPairing = false }
        let previous = engine
        await install(nil)
        do {
            try? FileManager.default.removeItem(at: ThockEnvironment.practiceWorldURL)
            let world = LocalWorld(url: ThockEnvironment.practiceWorldURL)
            try await world.start()
            let engine = SyncEngine(store: store, transport: world.backend, secrets: secrets)
            try await engine.pair(link: try await world.desk.pairingLink(), deviceName: UIDevice.current.name)
            for capture in SampleVault.make().captures {
                store.addCapture(capture)
            }
            self.world = world
            deskAwake = true
            await install(engine)
            await finishConnecting()
        } catch {
            await install(previous)
            pairingError = Self.sentence(for: error)
        }
    }

    private func finishConnecting() async {
        goToToday()
        isUnlocked = true
        needsGreeting = false
        phase = .ready
        await refreshSyncState()
    }

    func disconnect() async {
        askTask?.cancel()
        askTurns = []
        await engine?.disconnect()
        await install(nil)
        world?.reset()
        world = nil
        sheet = nil
        phase = .welcome
        await refreshSyncState()
    }

    static func sentence(for error: Error) -> String {
        switch error {
        case PairingError.keyMismatch: return "That code didn't work. Try again."
        case PairingError.badLink: return "That isn't a Thock code. Scan the one your desk is showing."
        case PairingError.refused(let sentence): return sentence
        case let api as APIError: return api.error
        default: return "Thock couldn't reach your desk's copy. Check your connection and try again."
        }
    }

    // MARK: Trust

    /// Reading anything from the vault needs the phone's owner; capture does
    /// not (V33 §4.6). A phone with no passcode has nothing to ask, and the
    /// practice notebook holds nothing of the person's to protect.
    func unlock() async {
        guard !isUnlocked, !isAuthenticating else { return }
        isAuthenticating = true
        isUnlocked = await ownerIsPresent()
        isAuthenticating = false
        if isUnlocked, let entry = entryAfterUnlock {
            entryAfterUnlock = nil
            open(entry)
        }
    }

    private func ownerIsPresent() async -> Bool {
        if isPractice, !ProcessInfo.processInfo.arguments.contains("-thock-gate") {
            return true
        }
        let context = LAContext()
        var failure: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &failure) else { return true }
        return (try? await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: "Show your notes")) ?? false
    }

    func becameActive() {
        guard phase == .ready else { return }
        revision += 1
        Task { await engine?.sync() }
        guard needsGreeting else { return }
        needsGreeting = false
        // Every launch is today (V33 §5, H1); so is every return.
        goToToday()
        let entry = EntryPoint.take()
        if let entry, entry != .journal {
            // Writing needs no unlock: the sheet rises over the locked canvas.
            open(entry)
            if isPractice { Task { await unlock() } }
            return
        }
        // Someone in the middle of writing is not interrupted to prove who
        // they are; the canvas behind the sheet stays locked until they ask.
        switch sheet {
        case .capture?, .clip?: return
        default: break
        }
        entryAfterUnlock = entry
        Task { await unlock() }
    }

    func wentToBackground() {
        commitPendingRemoval()
        isUnlocked = false
        needsGreeting = true
    }

    // MARK: Entry points

    func open(_ entry: EntryPoint) {
        guard phase == .ready else { return }
        if entry == .journal || entry == .ask, !isUnlocked {
            // Both show what the vault holds, so they wait.
            entryAfterUnlock = entry
            Task { await unlock() }
            return
        }
        switch entry {
        case .idea: sheet = .capture(entry: "idea", preset: nil)
        case .journal: sheet = .journal
        case .clip: sheet = .clip
        case .ask: sheet = .ask
        }
    }

    // MARK: The calendar

    /// Opens the calendar over the canvas. The dots say which days were
    /// written on, so it waits for the unlock like every other read.
    func openCalendar() {
        guard phase == .ready, isUnlocked else { return }
        sheet = .calendar
    }

    func go(to day: VaultDay) {
        dayPagesAnchor = day
        selectedDay = day
        showingWeek = false
        sheet = nil
    }

    func go(to week: VaultWeek) {
        weekPagesAnchor = week
        selectedWeek = week
        showingWeek = true
        sheet = nil
    }

    func goToToday() {
        dayPagesAnchor = .today()
        weekPagesAnchor = .current()
        selectedDay = .today()
        selectedWeek = .current()
        showingWeek = false
        showingBacklog = false
    }

    // MARK: The backlog

    /// Opens the backlog over the pager. It reads the vault, so it waits
    /// for the unlock like every other read.
    func openBacklog() {
        guard phase == .ready, isUnlocked else { return }
        sheet = nil
        withAnimation(.snappy) { showingBacklog = true }
    }

    func closeBacklog() {
        commitPendingRemoval()
        withAnimation(.snappy) { showingBacklog = false }
    }

    func takePendingEntry() {
        if let entry = EntryPoint.take() {
            open(entry)
        }
    }

    func handle(url: URL) {
        guard url.scheme == "thock" else { return }
        if url.host == "pair" {
            Task { await pair(text: url.absoluteString) }
            return
        }
        if url.host == "backlog" {
            openBacklog()
            return
        }
        if let host = url.host, let entry = EntryPoint(rawValue: host == "capture" ? "idea" : host) {
            open(entry)
        }
    }

    func handle(shortcut type: String) {
        if let entry = EntryPoint(rawValue: String(type.split(separator: ".").last ?? "")) {
            EntryPoint.leave(entry)
            takePendingEntry()
        }
    }

    /// Launch arguments for looking at a screen without tapping to it:
    /// `-thock-practice` opens the practice notebook from a fresh install,
    /// `-thock-open <journal|idea|clip|ask|receipts|you|dock|calendar|backlog>` opens a sheet or the backlog,
    /// `-thock-day <offset>` shows another day, `-thock-week <offset>` a week,
    /// `-thock-looks <dark|light|system>` sets the appearance.
    private func applyLaunchArguments() async {
        let arguments = ProcessInfo.processInfo.arguments
        func value(after flag: String) -> String? {
            guard let index = arguments.firstIndex(of: flag), index + 1 < arguments.count else { return nil }
            return arguments[index + 1]
        }
        if arguments.contains("-thock-practice"), phase == .welcome {
            await startPractice()
        }
        if let looks = value(after: "-thock-looks").flatMap(Appearance.init(rawValue:)) {
            appearance = looks
        }
        if let offset = value(after: "-thock-day").flatMap(Int.init) {
            go(to: VaultDay.today().adding(days: offset))
        }
        if let offset = value(after: "-thock-week").flatMap(Int.init) {
            go(to: VaultWeek.current().adding(weeks: offset))
        }
        #if DEBUG
        if let script = value(after: "-thock-script") {
            await run(script: script)
        }
        #endif
        switch value(after: "-thock-open") {
        case "receipts"?: sheet = .receipts
        case "you"?: sheet = .you
        case "calendar"?: sheet = .calendar
        case "backlog"?: openBacklog()
        case "dock"?: sheet = .capture(entry: "dock", preset: nil)
        case let other?:
            if let entry = EntryPoint(rawValue: other) { open(entry) }
        case nil: break
        }
    }

    #if DEBUG
    /// `-thock-script "capture:today:Call the notary;tick:Read 20;inbox-today:Ask Ana;asleep;triage"`
    /// does what taps would, in order, for checking the app end to end
    /// without driving the screen.
    private func run(script: String) async {
        for step in script.split(separator: ";") {
            let parts = step.split(separator: ":", maxSplits: 2).map(String.init)
            let today = VaultDay.today()
            switch parts.first {
            case "capture" where parts.count == 3:
                saveCapture(blocks: Blocks.parse(parts[2].replacingOccurrences(of: "\\n", with: "\n")), destination: CaptureDestination(rawValue: parts[1]) ?? .inbox, entry: "script")
            case "capture-photo" where parts.count == 3:
                // `capture-photo:<inbox|today|backlog>:<text>`: a capture with
                // one drawn picture attached (V39 §7.1).
                let image = UIGraphicsImageRenderer(size: CGSize(width: 64, height: 48)).image { context in
                    UIColor.orange.setFill()
                    context.fill(CGRect(x: 0, y: 0, width: 64, height: 48))
                }
                if let data = image.pngData(), let attachment = ImageDownsizer.prepare(data, name: "smoke") {
                    saveCapture(blocks: Blocks.parse(parts[2].replacingOccurrences(of: "\\n", with: "\n")), destination: CaptureDestination(rawValue: parts[1]) ?? .inbox, images: [attachment], entry: "script")
                }
            case "journal" where parts.count >= 2:
                perform { try $0.journalAppend(blocks: Blocks.parse(parts[1])) }
            case "tick" where parts.count >= 2:
                if let item = session?.view(today)?.planner.items.first(where: { $0.label.hasPrefix(parts[1]) }) {
                    tick(item, note: .day(today))
                }
            case "soon" where parts.count >= 2:
                if let item = session?.view(today)?.planner.items.first(where: { $0.label.hasPrefix(parts[1]) }) {
                    perform { try $0.moveToSoon(item, note: .day(today)) }
                }
            case "backlog-move" where parts.count == 3:
                // `backlog-move:<label prefix>:<soon|someday|today|top>`: the
                // menu's section items, Move to today, or a drag to the top
                // of the task's own group.
                if let backlog = session?.backlog(), let task = backlog.openSections.flatMap(\.tasks).first(where: { $0.label.hasPrefix(parts[1]) }), let group = backlog.group(of: task) {
                    switch parts[2] {
                    case "soon": backlogMove(task, from: group, toSection: backlog.soon)
                    case "someday": backlogMove(task, from: group, toSection: backlog.someday)
                    case "today": backlogMoveToToday(task)
                    case "top": backlogMove(task, to: group, place: .top)
                    default: break
                    }
                }
            case "backlog-tick" where parts.count >= 2:
                if let task = session?.backlog().openSections.flatMap(\.tasks).first(where: { $0.label.hasPrefix(parts[1]) }) {
                    backlogTick(task)
                }
            case "backlog-remove" where parts.count >= 2:
                if let task = session?.backlog().openSections.flatMap(\.tasks).first(where: { $0.label.hasPrefix(parts[1]) }) {
                    backlogRemove(task)
                    commitPendingRemoval()
                }
            case "inbox-today", "inbox-backlog", "inbox-archive":
                // `inbox-<today|backlog|archive>:<title prefix>`: the inbox
                // screen's swipes (V40 §4), written at once.
                if parts.count >= 2, let gesture = InboxGesture(rawValue: String(parts[0].dropFirst("inbox-".count))),
                   let note = store?.waitingInboxNotes().first(where: { $0.title.hasPrefix(parts[1]) }) {
                    inboxGesture(gesture, path: note.path)
                    commitPendingRemoval()
                }
            case "asked" where parts.count == 3:
                // A finished turn in the day's thread, as if the agent had
                // read today's note: `asked:<question>:<answer>`.
                if var turn = try? store?.addAskTurn(question: parts[1], day: today) {
                    turn.answer = parts[2].replacingOccurrences(of: "\\n", with: "\n")
                    turn.sources = session.map { [$0.config.dailyPath(today)] } ?? []
                    try? store?.finishAskTurn(turn)
                }
            case "asleep": await setDeskAwake(false)
            case "awake": await setDeskAwake(true)
            case "triage": await triageAtTheDesk()
            case "plan": await planAtTheDesk()
            case "lapse": await setPracticeLapsed(true)
            case "renew": await setPracticeLapsed(false)
            default: break
            }
            await engine?.sync()
            try? await Task.sleep(nanoseconds: 300_000_000)
        }
    }
    #endif

    // MARK: Doing things

    func show(_ text: String, action: String = "Undo", undo: (() -> Void)? = nil) {
        toastTask?.cancel()
        let toast = Toast(text: text, action: action, undo: undo)
        withAnimation(.snappy) { self.toast = toast }
        toastTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: undo == nil ? 2_200_000_000 : 4_000_000_000)
            guard !Task.isCancelled else { return }
            await MainActor.run {
                guard let self, self.toast == toast else { return }
                self.commitPendingRemoval()
                withAnimation(.snappy) { self.toast = nil }
            }
        }
    }

    /// Runs a vault action, surfacing a failure instead of losing it.
    @discardableResult
    func perform(_ action: (VaultSession) throws -> Void) -> Bool {
        guard let session else { return false }
        do {
            try action(session)
            return true
        } catch VaultSessionError.readOnly {
            show("This phone is read-only until Thock Plus is renewed.")
        } catch {
            show("That didn't save. Try again.")
        }
        return false
    }

    // MARK: Ask

    func loadAskThread() {
        askTurns = store?.askTurns(day: .today()) ?? []
    }

    /// Sends a question to the agent. The turn belongs to the app, not the
    /// sheet, so closing the sheet or leaving the app does not lose it.
    func ask(_ text: String) {
        let question = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !question.isEmpty, askTask == nil, let store, let session, let engine else { return }
        loadAskThread()
        let earlier = askTurns
        guard var turn = try? store.addAskTurn(question: question, day: .today()) else {
            show("That didn't send. Try again.")
            return
        }
        askTurns.append(turn)
        askingTurn = turn.id
        askActivity = "Thinking"
        let agent = AskAgent(session: session, grant: { try await engine.agentGrant() })
        // iOS gives a task a short while after the app leaves the screen;
        // when that runs out the turn is stopped rather than left hanging.
        askGrace = UIApplication.shared.beginBackgroundTask { [weak self] in
            self?.askTask?.cancel()
            // iOS ends an app whose expiry handler returns with the task
            // still open, and the turn's own cleanup cannot run before then.
            self?.endAskGrace()
        }
        askTask = Task { [weak self] in
            do {
                let answer = try await agent.answer(question: question, earlier: earlier) { line in
                    Task { @MainActor in
                        guard self?.askingTurn == turn.id else { return }
                        self?.askActivity = line
                    }
                }
                turn.answer = answer.text
                turn.sources = answer.sources
                self?.askRunningLow = answer.runningLow
            } catch let failure as AskFailure {
                turn.failure = failure.sentence
            } catch {
                turn.failure = "Stopped before an answer came."
            }
            do {
                try store.finishAskTurn(turn)
            } catch {
                self?.show("That answer couldn't be saved on this phone.")
            }
            self?.askTask = nil
            self?.askingTurn = nil
            self?.loadAskThread()
            if let index = self?.askTurns.firstIndex(where: { $0.id == turn.id }) {
                // Shown even if saving failed, for as long as the app is open.
                self?.askTurns[index] = turn
            }
            self?.endAskGrace()
        }
    }

    private func endAskGrace() {
        guard askGrace != .invalid else { return }
        UIApplication.shared.endBackgroundTask(askGrace)
        askGrace = .invalid
    }

    func stopAsking() {
        askTask?.cancel()
    }

    func askAgain(_ turn: AskTurn) {
        guard askTask == nil, let store else { return }
        do {
            try store.removeAskTurn(id: turn.id)
        } catch {
            show("That didn't send. Try again.")
            return
        }
        ask(turn.question)
    }

    /// Appends an answer to today's note under the agent's own heading.
    func keep(_ turn: AskTurn) {
        guard let answer = turn.answer, !turn.kept, let store else { return }
        var kept = false
        guard perform({ kept = try $0.keep(question: turn.question, answer: answer) }), kept else { return }
        var turn = turn
        turn.kept = true
        do {
            try store.finishAskTurn(turn)
        } catch {
            // The note has the answer; only the button's memory of it is lost.
        }
        loadAskThread()
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        show("Kept in today's note")
    }

    func chip(for entry: String) -> CaptureDestination {
        ThockEnvironment.defaults.string(forKey: "chip.\(entry)").flatMap(CaptureDestination.init(rawValue:)) ?? .today
    }

    func remember(chip: CaptureDestination, for entry: String) {
        ThockEnvironment.defaults.set(chip.rawValue, forKey: "chip.\(entry)")
    }

    func saveCapture(blocks: [Block], destination: CaptureDestination, images: [ImageAttachment] = [], entry: String) {
        var saved: CaptureRecord?
        let ok = perform { saved = try $0.capture(blocks: blocks, destination: destination, images: images) }
        guard ok, saved != nil else { return }
        remember(chip: destination, for: entry)
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        switch destination {
        case .inbox: show("Saved to your inbox")
        case .today: show("Added to today")
        case .backlog: show("Added to Backlog · Soon")
        }
    }

    func tick(_ item: PlannerItem, note: NoteID) {
        if perform({ try $0.tick(item, note: note) }) {
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
        }
    }

    /// Removal is the one destructive move, so it waits a few seconds for an
    /// undo before it is written.
    func remove(_ item: PlannerItem, note: NoteID) {
        commitPendingRemoval()
        pendingRemoval = (item, note)
        revision += 1
        show("Line removed") { [weak self] in
            self?.pendingRemoval = nil
            self?.revision += 1
            withAnimation(.snappy) { self?.toast = nil }
        }
    }

    func isBeingRemoved(_ item: PlannerItem, note: NoteID) -> Bool {
        pendingRemoval?.item == item && pendingRemoval?.note == note
    }

    func commitPendingRemoval() {
        if let removal = pendingRemoval {
            pendingRemoval = nil
            perform { try $0.remove(removal.item, note: removal.note) }
        }
        if let task = pendingBacklogRemoval {
            pendingBacklogRemoval = nil
            perform { try $0.removeBacklog(task) }
        }
        if let pending = pendingInboxGesture {
            pendingInboxGesture = nil
            perform { try $0.triageInbox(path: pending.path, gesture: pending.gesture) }
        }
    }

    // MARK: The inbox's gestures (V40 §4)

    /// A swipe on a waiting inbox item. The row leaves at once and the writes
    /// wait a few seconds for an undo, like a removal: a gesture that was
    /// undone must leave no trace on the wire.
    func inboxGesture(_ gesture: InboxGesture, path: String) {
        commitPendingRemoval()
        pendingInboxGesture = (path, gesture)
        revision += 1
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        let text: String
        switch gesture {
        case .today: text = "Added to today"
        case .backlog: text = "Added to the backlog"
        case .archive: text = "Archived"
        }
        show(text) { [weak self] in
            self?.pendingInboxGesture = nil
            self?.revision += 1
            withAnimation(.snappy) { self?.toast = nil }
        }
    }

    func isBeingTriaged(_ path: String) -> Bool {
        pendingInboxGesture?.path == path
    }

    // MARK: The backlog's moves (V38 §6)

    /// Where a task sits now, so a move can be undone: back into its group,
    /// after the task above it or at the top.
    private func placeBefore(_ task: BacklogTask) -> (group: BacklogGroup, place: BacklogPlace)? {
        guard let backlog = session?.backlog(), let group = backlog.group(of: task) else { return nil }
        let open = group.openTasks
        guard let index = open.firstIndex(of: task) else { return nil }
        return (group, index == 0 ? .top : .after(open[index - 1]))
    }

    func backlogMove(_ task: BacklogTask, to group: BacklogGroup, place: BacklogPlace) {
        let before = placeBefore(task)
        guard perform({ try $0.moveBacklog(task, to: group, place: place) }) else { return }
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        let destination = group.name ?? session?.backlog().section(group.heading) ?? "Backlog"
        show("Moved to \(destination)") { [weak self] in
            self?.undoMove(of: task, to: before)
        }
    }

    func backlogMove(_ task: BacklogTask, from group: BacklogGroup, toSection section: BacklogSection) {
        let before = placeBefore(task)
        guard perform({ try $0.moveBacklog(task, from: group, toSection: section) }) else { return }
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        show("Moved to \(section.title)") { [weak self] in
            self?.undoMove(of: task, to: before)
        }
    }

    /// The moved task is found again by its words in its new place; its
    /// line and ordinal there are not the ones it left with.
    private func undoMove(of task: BacklogTask, to before: (group: BacklogGroup, place: BacklogPlace)?) {
        guard let before, let backlog = session?.backlog(),
              let now = backlog.openSections.flatMap(\.tasks).first(where: { $0.hash == task.hash })
        else { return }
        perform { try $0.moveBacklog(now, to: before.group, place: before.place) }
        withAnimation(.snappy) { toast = nil }
    }

    func backlogTick(_ task: BacklogTask) {
        if perform({ try $0.tickBacklog(task) }) {
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
            show("Done, and in today's note")
        }
    }

    func backlogMoveToToday(_ task: BacklogTask) {
        if perform({ try $0.moveBacklogToToday(task) }) {
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
            show("Moved to today")
        }
    }

    func backlogEdit(_ task: BacklogTask, text: String) {
        perform { try $0.editBacklog(task, text: text) }
    }

    func backlogAdd(_ text: String, to group: BacklogGroup) {
        perform { try $0.addBacklog(text, to: group) }
    }

    /// Removal waits a few seconds for an undo before it is written, as a
    /// planner line's does.
    func backlogRemove(_ task: BacklogTask) {
        commitPendingRemoval()
        pendingBacklogRemoval = task
        revision += 1
        show("Task removed") { [weak self] in
            self?.pendingBacklogRemoval = nil
            self?.revision += 1
            withAnimation(.snappy) { self?.toast = nil }
        }
    }

    func isBeingRemoved(_ task: BacklogTask) -> Bool {
        pendingBacklogRemoval == task
    }

    func checkAgain() async {
        await engine?.sync()
        await refreshSyncState()
    }

    /// `Thock for iPhone 1.0 (10)`, as the settings sheet signs itself.
    static var versionLabel: String {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "Thock for iPhone \(version) (\(build))"
    }

    /// The facts behind *Report a problem*, for the email the person sends.
    func issueReport() -> IssueReport {
        let info = Bundle.main.infoDictionary
        let device = UIDevice.current
        var system = utsname()
        uname(&system)
        let hardware = withUnsafeBytes(of: &system.machine) { bytes in
            String(decoding: bytes.prefix { $0 != 0 }, as: UTF8.self)
        }
        return IssueReport(
            appVersion: info?["CFBundleShortVersionString"] as? String ?? "?",
            build: info?["CFBundleVersion"] as? String ?? "?",
            system: "\(device.systemName) \(device.systemVersion) · \(hardware)",
            phoneName: store?.meta("device_name"),
            address: store?.meta("backend"),
            isPractice: isPractice,
            isConnected: store?.isConnected ?? false,
            state: syncState,
            waitingForDesk: waitingForDesk,
            notesHere: store?.paths().count ?? 0,
            diagnostics: diagnostics
        )
    }

    // MARK: The practice desk

    func setDeskAwake(_ awake: Bool) async {
        guard let world else { return }
        do {
            try await world.desk.setAwake(awake)
            deskAwake = awake
            await engine?.sync()
        } catch {
            show("The practice desk didn't respond.")
        }
    }

    func triageAtTheDesk() async {
        guard let world else { return }
        do {
            let filed = try await world.desk.triageInbox()
            await engine?.sync()
            show(filed == 0 ? "The inbox was already empty" : "The desk filed \(filed) \(filed == 1 ? "capture" : "captures")")
        } catch {
            show("The practice desk didn't respond.")
        }
    }

    func planAtTheDesk() async {
        guard let world, let store else { return }
        let path = store.config.dailyPath(.today())
        do {
            try await world.desk.edit { disk in
                guard let note = disk[path] else { return }
                var write = WriteDocument(clientID: UUID().uuidString.lowercased(), kind: .append, path: path, madeAt: "", deviceID: "desk")
                write.heading = HeadingRef(text: "Day planner")
                write.lines = ["- [ ] 16:00 - 16:30 Review the week's goals"]
                write.placement = .beforeChildren
                disk[path] = SyncCore.apply(existing: note, write: write).text
            }
            await engine?.sync()
            show("The desk added a line to today")
        } catch {
            show("The practice desk didn't respond.")
        }
    }

    func setPracticeLapsed(_ lapsed: Bool) async {
        await world?.backend.setLapsed(lapsed)
        await engine?.sync()
        revision += 1
    }
}
