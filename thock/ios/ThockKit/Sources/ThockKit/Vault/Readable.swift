import Foundation

/// Reader-mode extraction for clips (V33 §10): the readable text of a web
/// page as the editor's Markdown subset, images off. Deliberately simple; a
/// page it cannot make sense of yields nothing and the clip keeps its link.
public enum Readable {
    public struct Page: Equatable, Sendable {
        public var title: String?
        public var markdown: String
    }

    static let dropped: Set<String> = ["script", "style", "noscript", "nav", "header", "footer", "aside", "form", "svg", "iframe", "button", "figure", "template", "select"]
    static let blocks: Set<String> = ["p", "div", "section", "article", "main", "ul", "ol", "table", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "blockquote", "pre", "li", "br", "hr"]

    enum Token {
        case open(String, [String: String])
        case close(String)
        case text(String)
    }

    public static func extract(html: String) -> Page {
        let tokens = tokenize(html)
        let title = self.title(tokens)
        let scoped = scope(tokens, to: "article") ?? scope(tokens, to: "main") ?? scope(tokens, to: "body") ?? tokens
        return Page(title: title, markdown: render(scoped))
    }

    static func tokenize(_ html: String) -> [Token] {
        var tokens: [Token] = []
        let characters = Array(html)
        var index = 0
        var text = ""
        func flush() {
            if !text.isEmpty {
                tokens.append(.text(text))
                text = ""
            }
        }
        while index < characters.count {
            guard characters[index] == "<" else {
                text.append(characters[index])
                index += 1
                continue
            }
            if SyncCore.matches(characters, at: index, "<!--") {
                var end = index + 4
                while end < characters.count, !SyncCore.matches(characters, at: end, "-->") {
                    end += 1
                }
                index = min(end + 3, characters.count)
                continue
            }
            guard let close = characters[index...].firstIndex(of: ">") else {
                text.append(characters[index])
                index += 1
                continue
            }
            let inside = String(characters[(index + 1)..<close])
            index = close + 1
            guard let first = inside.first else { continue }
            guard first.isLetter || first == "/" else {
                // A doctype or processing instruction is dropped; anything
                // else was a literal `<` in the text.
                if first != "!", first != "?" {
                    text.append("<" + inside + ">")
                }
                continue
            }
            flush()
            if first == "/" {
                tokens.append(.close(inside.dropFirst().trimmingCharacters(in: .whitespaces).lowercased()))
                continue
            }
            let name = String(inside.prefix { $0.isLetter || $0.isNumber }).lowercased()
            tokens.append(.open(name, attributes(String(inside.dropFirst(name.count)))))
            // Script and style bodies are not markup; skip to their end tag.
            if name == "script" || name == "style" {
                let closing = Array("</" + name)
                var end = index
                while end < characters.count {
                    if characters[end] == "<", end + closing.count <= characters.count,
                       String(characters[end..<(end + closing.count)]).lowercased() == String(closing)
                    {
                        break
                    }
                    end += 1
                }
                index = end
            }
        }
        flush()
        return tokens
    }

    static func attributes(_ text: String) -> [String: String] {
        var result: [String: String] = [:]
        let characters = Array(text)
        var index = 0
        while index < characters.count {
            while index < characters.count, characters[index].isWhitespace || characters[index] == "/" {
                index += 1
            }
            var name = ""
            while index < characters.count, !characters[index].isWhitespace, characters[index] != "=" {
                name.append(characters[index])
                index += 1
            }
            guard !name.isEmpty else { break }
            while index < characters.count, characters[index].isWhitespace {
                index += 1
            }
            guard index < characters.count, characters[index] == "=" else {
                result[name.lowercased()] = ""
                continue
            }
            index += 1
            while index < characters.count, characters[index].isWhitespace {
                index += 1
            }
            var value = ""
            if index < characters.count, characters[index] == "\"" || characters[index] == "'" {
                let quote = characters[index]
                index += 1
                while index < characters.count, characters[index] != quote {
                    value.append(characters[index])
                    index += 1
                }
                index += 1
            } else {
                while index < characters.count, !characters[index].isWhitespace {
                    value.append(characters[index])
                    index += 1
                }
            }
            result[name.lowercased()] = decodeEntities(value)
        }
        return result
    }

    static func title(_ tokens: [Token]) -> String? {
        var inTitle = false
        var title = ""
        for token in tokens {
            switch token {
            case .open("meta", let attributes) where attributes["property"] == "og:title":
                if let content = attributes["content"], !content.isEmpty {
                    return collapse(content)
                }
            case .open("title", _): inTitle = true
            case .close("title"): inTitle = false
            case .text(let text) where inTitle: title += text
            default: break
            }
        }
        let cleaned = collapse(decodeEntities(title))
        return cleaned.isEmpty ? nil : cleaned
    }

    static func scope(_ tokens: [Token], to tag: String) -> [Token]? {
        guard let start = tokens.firstIndex(where: { if case .open(tag, _) = $0 { return true } else { return false } }) else { return nil }
        var depth = 0
        for index in start..<tokens.count {
            switch tokens[index] {
            case .open(tag, _): depth += 1
            case .close(tag):
                depth -= 1
                if depth == 0 {
                    return Array(tokens[(start + 1)..<index])
                }
            default: break
            }
        }
        return Array(tokens[(start + 1)...])
    }

