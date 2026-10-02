import Foundation

public enum Outcome: String, Codable, Sendable {
    case created
    case sectionAdded = "section_added"
    case applied
    case keptBoth = "kept_both"
    case noop
}

public struct Applied: Equatable, Sendable {
    public var text: String
    public var outcome: Outcome
}

extension SyncCore {
    /// Applies one write to a file (V34 API §8). Deterministic and total: the
    /// phone runs it the moment a write is made, the desk runs it when the
    /// write arrives, and both end with the same text.
    public static func apply(existing: String?, write: WriteDocument, seed: String? = nil) -> Applied {
        if let existing, effectPresent(content: existing, write: write) {
            return Applied(text: existing, outcome: .noop)
        }

        var created = false
        var file: TextFile
        if let existing {
            file = TextFile(existing)
        } else {
            switch write.kind {
            case .create:
                return Applied(text: write.content ?? "", outcome: .created)
            case .append where write.createFromTemplate && seed != nil:
                file = TextFile(seed ?? "")
            default:
                file = TextFile("")
                if let heading = write.heading {
                    file.insert([headingLine(heading), ""], at: 0)
                }
            }
            created = true
        }

        if write.kind == .create {
            let lines = TextFile(write.content ?? "").lines.map(\.text)
            insertAtEnd(of: file.wholeFile(), lines: lines, blankLineBefore: true, in: &file, section: nil)
            return Applied(text: file.text, outcome: .applied)
        }

        var sectionAdded = false
        var keptBoth = false
        let section: SectionRange
        if let reference = write.heading {
            if let found = file.resolve(reference) {
                section = file.section(of: found)
            } else {
                let index = addHeading(reference, to: &file)
                sectionAdded = true
                section = file.section(of: HeadingLine(index: index, level: reference.level, text: reference.text))
            }
        } else {
            section = file.wholeFile()
        }

        switch write.kind {
        case .create:
            break
        case .append:
            let index = write.placement == .beforeChildren ? section.ownEnd : section.bodyEnd
            insert(write.lines, at: index, blankLineBefore: write.blankLineBefore, in: &file, section: section)
        case .replaceLine:
            let newLine = write.newLine ?? ""
            if let target = targetLine(in: file, section: section, hash: write.lineHash ?? "", ordinal: write.ordinal) {
                file.lines[target].text = newLine
            } else {
                file.insert([newLine + conflictMarker], at: section.bodyEnd)
                keptBoth = true
            }
        case .removeLine:
            if let target = targetLine(in: file, section: section, hash: write.lineHash ?? "", ordinal: write.ordinal) {
                file.lines.remove(at: target)
            } else if !created, !sectionAdded {
                return Applied(text: file.text, outcome: .noop)
            }
            // Against a missing file the corpus still creates the heading:
            // every write yields a file, even one with nothing to remove.
        case .replaceSection:
            let current = sectionHash(lines: file.lines[section.body].map(\.text))
            if current == write.baseHash {
                file.replace(section.body, with: write.lines)
            } else if !write.lines.isEmpty {
                var lines = write.lines
                let marked = lines.firstIndex { !$0.allSatisfy(\.isWhitespace) } ?? 0
                lines[marked] += conflictMarker
                insert(lines, at: section.bodyEnd, blankLineBefore: true, in: &file, section: section)
                keptBoth = true
            }
        }

        let outcome: Outcome
        if created {
            outcome = .created
        } else if sectionAdded {
            outcome = .sectionAdded
        } else if keptBoth {
            outcome = .keptBoth
        } else {
            outcome = .applied
        }
        return Applied(text: file.text, outcome: outcome)
    }

