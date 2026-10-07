import XCTest
@testable import ThockKit

/// The backlog on the phone (V38 §8): what the screen reads, and that every
/// gesture changes one block of `backlog.md` and nothing else.
final class BacklogTests: XCTestCase {
    let day = VaultDay(year: 2026, month: 10, day: 6)
    var config: VaultConfig { VaultConfig(config: SampleVault.config) }

    let backlog = "# Backlog\n\n## Soon\n\n- [ ] Renew passport\n- [ ] Call the dentist <!--gmail:0a1b2c3d4e5f-->\n\n### Home\n\n- [ ] Fix the gate\n  - the hinge first\n\n  - then the latch\n- [ ] Buy a smoke alarm\n- [x] Done by hand\n\n## Someday\n\n- [ ] Learn woodworking\n\n### Thock\n\n- [ ] Week widget\n\n## Completed\n\n- [x] Book the car ✅ 2026-10-01\n- [x] Undated\n- [x] Pay the bill ✅ 2026-10-03\n"

    func writes() -> PhoneWrites {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        // 2026-10-06 09:41 UTC
        return PhoneWrites(config: config, deviceID: "c41a9e0d7b2f4a61", now: Date(timeIntervalSince1970: 1_791_279_660), calendar: calendar)
    }

    func changedLines(_ before: String, _ after: String) -> (removed: [String], added: [String]) {
        let old = before.components(separatedBy: "\n")
        let new = after.components(separatedBy: "\n")
        var oldCounts: [String: Int] = [:]
        for line in old { oldCounts[line, default: 0] += 1 }
        var newCounts: [String: Int] = [:]
        for line in new { newCounts[line, default: 0] += 1 }
        var removed: [String] = []
        var added: [String] = []
        for line in old where (oldCounts[line] ?? 0) > (newCounts[line] ?? 0) {
            removed.append(line)
            oldCounts[line, default: 0] -= 1
        }
        for line in new where (newCounts[line] ?? 0) > (oldCounts[line] ?? 0) {
            added.append(line)
            newCounts[line, default: 0] -= 1
        }
        return (removed, added)
    }

    func apply(_ planned: PlannedWrite, to text: String) -> String {
        SyncCore.apply(existing: text, write: planned.document).text
    }

    func testTheFileIsReadAsTheDeskReadsIt() throws {
        let view = BacklogView(text: backlog, config: config)
        XCTAssertEqual(view.soon.title, "Soon")
        XCTAssertTrue(view.soon.exists)
        XCTAssertEqual(view.soon.groups.map(\.name), [nil, "Home"])
        XCTAssertEqual(view.soon.looseGroup.tasks.map(\.label), ["Renew passport", "Call the dentist"])
        XCTAssertEqual(view.soon.categories[0].tasks.map(\.label), ["Fix the gate", "Buy a smoke alarm", "Done by hand"])
        XCTAssertEqual(view.soon.categories[0].openTasks.count, 2)
        XCTAssertEqual(view.soon.openCount, 4)
        XCTAssertEqual(view.someday.groups.map(\.name), [nil, "Thock"])
        XCTAssertEqual(view.someday.openCount, 2)

        let gate = try XCTUnwrap(view.soon.categories[0].tasks.first)
        XCTAssertEqual(gate.childLines, 3)
        XCTAssertEqual(gate.block, ["- [ ] Fix the gate", "  - the hinge first", "", "  - then the latch"])
        XCTAssertEqual(gate.heading, HeadingRef(text: "Home", level: 3))
        let dentist = view.soon.looseGroup.tasks[1]
        XCTAssertEqual(dentist.heading, HeadingRef(text: "Soon", level: 2))
        XCTAssertEqual(dentist.hash, SyncCore.lineHash("Call the dentist"))
        XCTAssertEqual(dentist.raw, "- [ ] Call the dentist <!--gmail:0a1b2c3d4e5f-->")

        XCTAssertEqual(view.completedTasks.map(\.label), ["Pay the bill", "Book the car", "Undated"])
        XCTAssertEqual(view.completedTasks.map(\.completedOn), ["2026-10-03", "2026-10-01", nil])
        XCTAssertEqual(view.group(of: gate)?.name, "Home")
        XCTAssertEqual(view.section(of: gate)?.kind, .soon)
    }

    func testAMissingFileOrSectionIsAnEmptyBacklog() {
        let empty = BacklogView(text: "", config: config)
        XCTAssertFalse(empty.soon.exists)
        XCTAssertEqual(empty.soon.title, "Soon")
        XCTAssertEqual(empty.soon.groups.count, 1)
        XCTAssertEqual(empty.openSections.map(\.openCount), [0, 0])
        XCTAssertTrue(empty.completedTasks.isEmpty)

        let translated = BacklogView(text: "## Em breve\n\n- [ ] Passaporte\n\n## Algum dia\n", config: VaultConfig(config: "[backlog]\nheadings = { soon = \"Em breve\", someday = \"Algum dia\" }\n"))
        XCTAssertEqual(translated.soon.title, "Em breve")
        XCTAssertEqual(translated.soon.openCount, 1)
        XCTAssertTrue(translated.someday.exists)
        XCTAssertFalse(translated.completed.exists)
        XCTAssertEqual(translated.soon.tasks[0].heading, HeadingRef(text: "Em breve", level: 2))
    }

