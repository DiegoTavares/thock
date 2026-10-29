import Foundation
import ThockKit
import WatchConnectivity

/// The phone's end of the watch link (V48 §6.2): sends the snapshot, takes
/// the watch's requests and answers them. The watch never sees the vault;
/// every request runs through `AppModel.receive`, the phone's own calls.
@MainActor
final class PhoneWatchLink: NSObject {
    static let shared = PhoneWatchLink()

    /// The snapshot last handed over, without its instant, so a store change
    /// that changes nothing the watch draws sends nothing.
    private var lastSent: Data?
    /// Today's day as the complication last got it: a complication transfer
    /// is rationed, so it goes only when today's rows change.
    private var lastToday: WatchDay?

    private var session: WCSession? {
        WCSession.isSupported() ? WCSession.default : nil
    }

    /// Called at launch, before anything else can arrive, so a request that
    /// launched the app in the background is received.
    func activate() {
        guard let session else { return }
        session.delegate = self
        session.activate()
    }

    /// Whether a watch with the app is there to send to; nothing is built
    /// for it otherwise.
    var canSend: Bool {
        guard let session, session.activationState == .activated else { return false }
        return session.isPaired && session.isWatchAppInstalled
    }

    func send(_ snapshot: WatchSnapshot) {
        guard canSend, let session else { return }
        do {
            var unstamped = snapshot
            unstamped.madeAt = .distantPast
            let comparable = try WatchMessage.snapshot(unstamped).encoded()
            guard comparable != lastSent else { return }
            let dictionary = try WatchMessage.snapshot(snapshot).dictionary()
            try session.updateApplicationContext(dictionary)
            lastSent = comparable
            let today = VaultDay(iso: snapshot.today).flatMap(snapshot.day)
            if today != lastToday, session.remainingComplicationUserInfoTransfers > 0 {
                session.transferCurrentComplicationUserInfo(dictionary)
                lastToday = today
            }
        } catch {
            NSLog("Thock could not send the watch its snapshot: \(error)")
        }
    }

    /// Reads a request as WatchConnectivity carries it. One the phone cannot
    /// read (a newer or older watch app) is logged and ignored.
    static func decode(_ dictionary: [String: Any]) -> WatchMessage? {
        do {
            return try WatchMessage(dictionary: dictionary)
        } catch {
            NSLog("Thock ignored a message from the watch it cannot read: \(error)")
            return nil
        }
    }

    /// Answers by `transferUserInfo`, which the system delivers across
    /// restarts of either app.
    private func transfer(_ message: WatchMessage) {
        guard let session else { return }
        do {
            session.transferUserInfo(try message.dictionary())
        } catch {
            NSLog("Thock could not answer the watch: \(error)")
        }
    }

    private func handle(_ dictionary: [String: Any], reply: ReplyHandler?) {
        guard let request = Self.decode(dictionary) else {
            // An empty reply tells the watch its message was not understood,
            // so it asks for an update instead of retrying.
            reply?.call([:])
            return
        }
        Task { @MainActor in
            let answer = await AppDelegate.model.receive(request)
            guard let answer else {
                reply?.call([:])
                return
            }
            if let reply {
                do {
                    reply.call(try answer.dictionary())
                } catch {
                    NSLog("Thock could not answer the watch: \(error)")
                    transfer(answer)
                }
            } else {
                transfer(answer)
            }
        }
    }

    private func watchStateChanged() {
        lastSent = nil
        lastToday = nil
        AppDelegate.model.scheduleWatchSnapshot()
    }
}

/// `WCSession`'s reply block, carried onto the main actor.
private struct ReplyHandler: @unchecked Sendable {
    let call: ([String: Any]) -> Void
}

extension PhoneWatchLink: WCSessionDelegate {
    nonisolated func session(_ session: WCSession, activationDidCompleteWith activationState: WCSessionActivationState, error: Error?) {
        if let error {
            NSLog("Thock could not reach the watch: \(error)")
        }
        Task { @MainActor in self.watchStateChanged() }
    }

    nonisolated func sessionDidBecomeInactive(_ session: WCSession) {}

    nonisolated func sessionDidDeactivate(_ session: WCSession) {
        // Switching watches: the session for the new one starts here.
        session.activate()
    }

    nonisolated func sessionWatchStateDidChange(_ session: WCSession) {
        Task { @MainActor in self.watchStateChanged() }
    }

    nonisolated func session(_ session: WCSession, didReceiveMessage message: [String: Any], replyHandler: @escaping ([String: Any]) -> Void) {
        let reply = ReplyHandler(call: replyHandler)
        let request = Request(dictionary: message)
        Task { @MainActor in self.handle(request.dictionary, reply: reply) }
    }

    nonisolated func session(_ session: WCSession, didReceiveUserInfo userInfo: [String: Any] = [:]) {
        let request = Request(dictionary: userInfo)
        Task { @MainActor in self.handle(request.dictionary, reply: nil) }
    }
}

/// A property-list dictionary, which is safe to carry between threads.
private struct Request: @unchecked Sendable {
    let dictionary: [String: Any]
}
