import Foundation
import Observation
import SwiftUI
import ThockKit
import WatchConnectivity
import WatchKit
import WidgetKit

/// What the watch shows and what it has asked the phone: the last snapshot,
/// the outbox and the capture being confirmed (V48 §5, §8, §9).
@MainActor
@Observable
final class WatchModel {
    static let shared = WatchModel()

    private(set) var snapshot: WatchSnapshot?
    private(set) var outbox = WatchOutbox()
    /// The day on screen, by the watch's own clock.
    var day: VaultDay = .today()
    /// Days the phone read on request, outside the snapshot's range.
    private(set) var askedDays: [String: WatchDay] = [:]
    private(set) var askingDays: Set<String> = []
    @ObservationIgnored private var asks: [UUID: String] = [:]
    /// One sentence for the person: a refused tick, a write that failed.
    var notice: String?
    /// The phone's app cannot read this one's messages, or the reverse.
    private(set) var needsUpdate = false
    /// The capture sheet is up: listening, confirming, or saved.
    var showingCapture = false
    /// Set once Save is tapped: what the confirmation reads.
    private(set) var saved: Saved?
    var showingOutbox = false

    struct Saved: Equatable {
        var id: UUID
        var toast: String
    }

    @ObservationIgnored private let files = WatchFiles(folder: WatchEnvironment.container)
    @ObservationIgnored let connection = WatchConnection()

    init() {
        snapshot = files.snapshot()
        do {
            outbox = try files.outbox()
        } catch {
            NSLog("Thock could not read what was waiting for the phone: \(error)")
        }
    }

    func start() {
        connection.activate()
    }

    // MARK: What the phone sent

    func receive(_ message: WatchMessage) {
        switch message {
        case .snapshot(let snapshot):
            take(snapshot)
        case .ack(let ack):
            settle(ack)
        case .dayReply(let reply):
            asks[reply.id] = nil
            askingDays.remove(reply.day.day)
            askedDays[reply.day.day] = reply.day
        case .capture, .tick, .dayAsk:
            break
        }
    }

    /// A message from a phone app that speaks another version of the link.
    func receivedUnreadable() {
        needsUpdate = true
    }

    private func take(_ snapshot: WatchSnapshot) {
        needsUpdate = false
        if let current = self.snapshot, current.madeAt > snapshot.madeAt { return }
        self.snapshot = snapshot
        do {
            try files.save(snapshot)
        } catch {
            NSLog("Thock could not keep the phone's reading on the watch: \(error)")
        }
        WidgetCenter.shared.reloadTimelines(ofKind: WatchEnvironment.todayKind)
        outbox.forgetSaved(before: snapshot.madeAt)
        persist()
    }

    private func settle(_ ack: WatchAck) {
        if let day = asks.removeValue(forKey: ack.id) {
            // A day the phone could not read for the watch.
            askingDays.remove(day)
            notice = ack.message
            return
        }
        guard let item = outbox.apply(ack) else { return }
        if ack.outcome == .refused, case .tick = item.request {
            notice = ack.message ?? VaultSession.failedMessage
        }
        persist()
    }

    /// The phone answered a request with nothing it could read.
    func unreadableAnswer(to id: UUID) {
        needsUpdate = true
        outbox.needsUpdate(id)
        persist()
    }

    // MARK: The outbox

    private func persist() {
        do {
            try files.save(outbox)
        } catch {
            notice = "That didn't save on the watch. Try again."
            NSLog("Thock could not keep what is waiting for the phone: \(error)")
        }
    }

    private func enqueue(_ request: WatchOutbox.Request) {
        outbox.add(request)
        persist()
        connection.send(request.message)
    }

    /// Marks a request as handed to `transferUserInfo`, so it never goes twice.
    func markTransferred(_ id: UUID) {
        guard let position = outbox.items.firstIndex(where: { $0.id == id }) else { return }
        outbox.items[position].transferred = true
        persist()
    }

    func state(of id: UUID) -> WatchOutbox.State? {
        outbox.items.first { $0.id == id }?.state
    }

    func reconcileTransfers(outstanding: Set<UUID>) {
        guard outbox.reconcile(outstanding: outstanding) else { return }
        persist()
    }

    func isWaiting(_ id: UUID) -> Bool {
        outbox.items.contains { $0.id == id && $0.state == .waiting }
    }

    func isTransferred(_ id: UUID) -> Bool {
        outbox.items.first { $0.id == id }?.transferred ?? false
    }