    func testOrdinalsCountInsideTheGroupOnly() throws {
        let twins = "## Soon\n\n- [ ] Call Ana\n- [ ] Call Ana\n\n### Home\n\n- [ ] Call Ana\n\n## Someday\n"
        let view = BacklogView(text: twins, config: config)
        XCTAssertEqual(view.soon.looseGroup.tasks.map(\.ordinal), [0, 1])
        XCTAssertEqual(view.soon.categories[0].tasks.map(\.ordinal), [0])
        let second = view.soon.looseGroup.tasks[1]
        let after = apply(writes().backlogRemove(second), to: twins)
        XCTAssertEqual(after, "## Soon\n\n- [ ] Call Ana\n\n### Home\n\n- [ ] Call Ana\n\n## Someday\n")
    }

    func testEveryGestureChangesOneBlock() throws {
        let builder = writes()
        let view = BacklogView(text: backlog, config: config)
        let gate = try XCTUnwrap(view.soon.categories[0].tasks.first)
        let dentist = view.soon.looseGroup.tasks[1]
        let passport = view.soon.looseGroup.tasks[0]

        // Reorder: the block and its children move, nothing else differs.
        var after = apply(builder.backlogMove(gate, to: view.soon.looseGroup, place: .after(passport)), to: backlog)
        var change = changedLines(backlog, after)
        XCTAssertEqual(change.removed, [])
        XCTAssertEqual(change.added, [])
        XCTAssertTrue(after.contains("- [ ] Renew passport\n- [ ] Fix the gate\n  - the hinge first\n\n  - then the latch\n- [ ] Call the dentist <!--gmail:0a1b2c3d4e5f-->\n\n### Home\n\n- [ ] Buy a smoke alarm"))

        after = apply(builder.backlogMove(dentist, to: view.soon.looseGroup, place: .top), to: backlog)
        XCTAssertTrue(after.contains("## Soon\n\n- [ ] Call the dentist <!--gmail:0a1b2c3d4e5f-->\n- [ ] Renew passport\n\n### Home"))

        // Into a category, and from a category to the other section with
        // the category recreated there.
        after = apply(builder.backlogMove(dentist, to: view.someday.categories[0], place: .end), to: backlog)
        XCTAssertTrue(after.contains("### Thock\n\n- [ ] Week widget\n- [ ] Call the dentist <!--gmail:0a1b2c3d4e5f-->\n\n## Completed"))
        let toSomeday = builder.backlogMove(gate, from: view.soon.categories[0], toSection: view.someday)
        XCTAssertEqual(toSomeday.document.createUnder, view.someday.heading)
        after = apply(toSomeday, to: backlog)
        XCTAssertTrue(after.contains("### Thock\n\n- [ ] Week widget\n\n### Home\n- [ ] Fix the gate\n  - the hinge first\n\n  - then the latch\n\n## Completed"))
        XCTAssertTrue(after.contains("### Home\n\n- [ ] Buy a smoke alarm\n- [x] Done by hand\n\n## Someday"))
        let loose = builder.backlogMove(passport, from: view.soon.looseGroup, toSection: view.someday)
        XCTAssertNil(loose.document.createUnder)
        XCTAssertTrue(apply(loose, to: backlog).contains("- [ ] Learn woodworking\n- [ ] Renew passport\n\n### Thock"))

        // Edit keeps the checkbox and the hidden marker.
        let edit = try XCTUnwrap(builder.backlogEdit(dentist, text: "Call the dentist about Friday"))
        change = changedLines(backlog, apply(edit, to: backlog))
        XCTAssertEqual(change.removed, ["- [ ] Call the dentist <!--gmail:0a1b2c3d4e5f-->"])
        XCTAssertEqual(change.added, ["- [ ] Call the dentist about Friday <!--gmail:0a1b2c3d4e5f-->"])
        XCTAssertNil(builder.backlogEdit(dentist, text: "  Call the dentist "))
        XCTAssertNil(builder.backlogEdit(dentist, text: ""))

        // Add lands below the loose tasks, or at the end of the category.
        let add = try XCTUnwrap(builder.backlogAdd("Water the plants", to: view.soon.looseGroup))
        XCTAssertTrue(apply(add, to: backlog).contains("<!--gmail:0a1b2c3d4e5f-->\n- [ ] Water the plants\n\n### Home"))
        let addToHome = try XCTUnwrap(builder.backlogAdd("Water the plants", to: view.soon.categories[0]))
        XCTAssertTrue(apply(addToHome, to: backlog).contains("- [x] Done by hand\n- [ ] Water the plants\n\n## Someday"))

        // Remove takes the children and one of the two blank lines.
        change = changedLines(backlog, apply(builder.backlogRemove(gate), to: backlog))
        XCTAssertEqual(change.removed, ["", "- [ ] Fix the gate", "  - the hinge first", "  - then the latch"])
        XCTAssertEqual(change.added, [])
    }

