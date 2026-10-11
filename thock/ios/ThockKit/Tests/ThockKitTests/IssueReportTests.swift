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
        return IssueReport(appVersion: "1.0", build: "11", system: "iOS 18.1 · iPhone15,2", phoneName: "Diego's iPhone",
                           address: "https://plus.thethock.com", isPractice: false, isConnected: connected,
                           state: .offline, waitingForDesk: 2, notesHere: 120, diagnostics: diagnostics)
    }

    private let png = Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] + [UInt8](repeating: 0, count: 16))
    private let jpeg = Data([0xFF, 0xD8, 0xFF, 0xE0] + [UInt8](repeating: 0, count: 16))

    func testDetailsCarryTheFacts() {
        let details = report(failures: ["daily/2026-10-02.md: did not open with this phone's key"]).details
        XCTAssertEqual(details, """
        Thock for iPhone 1.0 (11) · iOS 18.1 · iPhone15,2
        Connection: connected as Diego's iPhone
        Status: can't reach the desk's copy · 2 changes waiting for the desk
        Notes here: 120 · at the desk's copy: 121
        Version 40 of 41 · writes not yet at the desk: 1
        Address: https://plus.thethock.com
        Last check: 2026-10-02T21:14:00Z
        Problem: 503 unavailable: try later
        Could not take: daily/2026-10-02.md: did not open with this phone's key
        """)
    }

    func testLongFailureListsAreCounted() {
        let details = report(failures: (1...13).map { "notes/\($0).md: not text" }).details
        XCTAssertTrue(details.contains("Could not take: notes/10.md: not text"))
        XCTAssertFalse(details.contains("notes/11.md"))
        XCTAssertTrue(details.hasSuffix("and 3 more"))
    }

    func testPracticeAndUnconnectedPhonesSayLittle() {
        var practice = report(connected: false)
        practice.isPractice = true
        practice.state = .upToDate
        practice.waitingForDesk = 0
        let details = practice.details
        XCTAssertTrue(details.contains("Connection: practice notebook"))
        XCTAssertTrue(details.contains("Status: up to date"))
        XCTAssertFalse(details.contains("Notes here"))
        XCTAssertFalse(details.contains("Address:"))
        XCTAssertTrue(report(connected: false).details.contains("Connection: not connected"))
    }

    func testPayloadIsWhatTheServiceReads() throws {
        let report = report()
        let payload = report.payload(description: "Nothing arrives", screenshots: [png, jpeg, png, png])
        let data = try JSONSerialization.data(withJSONObject: payload.mapValues { $0 ?? NSNull() })
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(json["description"] as? String, "Nothing arrives")
        XCTAssertEqual(json["details"] as? String, report.details)
        XCTAssertEqual(json["app_version"] as? String, "1.0")
        XCTAssertEqual(json["build"] as? String, "11")
        XCTAssertEqual(json["system"] as? String, "iOS 18.1 · iPhone15,2")
        let screenshots = try XCTUnwrap(json["screenshots"] as? [[String: String]])
        XCTAssertEqual(screenshots.count, IssueReport.maxScreenshots, "a fourth screenshot is dropped, not sent")
        XCTAssertEqual(screenshots.map { $0["content_type"] }, ["image/png", "image/jpeg", "image/png"])
        XCTAssertEqual(screenshots[0]["data"], png.base64EncodedString())
    }

    func testImageTypeComesFromTheBytes() {
        XCTAssertEqual(IssueReport.imageType(of: png), "image/png")
        XCTAssertEqual(IssueReport.imageType(of: jpeg), "image/jpeg")
        XCTAssertNil(IssueReport.imageType(of: Data("<html>".utf8)))
        XCTAssertNil(IssueReport.imageType(of: Data()))
    }
}