    /// Sends again what no transport has taken yet: after a restart, or when
    /// the phone comes back in reach.
    func resendWaiting() {
        for item in outbox.items where item.state == .waiting && !item.transferred {
            connection.send(item.request.message)
        }
    }

    func retry(_ id: UUID) {
        guard let item = outbox.retry(id) else { return }
        persist()
        connection.send(item.request.message)
    }

    func delete(_ id: UUID) {
        outbox.delete(id)
        persist()
    }

    // MARK: Days

    var today: VaultDay { .today() }

    /// What the day screen draws for `day`: the snapshot's reading, or the
    /// phone's answer for a day outside it.
    func reading(_ day: VaultDay) -> WatchDay? {
        snapshot?.day(day) ?? askedDays[day.iso]
    }

    func isOutOfRange(_ day: VaultDay) -> Bool {
        guard let snapshot, snapshot.state != .notConnected else { return false }
        return !snapshot.covers(day)
    }

    /// Asks the phone for a day the snapshot does not hold. Out of reach,
    /// the screen says so instead.
    func ask(_ day: VaultDay) {
        guard isOutOfRange(day), reading(day) == nil, !askingDays.contains(day.iso), connection.isReachable else { return }
        let ask = WatchDayAsk(day: day.iso)
        askingDays.insert(day.iso)
        asks[ask.id] = day.iso
        connection.ask(ask)
    }

    func asking(_ day: VaultDay) -> Bool {
        askingDays.contains(day.iso)
    }

    /// The phone is back in reach: what waited goes, and a day on screen
    /// that only the phone can read is asked for.
    func phoneInReach() {
        resendWaiting()
        ask(day)
    }

    func dayAskFailed(_ day: String) {
        askingDays.remove(day)
    }

    func isDone(_ row: WatchRow, on day: VaultDay) -> Bool {
        row.done || outbox.isTicked(day: day.iso, hash: row.hash, ordinal: row.ordinal)
    }

    func tick(_ row: WatchRow, on day: VaultDay) {
        guard row.kind != .calendar, !isDone(row, on: day) else { return }
        if let blocked = writeBlocked {
            notice = blocked
            return
        }
        WKInterfaceDevice.current().play(.click)
        enqueue(.tick(WatchTick(day: day.iso, hash: row.hash, ordinal: row.ordinal)))
    }

    /// Older than ten minutes: the day screen says when it was read.
    var staleSince: Date? {
        guard let snapshot, Date().timeIntervalSince(snapshot.madeAt) > 600 else { return nil }
        return snapshot.madeAt
    }

    /// Why nothing can be written from here, in words, or `nil`.
    var writeBlocked: String? {
        if needsUpdate { return WatchOutbox.updateMessage }
        switch snapshot?.state {
        case nil, .notConnected?: return "Open Thock on your iPhone to connect"
        case .readOnly?: return VaultSession.readOnlyMessage
        case .ready?: return nil
        }
    }

    // MARK: Capture (§8)

    var settings: ReminderSettings {
        snapshot?.reminders.settings ?? ReminderSettings()
    }

    /// The chip a new capture starts on: the watch's last, else the phone's
    /// memory of it.
    var rememberedChip: CaptureDestination {
        outbox.chip ?? snapshot?.chip ?? .today
    }

    var draft: WatchOutbox.Draft? {
        outbox.draft
    }

    /// Opens capture. With `voice`, dictation starts at once; a cancelled
    /// dictation writes nothing and leaves where the app was.
    func startCapture(voice: Bool) {
        saved = nil
        if writeBlocked != nil || outbox.draft != nil {
            showingCapture = true
            return
        }
        guard voice else {
            showingCapture = true
            return
        }
        dictate { [weak self] text in
            guard let self, let text else { return }
            self.startDraft(text)
            self.showingCapture = true
        }
    }

    /// Words came back from dictation: they are the draft from now on.
    func startDraft(_ text: String) {
        outbox.draft = WatchOutbox.Draft(text: text, chip: rememberedChip)
        persist()
    }