    static func render(_ tokens: [Token]) -> String {
        var output: [String] = []
        var current = ""
        var skipDepth = 0
        var skipTag = ""
        var prefix = ""
        var quoteDepth = 0
        var inPre = false
        var bold = 0
        var italic = 0

        func flush() {
            let text = inPre ? current : collapse(current)
            current = ""
            guard !text.isEmpty else { return }
            let quote = String(repeating: "> ", count: quoteDepth)
            if inPre {
                output.append("```\n" + text.trimmingCharacters(in: .newlines) + "\n```")
            } else {
                output.append(quote + prefix + text)
            }
        }

        for token in tokens {
            if skipDepth > 0 {
                if case .open(let name, _) = token, name == skipTag { skipDepth += 1 }
                if case .close(let name) = token, name == skipTag { skipDepth -= 1 }
                continue
            }
            switch token {
            case .text(let text):
                current += inPre ? decodeEntities(text) : decodeEntities(text).replacingOccurrences(of: "\n", with: " ")
            case .open(let name, _):
                if dropped.contains(name) {
                    // Void-ish drops would otherwise swallow the rest of the page.
                    if name != "br" {
                        skipDepth = 1
                        skipTag = name
                    }
                    continue
                }
                if name == "img" || name == "input" || name == "link" || name == "meta" { continue }
                if blocks.contains(name) { flush() }
                switch name {
                case "h1", "h2": prefix = "## "
                case "h3", "h4", "h5", "h6": prefix = "### "
                case "li": prefix = "- "
                case "blockquote": quoteDepth += 1
                case "pre": inPre = true
                case "hr": output.append("___")
                case "strong", "b":
                    if bold == 0 { current += "**" }
                    bold += 1
                case "em", "i":
                    if italic == 0 { current += "_" }
                    italic += 1
                default: break
                }
            case .close(let name):
                switch name {
                case "strong", "b":
                    bold = max(bold - 1, 0)
                    if bold == 0 { current = closingMarker(current, "**") }
                case "em", "i":
                    italic = max(italic - 1, 0)
                    if italic == 0 { current = closingMarker(current, "_") }
                default: break
                }
                if blocks.contains(name) {
                    flush()
                    prefix = ""
                }
                if name == "blockquote" { quoteDepth = max(quoteDepth - 1, 0) }
                if name == "pre" { inPre = false }
            }
        }
        flush()
        // Navigation crumbs and bylines are short; an article is not. Without
        // a few real sentences there is nothing worth keeping.
        let words = output.joined(separator: " ").split(separator: " ").count
        guard words >= 40 else { return "" }
        // List items sit on consecutive lines; every other block gets the
        // blank line Markdown needs.
        var text = ""
        for (index, block) in output.enumerated() {
            if index > 0 {
                let bothItems = isListItem(block) && isListItem(output[index - 1])
                text += bothItems ? "\n" : "\n\n"
            }
            text += block
        }
        return text
    }

    private static func isListItem(_ block: String) -> Bool {
        block.hasPrefix("- ") || block.hasPrefix("> - ")
    }

    /// An emphasis marker closes against the word, not the space before it.
    static func closingMarker(_ text: String, _ marker: String) -> String {
        if text.hasSuffix(marker) {
            return String(text.dropLast(marker.count))
        }
        let trailing = String(text.reversed().prefix { $0 == " " })
        return String(text.dropLast(trailing.count)) + marker + trailing
    }

    static func collapse(_ text: String) -> String {
        text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    static let entities: [String: String] = ["amp": "&", "lt": "<", "gt": ">", "quot": "\"", "apos": "'", "nbsp": " ", "mdash": "—", "ndash": "–", "hellip": "…",
                                             "rsquo": "’", "lsquo": "‘", "rdquo": "”", "ldquo": "“", "copy": "©"]

    public static func decodeEntities(_ text: String) -> String {
        guard text.contains("&") else { return text }
        var output = ""
        var rest = Substring(text)
        while let ampersand = rest.firstIndex(of: "&") {
            output += rest[..<ampersand]
            let after = rest[rest.index(after: ampersand)...]
            guard let semicolon = after.firstIndex(of: ";"), after.distance(from: after.startIndex, to: semicolon) <= 8 else {
                output += "&"
                rest = after
                continue
            }
            let name = after[..<semicolon]
            var replacement: String?
            if name.hasPrefix("#x") || name.hasPrefix("#X") {
                replacement = UInt32(name.dropFirst(2), radix: 16).flatMap(Unicode.Scalar.init).map { String(Character($0)) }
            } else if name.hasPrefix("#") {
                replacement = UInt32(name.dropFirst()).flatMap(Unicode.Scalar.init).map { String(Character($0)) }
            } else {
                replacement = entities[String(name)]
            }
            if let replacement {
                output += replacement
                rest = after[after.index(after: semicolon)...]
            } else {
                output += "&"
                rest = after
            }
        }
        output += rest
        return output
    }
}