    /// Whether a write's effect is already in `content` (V34 API §8.3). This
    /// is what makes re-applying after a crash safe, and what the phone uses
    /// to drop its own writes when a snapshot already carries them.
    public static func effectPresent(content: String, write: WriteDocument) -> Bool {
        let file = TextFile(content)
        switch write.kind {
        case .create:
            if normalisingLineEndings(content) == normalisingLineEndings(write.content ?? "") {
                return true
            }
            // A create over other content becomes an append (rule 3), so its
            // effect is present the way an append's is. Without this a
            // re-applied create would append its content a second time.
            let lines = TextFile(write.content ?? "").lines.map(\.text)
            return !lines.isEmpty && contains(run: lines, in: file, range: file.wholeFile().body)
        case .append:
            guard let section = resolveSection(write.heading, in: file) else { return false }
            return contains(run: write.lines, in: file, range: section.body)
        case .replaceLine:
            guard let section = resolveSection(write.heading, in: file) else { return false }
            let newLine = (write.newLine ?? "").trimmingTrailingWhitespace()
            // The contract words this as "a body line's hash equals the new
            // line's", which would make every tick and retime a no-op: the
            // hash ignores exactly what those change. The target is compared
            // by text when it still exists, and by hash only once it is gone.
            if let target = targetLine(in: file, section: section, hash: write.lineHash ?? "", ordinal: write.ordinal) {
                let text = file.lines[target].text.trimmingTrailingWhitespace()
                return text == newLine || text == newLine + conflictMarker
            }
            let mask = file.contentMask()
            let newHash = lineHash(write.newLine ?? "")
            return section.body.contains { index in
                guard mask[index] else { return false }
                let text = file.lines[index].text
                return lineHash(text) == newHash || text.trimmingTrailingWhitespace() == newLine + conflictMarker
            }
        case .removeLine:
            guard let section = resolveSection(write.heading, in: file) else { return true }
            return targetLine(in: file, section: section, hash: write.lineHash ?? "", ordinal: 0) == nil
        case .replaceSection:
            guard let section = resolveSection(write.heading, in: file) else { return false }
            if sectionHash(lines: file.lines[section.body].map(\.text)) == sectionHash(lines: write.lines) {
                return true
            }
            guard let first = write.lines.first(where: { !$0.allSatisfy(\.isWhitespace) }) else { return false }
            return section.body.contains { file.lines[$0].text.trimmingTrailingWhitespace() == first.trimmingTrailingWhitespace() + conflictMarker }
        }
    }

    static func headingLine(_ heading: HeadingRef) -> String {
        String(repeating: "#", count: min(max(heading.level, 1), 6)) + " " + heading.text
    }

    private static func resolveSection(_ heading: HeadingRef?, in file: TextFile) -> SectionRange? {
        guard let heading else { return file.wholeFile() }
        guard let found = file.resolve(heading) else { return nil }
        return file.section(of: found)
    }

    /// Among the section's lines with this hash, the one at `ordinal`, or the
    /// first when the ordinal is out of range.
    static func targetLine(in file: TextFile, section: SectionRange, hash: String, ordinal: Int) -> Int? {
        let mask = file.contentMask()
        let candidates = section.body.filter { mask[$0] && lineHash(file.lines[$0].text) == hash && !lineIdentity(file.lines[$0].text).isEmpty }
        guard !candidates.isEmpty else { return nil }
        return candidates.indices.contains(ordinal) ? candidates[ordinal] : candidates[0]
    }

    private static func contains(run: [String], in file: TextFile, range: Range<Int>) -> Bool {
        guard !run.isEmpty else { return true }
        let wanted = run.map { $0.trimmingTrailingWhitespace() }
        let mask = file.contentMask()
        // A line inside a fence or front matter never matches, so a run
        // cannot span one.
        let body: [String?] = range.map { mask[$0] ? file.lines[$0].text.trimmingTrailingWhitespace() : nil }
        guard body.count >= wanted.count else { return false }
        for start in 0...(body.count - wanted.count) where Array(body[start..<(start + wanted.count)]) == wanted {
            return true
        }
        return false
    }

    private static func insert(_ lines: [String], at index: Int, blankLineBefore: Bool, in file: inout TextFile, section: SectionRange?) {
        var lines = lines
        if blankLineBefore, index > 0, index - 1 != section?.headingIndex, !file.lines[index - 1].isBlank {
            lines.insert("", at: 0)
        }
        file.insert(lines, at: index)
    }

    private static func insertAtEnd(of whole: SectionRange, lines: [String], blankLineBefore: Bool, in file: inout TextFile, section: SectionRange?) {
        insert(lines, at: whole.bodyEnd, blankLineBefore: blankLineBefore, in: &file, section: section)
    }

    /// A missing heading goes before the first level-1 heading that is not
    /// the note's title, so `# Daily Closure` and friends stay last.
    private static func addHeading(_ heading: HeadingRef, to file: inout TextFile) -> Int {
        let headings = file.headings()
        let before = headings.dropFirst().first { $0.level == 1 }?.index
        let index = before ?? file.lines.count
        var lines: [String] = []
        if index > 0, !file.lines[index - 1].isBlank {
            lines.append("")
        }
        lines.append(headingLine(heading))
        if before != nil {
            lines.append("")
        }
        file.insert(lines, at: index)
        return index + (lines.first == "" ? 1 : 0)
    }

    private static func normalisingLineEndings(_ text: String) -> String {
        text.replacingOccurrences(of: "\r\n", with: "\n")
    }
}
