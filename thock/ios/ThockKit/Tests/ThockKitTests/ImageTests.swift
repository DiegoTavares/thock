import CoreGraphics
import ImageIO
import UniformTypeIdentifiers
import XCTest
@testable import ThockKit

/// Pictures on the phone (V39 §8): the downsizer, the file names, the
/// `put_file` and the lines a capture links them with, and the store.
final class ImageTests: XCTestCase {
    var config: VaultConfig { VaultConfig(config: SampleVault.config) }

    func writes() -> PhoneWrites {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        // 2026-10-10 09:31 UTC
        return PhoneWrites(config: config, deviceID: "c41a9e0d7b2f4a61", now: Date(timeIntervalSince1970: 1_791_624_660), calendar: calendar)
    }

    /// A PNG of the given size, drawn so JPEG has real work to do.
    func png(width: Int, height: Int) throws -> Data {
        let space = CGColorSpaceCreateDeviceRGB()
        let context = try XCTUnwrap(CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0, space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        for x in stride(from: 0, to: width, by: 16) {
            context.setFillColor(CGColor(red: CGFloat(x % 255) / 255, green: 0.4, blue: CGFloat((x * 7) % 255) / 255, alpha: 1))
            context.fill(CGRect(x: x, y: 0, width: 16, height: height))
        }
        let image = try XCTUnwrap(context.makeImage())
        let output = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(output, UTType.png.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(destination, image, nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        return output as Data
    }

    func testALargePictureIsDownsizedToAJPEGUnderBudget() throws {
        let big = try png(width: 3000, height: 2000)
        let prepared = try XCTUnwrap(ImageDownsizer.prepare(big, name: "IMG_0421 whiteboard"))
        XCTAssertEqual(prepared.fileExtension, "jpg")
        XCTAssertEqual(prepared.name, "img-0421-whiteboard")
        XCTAssertLessThanOrEqual(prepared.bytes.count, ImageDownsizer.budget)
        let size = try XCTUnwrap(ImageDownsizer.pixelSize(of: prepared.bytes))
        XCTAssertEqual(size.width, 1600)
        XCTAssertEqual(size.height, 1067)
        XCTAssertEqual(CGImageSourceGetType(CGImageSourceCreateWithData(prepared.bytes as CFData, nil)!).map { $0 as String }, UTType.jpeg.identifier)
    }

    func testASmallPNGStaysAPNG() throws {
        let small = try png(width: 320, height: 200)
        let prepared = try XCTUnwrap(ImageDownsizer.prepare(small, name: "Screenshot"))
        XCTAssertEqual(prepared.fileExtension, "png")
        XCTAssertEqual(prepared.bytes, small)
        XCTAssertNil(ImageDownsizer.prepare(Data("not a picture".utf8), name: "x"))
    }

    func testPicturesAreNamedByTheMomentAndNeverCollide() throws {
        let builder = writes()
        let image = ImageAttachment(name: "whiteboard", bytes: Data([1, 2, 3]), fileExtension: "jpg")
        XCTAssertEqual(builder.imagePath(for: image, taken: { _ in false }), "images/2026-10-10-0931-whiteboard.jpg")
        XCTAssertEqual(builder.imagePath(for: image, taken: { $0 == "images/2026-10-10-0931-whiteboard.jpg" }), "images/2026-10-10-0931-whiteboard-2.jpg")
        let nameless = ImageAttachment(name: "", bytes: Data(), fileExtension: "png")
        XCTAssertEqual(builder.imagePath(for: nameless, taken: { _ in false }), "images/2026-10-10-0931-photo.png")

        let put = builder.putFile(path: "images/a.jpg", bytes: Data([1, 2, 3])).document
        XCTAssertEqual(put.kind, .putFile)
        XCTAssertEqual(put.contentBase64, "AQID")
        XCTAssertEqual(put.contentHash, SyncCore.sha256Hex(Data([1, 2, 3])))
        XCTAssertTrue(SyncCore.isSyncableImagePath(put.path, imagesDir: "images"))
        XCTAssertEqual(try WriteDocument.parse(put.json()).contentBase64, "AQID")
    }

    func testAnInboxCaptureWithPicturesWritesThemFirstAndLinksThem() throws {
        let builder = writes()
        let images = [
            ImageAttachment(name: "whiteboard", bytes: Data([1]), fileExtension: "jpg"),
            ImageAttachment(name: "whiteboard", bytes: Data([2]), fileExtension: "jpg"),
        ]
        let captured = try XCTUnwrap(builder.capture(blocks: Blocks.parse("Sketched the sync flow\n\nWith Ana."), destination: .inbox, images: images, todayNote: nil, template: nil, taken: { _ in false }))
        XCTAssertEqual(captured.writes.map(\.document.kind), [.putFile, .putFile, .create])
        XCTAssertEqual(captured.writes[0].document.path, "images/2026-10-10-0931-whiteboard.jpg")
        XCTAssertEqual(captured.writes[1].document.path, "images/2026-10-10-0931-whiteboard-2.jpg")
        let note = try XCTUnwrap(captured.writes[2].document.content)
        XCTAssertTrue(note.hasSuffix("# Sketched the sync flow\n\nWith Ana.\n\n![whiteboard](/images/2026-10-10-0931-whiteboard.jpg)\n![whiteboard](/images/2026-10-10-0931-whiteboard-2.jpg)\n"), note)
        XCTAssertEqual(captured.record.imageCount, 2)
        XCTAssertTrue(PhoneWrites.inboxHasBody(note))

        // Pictures alone are a capture of their own.
        let alone = try XCTUnwrap(builder.capture(blocks: [], destination: .inbox, images: [images[0]], todayNote: nil, template: nil, taken: { _ in false }))
        let aloneNote = try XCTUnwrap(alone.writes[1].document.content)
        XCTAssertTrue(aloneNote.contains("# Photo\n\n![whiteboard](/images/2026-10-10-0931-whiteboard.jpg)\n"), aloneNote)
        XCTAssertNil(builder.capture(blocks: [], destination: .inbox, todayNote: nil, template: nil, taken: { _ in false }))
    }

    func testTodayAndBacklogCapturesCarryPicturesAsTheTasksContinuation() throws {
        let builder = writes()
        let image = ImageAttachment(name: "receipt", bytes: Data([1]), fileExtension: "png")
        let today = "# 2026-10-10\n\n## Day planner\n\n- [ ] 09:00 - 09:30 Standup\n\n## Personal\n"
        let task = try XCTUnwrap(builder.capture(blocks: Blocks.parse("- [ ] Expense the lunch"), destination: .today, images: [image], todayNote: today, template: nil, taken: { _ in false }))
        XCTAssertEqual(task.writes.map(\.document.kind), [.putFile, .append])
        XCTAssertEqual(task.writes[1].document.lines, ["- [ ] Expense the lunch", "  ![receipt](/images/2026-10-10-0931-receipt.png)"])
        let prose = try XCTUnwrap(builder.capture(blocks: Blocks.parse("Lunch with the team"), destination: .today, images: [image], todayNote: today, template: nil, taken: { _ in false }))
        XCTAssertEqual(prose.writes[1].document.lines, ["Lunch with the team", "![receipt](/images/2026-10-10-0931-receipt.png)"])
        let backlog = try XCTUnwrap(builder.capture(blocks: Blocks.parse("Frame the print"), destination: .backlog, images: [image], todayNote: nil, template: nil, taken: { _ in false }))
        XCTAssertEqual(backlog.writes[1].document.lines, ["- [ ] Frame the print", "  ![receipt](/images/2026-10-10-0931-receipt.png)"])
        let after = SyncCore.apply(existing: today, write: task.writes[1].document).text
        XCTAssertTrue(after.contains("- [ ] Expense the lunch\n  ![receipt](/images/2026-10-10-0931-receipt.png)\n"), after)
    }

    func testEditingAWaitingNoteAttachesPictures() throws {
        let builder = writes()
        let captured = try XCTUnwrap(builder.capture(blocks: Blocks.parse("Call Ana\n\nAbout the article."), destination: .inbox, todayNote: nil, template: nil, taken: { _ in false }))
        let created = try XCTUnwrap(captured.writes[0].document.content)
        let path = captured.writes[0].document.path
        let image = ImageAttachment(name: "cover", bytes: Data([9]), fileExtension: "jpg")
        let edit = try XCTUnwrap(builder.inboxEdit(path: path, note: created, blocks: Blocks.parse("Call Ana\n\nAbout the article."), images: [image]))
        XCTAssertEqual(edit.writes.map(\.document.kind), [.putFile, .replaceSection])
        let text = SyncCore.apply(existing: created, write: edit.writes[1].document).text
        XCTAssertTrue(text.hasSuffix("# Call Ana\n\nAbout the article.\n\n![cover](/images/2026-10-10-0931-cover.jpg)\n"), text)
    }

    func testASharedPictureIsAnInboxNote() throws {
        let builder = writes()
        let image = ImageAttachment(name: "IMG_0001", bytes: Data([1]), fileExtension: "jpg")
        let shared = try XCTUnwrap(builder.photoCapture(images: [image], text: "", taken: { _ in false }))
        XCTAssertEqual(shared.writes.map(\.document.kind), [.putFile, .create])
        XCTAssertEqual(shared.record.title, "Photo")
        XCTAssertEqual(shared.record.imageCount, 1)
        XCTAssertTrue(try XCTUnwrap(shared.writes[1].document.content).hasSuffix("# Photo\n\n![img-0001](/images/2026-10-10-0931-img-0001.jpg)\n"))
        let titled = try XCTUnwrap(builder.photoCapture(images: [image, image], text: "Whiteboard from the planning\nleft wall", taken: { _ in false }))
        XCTAssertEqual(titled.record.title, "Whiteboard from the planning")
        XCTAssertTrue(try XCTUnwrap(titled.writes[2].document.content).contains("# Whiteboard from the planning\n\nleft wall\n\n![img-0001](/images/2026-10-10-0931-img-0001.jpg)\n![img-0001](/images/2026-10-10-0931-img-0001-2.jpg)\n"))
        XCTAssertNil(builder.photoCapture(images: [], text: "x", taken: { _ in false }))
    }

    func testTheStoreKeepsThePictureAndQueuesItOnce() throws {
        let store = try VaultStore(url: nil)
        store.setMeta("vault_id", "v")
        try store.applySnapshot(path: VaultConfig.configPath, version: 1, content: SampleVault.config, contentHash: "h1", blobID: "b1")
        let session = VaultSession(store: store)
        let image = ImageAttachment(name: "whiteboard", bytes: Data([0x89, 0x50, 0x4e, 0x47]), fileExtension: "png")

        let record = try XCTUnwrap(try session.capture(blocks: Blocks.parse("Sketch"), destination: .inbox, images: [image]))
        XCTAssertEqual(record.imageCount, 1)
        let path = try XCTUnwrap(store.pending().first?.document.path)
        XCTAssertTrue(path.hasPrefix("images/"), path)
        XCTAssertEqual(store.blob(path), image.bytes)
        XCTAssertTrue(store.hasBlob(path))
        XCTAssertNil(store.content(path), "a picture is not a note")
        XCTAssertEqual(store.pending().map(\.document.kind), [.putFile, .create])
        XCTAssertEqual(store.captures().first?.imageCount, 1)

        // The same picture again takes the next name; a put_file for a path
        // already held changes nothing and queues nothing.
        let again = try XCTUnwrap(try session.capture(blocks: Blocks.parse("Sketch again"), destination: .inbox, images: [image]))
        XCTAssertEqual(again.imageCount, 1)
        XCTAssertEqual(store.pending().filter { $0.document.kind == .putFile }.map(\.document.path), [path, path.replacingOccurrences(of: ".png", with: "-2.png")])
        let duplicate = store.pending().first!.document
        XCTAssertEqual(try store.record([PlannedWrite(document: duplicate)]), [.noop])
        XCTAssertEqual(store.pending().count, 4)

        // A full pull that lists the picture leaves the blob alone.
        try store.removeFiles(notIn: [VaultConfig.configPath])
        XCTAssertTrue(store.hasBlob(path))
    }

    func testThePathRuleKeepsPicturesToTheImagesFolder() {
        XCTAssertTrue(SyncCore.isSyncableImagePath("images/2026-10-10-0931-whiteboard.jpg", imagesDir: "images"))
        XCTAssertTrue(SyncCore.isSyncableImagePath("images/receipt.PNG", imagesDir: "images"))
        XCTAssertTrue(SyncCore.isSyncableImagePath("pictures/a.png", imagesDir: "pictures"))
        for path in ["photo.png", "daily/photo.png", "images.png", "imagesx/photo.png", "images/note.md", "images/scan.pdf", "images/.png", "images/../photo.png", "/images/photo.png", ".thock/history/images/x.png"] {
            XCTAssertFalse(SyncCore.isSyncableImagePath(path, imagesDir: "images"), path)
        }
        XCTAssertFalse(SyncCore.isSyncablePath("images/receipt.png"))
        XCTAssertEqual(VaultConfig(config: "[images]\ndir = \"pictures\"\n").imagesDir, "pictures")
        XCTAssertEqual(VaultConfig(config: "").imagesDir, "images")
    }
}