    func testTickRecordsTodayFirstThenFilesUnderCompleted() throws {
        let builder = writes()
        let view = BacklogView(text: backlog, config: config)
        let gate = try XCTUnwrap(view.soon.categories[0].tasks.first)
        let today = try XCTUnwrap(SampleVault.make(today: day).files["daily/2026-10-06.md"])
        let planned = builder.backlogTick(gate, completed: view.completed, todayNote: today, template: nil)
        XCTAssertEqual(planned.map(\.document.path), ["daily/2026-10-06.md", "backlog.md"])
        XCTAssertEqual(planned[0].document.lines, ["- [x] Fix the gate"])
        XCTAssertEqual(planned[0].document.placement, .beforeChildren)
        XCTAssertTrue(planned[0].document.createFromTemplate)
        let note = apply(planned[0], to: today)
        XCTAssertTrue(note.contains("- [x] Fix the gate\n\n### Calendar"))
        let after = apply(planned[1], to: backlog)
        XCTAssertTrue(after.contains("### Home\n\n- [ ] Buy a smoke alarm\n- [x] Done by hand\n\n## Someday"))
        XCTAssertTrue(after.hasSuffix("- [x] Pay the bill ✅ 2026-10-03\n- [x] Fix the gate ✅ 2026-10-06\n  - the hinge first\n\n  - then the latch\n"))
        XCTAssertEqual(SyncCore.apply(existing: after, write: planned[1].document).outcome, .noop)

        // The marker travels with the stamp, so the desk still knows the
        // task came from Gmail.
        let dentist = view.soon.looseGroup.tasks[1]
        let marked = builder.backlogTick(dentist, completed: view.completed, todayNote: today, template: nil)
        XCTAssertEqual(marked[1].document.newLine, "- [x] Call the dentist ✅ 2026-10-06 <!--gmail:0a1b2c3d4e5f-->")
        XCTAssertEqual(marked[0].document.lines, ["- [x] Call the dentist"])

        // Without today's note, the tick creates it from the template.
        let template = try XCTUnwrap(SampleVault.make(today: day).files["templates/daily.md"])
        let fresh = builder.backlogTick(gate, completed: view.completed, todayNote: nil, template: template)
        let created = SyncCore.apply(existing: nil, write: fresh[0].document, seed: Template.expand(template, day: day, time: "09:41", title: "2026-10-06"))
        XCTAssertEqual(created.outcome, .created)
        XCTAssertTrue(NoteView(text: created.text, config: config).planner.items.contains { $0.label == "Fix the gate" && $0.done })
    }

    func testMoveToTodayIsThePlannersMoveToSoonInReverse() throws {
        let builder = writes()
        let view = BacklogView(text: backlog, config: config)
        let gate = try XCTUnwrap(view.soon.categories[0].tasks.first)
        let today = try XCTUnwrap(SampleVault.make(today: day).files["daily/2026-10-06.md"])
        let planned = builder.backlogMoveToToday(gate, todayNote: today, template: nil)
        XCTAssertEqual(planned.map(\.document.kind), [.append, .removeBlock])
        XCTAssertEqual(planned[0].document.lines, gate.block)
        XCTAssertTrue(apply(planned[0], to: today).contains("- [ ] Fix the gate\n  - the hinge first\n\n  - then the latch\n\n### Calendar"))
        XCTAssertTrue(apply(planned[1], to: backlog).contains("### Home\n\n- [ ] Buy a smoke alarm"))
    }

    func testTheSampleBacklogReadsAndMovesOnTheSession() throws {
        let store = try VaultStore(url: nil)
        store.setMeta("vault_id", "v")
        let sample = SampleVault.make(today: day)
        for (index, (path, content)) in sample.files.sorted(by: { $0.key < $1.key }).enumerated() {
            try store.applySnapshot(path: path, version: index + 1, content: content, contentHash: "h\(index)", blobID: "b\(index)")
        }
        let session = VaultSession(store: store)
        let view = session.backlog()
        XCTAssertEqual(view.soon.openCount, 4)
        XCTAssertEqual(view.someday.openCount, 2)
        XCTAssertEqual(view.completedTasks.count, 1)
        let passport = try XCTUnwrap(view.soon.looseGroup.tasks.first)
        try session.moveBacklog(passport, from: view.soon.looseGroup, toSection: view.someday)
        let moved = session.backlog()
        XCTAssertEqual(moved.soon.openCount, 3)
        XCTAssertEqual(moved.someday.looseGroup.tasks.map(\.label).last, "Renew the passport")
        XCTAssertEqual(store.pending().count, 1)
    }
}