    /// The system's dictation, straight to the mic. Scribble and the
    /// keyboard are in the same sheet. `nil` when the person cancelled or
    /// said nothing.
    func dictate(_ completion: @escaping (String?) -> Void) {
        let application = WKApplication.shared()
        guard let controller = application.visibleInterfaceController ?? application.rootInterfaceController else {
            completion(nil)
            return
        }
        controller.presentTextInputController(withSuggestions: nil, allowedInputMode: .plain) { results in
            let text = (results?.first as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
            Task { @MainActor in completion(text?.isEmpty == false ? text : nil) }
        }
    }

    func redictate() {
        dictate { [weak self] text in
            guard let self, let text else { return }
            self.updateDraft { $0.text = text }
        }
    }

    func updateDraft(_ change: (inout WatchOutbox.Draft) -> Void) {
        guard var draft = outbox.draft else { return }
        change(&draft)
        outbox.draft = draft
        persist()
    }

    func choose(_ chip: CaptureDestination) {
        updateDraft { draft in
            if chip == .reminder {
                if draft.chip != .reminder { draft.before = draft.chip }
            } else {
                draft.due = nil
            }
            draft.chip = chip
        }
    }

    func pick(_ due: ReminderDue) {
        updateDraft { draft in
            draft.chip = .reminder
            draft.due = due.description
        }
    }

    /// *Remind me* closed: without a time, the chip goes back.
    func closedRemindMe() {
        updateDraft { draft in
            guard draft.chip == .reminder, draft.due == nil else { return }
            draft.chip = draft.before
        }
    }

    func cancelCapture() {
        outbox.draft = nil
        persist()
        showingCapture = false
    }

    func save() {
        guard let draft = outbox.draft, writeBlocked == nil else { return }
        let due = draft.chip == .reminder ? draft.reminderDue : nil
        guard draft.chip != .reminder || due != nil else { return }
        let capture = WatchCapture(at: Date(), text: draft.text, destination: draft.chip, due: due)
        outbox.draft = nil
        if draft.chip.isRemembered {
            outbox.chip = draft.chip
        }
        saved = Saved(id: capture.id, toast: toast(for: draft.chip, due: due))
        WKInterfaceDevice.current().play(.success)
        enqueue(.capture(capture))
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            guard self.saved?.id == capture.id else { return }
            self.showingCapture = false
            self.saved = nil
        }
    }

    /// The phone's toast, word for word.
    func toast(for chip: CaptureDestination, due: ReminderDue?) -> String {
        chip.savedSentence(due: due, today: today, now: .now(), dateOnlyAt: settings.dateOnlyAt)
    }

    /// The phone's hint under the chips (V48 §8.2). A watch capture is
    /// always one paragraph.
    func hint(for draft: WatchOutbox.Draft) -> String {
        draft.chip.hint(due: draft.reminderDue, settings: settings, today: today, now: .now(), oneParagraph: true)
    }

    // MARK: Opening

    func handle(_ url: URL) {
        guard url.scheme == "thock" else { return }
        switch url.host() {
        case "capture":
            let voice = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.contains { $0.name == "voice" && $0.value == "1" } ?? false
            startCapture(voice: voice)
        case "day":
            day = today
            showingOutbox = false
        default:
            break
        }
    }

    /// Back from the wrist going down: a draft comes back on its screen, and
    /// the Capture control's request is taken.
    func becameActive() {
        if WatchEnvironment.defaults.bool(forKey: WatchEnvironment.pendingCaptureKey) {
            WatchEnvironment.defaults.removeObject(forKey: WatchEnvironment.pendingCaptureKey)
            startCapture(voice: true)
            return
        }
        if outbox.draft != nil, !showingCapture {
            showingCapture = true
        }
        resendWaiting()
    }
}

/// The watch's end of the link (V48 §6.2): requests by `sendMessage` while
/// the phone is in reach, else by `transferUserInfo`, which the system
/// queues across restarts of either app.
@MainActor
final class WatchConnection: NSObject {
    private var session: WCSession? {
        WCSession.isSupported() ? WCSession.default : nil
    }

    /// Background refreshes the system woke the app for, finished once
    /// nothing more is pending.
    private var refreshTasks: [WKWatchConnectivityRefreshBackgroundTask] = []

    private var model: WatchModel { WatchModel.shared }

    var isReachable: Bool {
        session?.activationState == .activated && session?.isReachable == true
    }

    func activate() {
        guard let session else { return }
        session.delegate = self
        session.activate()
    }

