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
        if write.kind == .moveBlock || write.kind == .removeBlock {
            return applyBlock(write, to: &file, created: created)
        }

        var sectionAdded = false
        var keptBoth = false
        let section: SectionRange
        if let reference = write.heading {
            if let found = file.resolve(reference) {
                section = file.section(of: found)
            } else {
                addHeading(reference, to: &file)
                sectionAdded = true
                section = file.resolve(reference).map(file.section(of:)) ?? file.wholeFile()
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
        case .moveBlock, .removeBlock:
            break
        case .replaceSection:
            let current = sectionHash(lines: file.lines[section.body].map(\.text))
            if current == write.baseHash {
                file.replace(section.body, with: write.lines)
            } else if !write.lines.allSatisfy({ $0.trimmingCharacters(in: .whitespaces).isEmpty }) {
                var lines = write.lines
                let marked = lines.firstIndex { !$0.allSatisfy(\.isWhitespace) } ?? 0
                lines[marked] += conflictMarker
                insert(lines, at: section.bodyEnd, blankLineBefore: true, in: &file, section: section)
                keptBoth = true
            } else if !created, !sectionAdded {
                // A stale replacement with nothing in it has nothing to keep.
                return Applied(text: file.text, outcome: .noop)
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
        case .moveBlock:
            guard let destination = findGroup(write.to, under: write.createUnder, in: file).map(own) else { return false }
            let wanted = write.lineHash ?? ""
            let landed = write.newLine.map(lineHash) ?? wanted
            let mask = file.contentMask()
            let placed = destination.body.contains { index in
                guard mask[index], lineHash(file.lines[index].text) == landed, !lineIdentity(file.lines[index].text).isEmpty else { return false }
                switch write.place {
                case .top:
                    return index == firstBodyLine(of: destination, in: file)
                case .end:
                    return blockEnd(from: index, in: destination, file: file) == destination.bodyEnd
                case .after(let anchorHash, let anchorOrdinal):
                    // A missing anchor put the block at the end (§7.2), so
                    // that is where a retry looks for it.
                    if let anchor = targetLine(in: file, section: destination, hash: anchorHash, ordinal: anchorOrdinal) {
                        return blockEnd(from: anchor, in: destination, file: file) == index
                    }
                    return blockEnd(from: index, in: destination, file: file) == destination.bodyEnd
                }
            }
            guard placed else { return false }
            // Moved between groups, the source must have let go of it too;
            // reordered inside one group, the placed line is the source.
            guard let source = resolveSection(write.heading, in: file).map(own), source.headingIndex != destination.headingIndex else { return true }
            return targetLine(in: file, section: source, hash: wanted, ordinal: 0) == nil
        case .removeBlock:
            guard let section = resolveSection(write.heading, in: file).map(own) else { return true }
            return targetLine(in: file, section: section, hash: write.lineHash ?? "", ordinal: 0) == nil
        }
    }

    /// `move_block` and `remove_block` (V38 §7): a block is the line plus its
    /// indented continuation, found by hash inside its group's own lines.
    private static func applyBlock(_ write: WriteDocument, to file: inout TextFile, created: Bool) -> Applied {
        // A missing source is a task the desk renamed, moved or completed
        // meanwhile; a move must never become a copy, so nothing happens.
        let untouched = Applied(text: file.text, outcome: created ? .created : .noop)
        guard let source = resolveSection(write.heading, in: file).map(own),
              let index = targetLine(in: file, section: source, hash: write.lineHash ?? "", ordinal: write.ordinal)
        else { return untouched }
        let end = blockEnd(from: index, in: source, file: file)
        var texts = file.lines[index..<end].map(\.text)
        if write.kind == .removeBlock {
            cut(index..<end, from: &file)
            return Applied(text: file.text, outcome: created ? .created : .applied)
        }
        if let newLine = write.newLine, !texts.isEmpty {
            texts[0] = newLine
        }
        cut(index..<end, from: &file)
        var sectionAdded = false
        let destination = own(locateGroup(write.to, under: write.createUnder, in: &file, sectionAdded: &sectionAdded))
        let at: Int
        switch write.place {
        case .top:
            at = firstBodyLine(of: destination, in: file)
        case .end:
            at = destination.bodyEnd
        case .after(let anchorHash, let anchorOrdinal):
            if let anchor = targetLine(in: file, section: destination, hash: anchorHash, ordinal: anchorOrdinal) {
                at = blockEnd(from: anchor, in: destination, file: file)
            } else {
                at = destination.bodyEnd
            }
        }
        insert(texts, at: at, blankLineBefore: false, in: &file, section: destination)
        return Applied(text: file.text, outcome: created ? .created : (sectionAdded ? .sectionAdded : .applied))
    }

    /// The section's own lines as a section of their own, so a block rule
    /// never reaches into a subsection: a group is the lines under its
    /// heading above the next heading (V38 §7.1).
    private static func own(_ section: SectionRange) -> SectionRange {
        var own = section
        own.bodyEnd = section.ownEnd
        return own
    }

    /// The first non-blank line of a section, or its end when it has none:
    /// where `place: top` lands.
    private static func firstBodyLine(of section: SectionRange, in file: TextFile) -> Int {
        section.body.first { !file.lines[$0].isBlank } ?? section.bodyEnd
    }

    /// One past the last line of the block starting at `index`: the line plus
    /// every indented, non-blank line after it inside the section, with blank
    /// lines between them included and trailing blank lines left out. The
    /// desk's own span rule, so both ends cut the same block.
    static func blockEnd(from index: Int, in section: SectionRange, file: TextFile) -> Int {
        var end = index + 1
        var lastContent = end
        while end < section.bodyEnd {
            let line = file.lines[end]
            if line.isBlank {
                end += 1
            } else if line.text.hasPrefix(" ") || line.text.hasPrefix("\t") {
                end += 1
                lastContent = end
            } else {
                break
            }
        }
        return lastContent
    }

    /// Removes a run of lines. A blank line on each side of the gap would
    /// leave two in a row, which neither end ever writes, so one goes with
    /// the block.
    private static func cut(_ range: Range<Int>, from file: inout TextFile) {
        let range = range.lowerBound..<min(range.upperBound, file.lines.count)
        guard !range.isEmpty else { return }
        file.lines.removeSubrange(range)
        let start = range.lowerBound
        if start > 0, start < file.lines.count, file.lines[start - 1].isBlank, file.lines[start].isBlank {
            file.lines.remove(at: start)
        }
    }

    /// The group a block moves into. A missing `to` is created at the end of
    /// `createUnder` (itself created like any missing heading), or, with no
    /// parent named, where an append would create it. `Someday › Home` is
    /// the Home inside Someday, whatever other Home the note has.
    private static func locateGroup(_ to: HeadingRef?, under parent: HeadingRef?, in file: inout TextFile, sectionAdded: inout Bool) -> SectionRange {
        guard let to else { return file.wholeFile() }
        guard let parent else {
            if let found = file.resolve(to) { return file.section(of: found) }
            addHeading(to, to: &file)
            sectionAdded = true
            return file.resolve(to).map(file.section(of:)) ?? file.wholeFile()
        }
        let parentSection: SectionRange
        if let found = file.resolve(parent) {
            parentSection = file.section(of: found)
        } else {
            addHeading(parent, to: &file)
            sectionAdded = true
            parentSection = file.resolve(parent).map(file.section(of:)) ?? file.wholeFile()
        }
        if let found = file.resolve(to, within: parentSection.start..<parentSection.end) {
            return file.section(of: found)
        }
        insert([headingLine(to)], at: parentSection.bodyEnd, blankLineBefore: true, in: &file, section: parentSection)
        sectionAdded = true
        return file.resolve(to, within: parentSection.start..<file.lines.count).map(file.section(of:)) ?? file.wholeFile()
    }

    /// The group a `move_block` names as its destination, when the note has
    /// it: `to` inside `createUnder`'s section when a parent is named, else
    /// `to` anywhere.
    private static func findGroup(_ to: HeadingRef?, under parent: HeadingRef?, in file: TextFile) -> SectionRange? {
        guard let to else { return file.wholeFile() }
        if let parent {
            guard let parentSection = resolveSection(parent, in: file), let found = file.resolve(to, within: parentSection.start..<parentSection.end) else { return nil }
            return file.section(of: found)
        }
        return file.resolve(to).map(file.section(of:))
    }

    static func headingLine(_ heading: HeadingRef) -> String {
        String(repeating: "#", count: min(max(heading.level, 1), 6)) + " " + heading.text.trimmingCharacters(in: .whitespacesAndNewlines)
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
    @discardableResult
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
