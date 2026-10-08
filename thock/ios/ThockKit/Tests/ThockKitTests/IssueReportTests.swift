import XCTest
@testable import ThockKit

final class IssueReportTests: XCTestCase {
    private func report(connected: Bool = true, failures: [String] = []) -> IssueReport {
        var diagnostics = SyncDiagnostics()
        diagnostics.lastRound = Date(timeIntervalSince1970: 1_790_975_640)
        diagnostics.lastError = "503 unavailable: try later"
        diagnostics.serverFileCount = 121
        diagnostics.serverLatestVersion = 41
        diagnostics.serverPendingWrites = 1
        diagnostics.cursor = 40
        diagnostics.failures = failures
        return IssueReport(appVersion: "1.0", build: "10", system: "iOS 18.1 · iPhone15,2", phoneName: "Diego's iPhone",
                           address: "https://plus.thethock.com", isPractice: false, isConnected: connected,
                           state: .offline, waitingForDesk: 2, notesHere: 120, diagnostics: diagnostics)
    }

    func testBodyCarriesTheFactsAndRoomToWrite() {
        let body = report(failures: ["daily/2026-10-02.md: did not open with this phone's key"]).body
        XCTAssertTrue(body.hasPrefix("Tell us what happened, and what you expected instead:\n\n\n\n— Details Thock added —\n"))
        for line in [
            "Thock for iPhone 1.0 (10) · iOS 18.1 · iPhone15,2",
            "Connection: connected as Diego's iPhone",
            "Status: can't reach the desk's copy · 2 changes waiting for the desk",
            "Notes here: 120 · at the desk's copy: 121",
            "Version 40 of 41 · writes not yet at the desk: 1",
            "Address: https://plus.thethock.com",
            "Last check: 2026-10-02T21:14:00Z",
            "Problem: 503 unavailable: try later",
            "Could not take: daily/2026-10-02.md: did not open with this phone's key",
        ] {
            XCTAssertTrue(body.contains(line), "missing: \(line)\n\(body)")
        }
    }

    func testLongFailureListsAreCounted() {
        let body = report(failures: (1...13).map { "notes/\($0).md: not text" }).body
        XCTAssertTrue(body.contains("Could not take: notes/10.md: not text"))
        XCTAssertFalse(body.contains("notes/11.md"))
        XCTAssertTrue(body.hasSuffix("and 3 more"))
    }

    func testPracticeAndUnconnectedPhonesSayLittle() {
        var practice = report(connected: false)
        practice.isPractice = true
        practice.state = .upToDate
        practice.waitingForDesk = 0
        let body = practice.body
        XCTAssertTrue(body.contains("Connection: practice notebook"))
        XCTAssertTrue(body.contains("Status: up to date"))
        XCTAssertFalse(body.contains("Notes here"))
        XCTAssertFalse(body.contains("Address:"))
        XCTAssertTrue(report(connected: false).body.contains("Connection: not connected"))
    }

    func testMailLinkOpensOnTheReport() throws {
        let report = report()
        let url = try XCTUnwrap(report.mailURL(to: "help@example.com"))
        XCTAssertEqual(url.scheme, "mailto")
        let components = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false))
        XCTAssertEqual(components.path, "help@example.com")
        let query = Dictionary(uniqueKeysWithValues: (components.queryItems ?? []).map { ($0.name, $0.value ?? "") })
        XCTAssertEqual(query["subject"], IssueReport.subject)
        XCTAssertEqual(query["body"], report.body.replacingOccurrences(of: "\n", with: "\r\n"))
        // Nothing a mail app would read as a separator survives unencoded.
        let raw = url.absoluteString
        XCTAssertFalse(raw.contains("+"))
        XCTAssertFalse(raw.contains(" "))
        XCTAssertEqual(raw.filter { $0 == "&" }.count, 1)
    }
}