    func send(_ message: WatchMessage) {
        guard let session, let id = message.requestID else { return }
        let dictionary: [String: Any]
        do {
            dictionary = try message.dictionary()
        } catch {
            NSLog("Thock could not write a request for the phone: \(error)")
            return
        }
        guard session.activationState == .activated else { return }
        guard session.isReachable else {
            transfer(dictionary, id: id)
            return
        }
        let reply = Reply(dictionary: dictionary)
        session.sendMessage(dictionary, replyHandler: { answer in
            let answer = Reply(dictionary: answer)
            Task { @MainActor in self.answered(answer.dictionary, to: id) }
        }, errorHandler: { _ in
            Task { @MainActor in self.transfer(reply.dictionary, id: id) }
        })
        // No answer within ten seconds: the queued transport takes it.
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 10_000_000_000)
            self.transfer(reply.dictionary, id: id)
        }
    }

    /// Hands a request to `transferUserInfo` once, if it is still waiting.
    private func transfer(_ dictionary: [String: Any], id: UUID) {
        guard let session, model.isWaiting(id), !model.isTransferred(id) else { return }
        session.transferUserInfo(dictionary)
        model.markTransferred(id)
    }

    func ask(_ ask: WatchDayAsk) {
        guard let session else { return }
        let dictionary: [String: Any]
        do {
            dictionary = try WatchMessage.dayAsk(ask).dictionary()
        } catch {
            NSLog("Thock could not write a request for the phone: \(error)")
            model.dayAskFailed(ask.day)
            return
        }
        session.sendMessage(dictionary, replyHandler: { answer in
            let answer = Reply(dictionary: answer)
            Task { @MainActor in self.answered(answer.dictionary, to: ask.id) }
        }, errorHandler: { _ in
            Task { @MainActor in self.model.dayAskFailed(ask.day) }
        })
    }

    private func answered(_ dictionary: [String: Any], to id: UUID) {
        guard !dictionary.isEmpty, let message = try? WatchMessage(dictionary: dictionary) else {
            model.unreadableAnswer(to: id)
            return
        }
        model.receive(message)
    }

    private func received(_ dictionary: [String: Any]) {
        do {
            model.receive(try WatchMessage(dictionary: dictionary))
        } catch WatchLinkError.unknownVersion(let version) {
            NSLog("Thock ignored a message of version \(version.map(String.init) ?? "none") from the phone")
            model.receivedUnreadable()
        } catch {
            NSLog("Thock ignored a message from the phone it cannot read: \(error)")
        }
        finishRefreshesIfIdle()
    }

    func hold(_ task: WKWatchConnectivityRefreshBackgroundTask) {
        refreshTasks.append(task)
        finishRefreshesIfIdle()
    }

    private func finishRefreshesIfIdle() {
        guard session?.hasContentPending != true else { return }
        for task in refreshTasks {
            task.setTaskCompletedWithSnapshot(false)
        }
        refreshTasks.removeAll()
    }
}

/// A property-list dictionary, carried between WatchConnectivity's queue and
/// the main actor.
private struct Reply: @unchecked Sendable {
    let dictionary: [String: Any]
}

extension WatchConnection: WCSessionDelegate {
    nonisolated func session(_ session: WCSession, activationDidCompleteWith activationState: WCSessionActivationState, error: Error?) {
        if let error {
            NSLog("Thock could not reach the phone: \(error)")
        }
        let context = Reply(dictionary: session.receivedApplicationContext)
        Task { @MainActor in
            if !context.dictionary.isEmpty {
                self.received(context.dictionary)
            }
            // A transfer the system no longer holds (a re-pair, a
            // reinstall) and the phone never answered goes again; the phone
            // answers a request it already has without writing it twice.
            let outstanding = Set(session.outstandingUserInfoTransfers.compactMap { transfer in
                (try? WatchMessage(dictionary: transfer.userInfo))?.requestID
            })
            self.model.reconcileTransfers(outstanding: outstanding)
            self.model.resendWaiting()
            self.finishRefreshesIfIdle()
        }
    }

    nonisolated func sessionReachabilityDidChange(_ session: WCSession) {
        guard session.isReachable else { return }
        Task { @MainActor in self.model.phoneInReach() }
    }

    nonisolated func session(_ session: WCSession, didReceiveApplicationContext applicationContext: [String: Any]) {
        let context = Reply(dictionary: applicationContext)
        Task { @MainActor in self.received(context.dictionary) }
    }

    nonisolated func session(_ session: WCSession, didReceiveUserInfo userInfo: [String: Any] = [:]) {
        let info = Reply(dictionary: userInfo)
        Task { @MainActor in self.received(info.dictionary) }
    }

    nonisolated func session(_ session: WCSession, didReceiveMessage message: [String: Any]) {
        let info = Reply(dictionary: message)
        Task { @MainActor in self.received(info.dictionary) }
    }
}
