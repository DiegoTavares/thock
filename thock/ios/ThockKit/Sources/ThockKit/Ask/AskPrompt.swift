import Foundation

/// The agent's system prompt on the phone (V35 §5.4): the phone's own
/// sections, the desk agent's voice, memory and language sections word for
/// word, what is true right now, and the vault's own instructions.
public enum AskPrompt {
    /// The sections of the desk's `SYSTEM.md` the phone agent shares. A test
    /// keeps the bundled copy equal to the desk's file, so a heading renamed
    /// there fails here instead of silently dropping out of the prompt.
    static let sharedHeadings = ["How you speak", "What you remember", "Language"]
    static let vaultInstructionsPath = "AGENTS.md"
    static let profilePath = "profile.md"
    static let memoryIndexPath = "memory/index.md"
    static let vaultWordsCap = 200

    static func resource(_ name: String) -> String {
        guard let url = Bundle.module.url(forResource: name, withExtension: "md", subdirectory: "Prompts"),
              let text = try? String(contentsOf: url, encoding: .utf8)
        else { return "" }
        return text
    }

    /// One `##` section of a prompt file, heading included.
    static func section(_ heading: String, of text: String) -> String? {
        let lines = text.components(separatedBy: "\n")
        guard let start = lines.firstIndex(of: "## " + heading) else { return nil }
        let end = lines[(start + 1)...].firstIndex { $0.hasPrefix("## ") } ?? lines.count
        return lines[start..<end].joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    static func capped(_ text: String, lines cap: Int, closing: String) -> String {
        let lines = text.trimmingCharacters(in: .whitespacesAndNewlines).components(separatedBy: "\n")
        guard lines.count > cap else { return lines.joined(separator: "\n") }
        return (lines.prefix(cap) + ["", closing]).joined(separator: "\n")
    }

    /// The phone's port of the desk's `compose_vault_context` (V27, V28
    /// decision 11), without the Routines list: no ritual runs here.
    public static func context(session: VaultSession, now: Date = Date()) -> String {
        let config = session.config
        let store = session.store
        let builder = session.writes(now: now)
        let today = builder.today
        var lines = ["# Right now", ""]
        lines.append("Today is \(today.formatted("dddd, D MMMM YYYY")) (\(today.iso)), week \(today.formatted("GGGG-[W]WW")). The time is \(builder.clock).")
        lines.append("You are reading the copy of this person's vault that their phone carries.")
        lines.append("")

        func note(_ label: String, _ path: String) -> String {
            "- \(label): `\(path)`" + (store.exists(path) ? "." : " (not written yet).")
        }
        lines.append(note("Today's note", config.dailyPath(today)))
        lines.append(note("This week's note", config.weeklyPath(today)))
        lines.append("- Daily notes are in `\(config.daily.dir)/` and weekly notes in `\(config.weekly.dir)/`, each named by its date like the two above.")
        lines.append("- Tasks: `\(config.backlogFile)`, under the headings `\(config.soonHeading)`, `\(config.somedayHeading)` and `\(config.completedHeading)`.")
        if let planner = config.plannerHeadings.first {
            lines.append("- The day's plan lives under the `\(planner)` heading of the daily note.")
        }

        lines += ["", "## Language", ""]
        let label: String?
        switch (config.languageName, config.languageTag) {
        case let (name?, tag?): label = "**\(name)** (`\(tag)`)"
        case let (name?, nil): label = "**\(name)**"
        case let (nil, tag?): label = "`\(tag)`"
        case (nil, nil): label = nil
        }
        if let label {
            lines.append("This vault is set to \(label). Speak and write in it, from your first greeting onwards.")
        } else {
            lines.append("No language has been set for this vault, so answer in whatever language the person writes to you in.")
        }

        lines += ["", "## What you already know", ""]
        if let index = store.content(memoryIndexPath), !index.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            lines.append("This is `memory/index.md`, what past sessions learned about this person. Treat it as things you already know. Where a line points at a page under `memory/`, open that page when the conversation touches it. When they tell you something that will still be true next month, or correct you, add one dated line to `memory/inbox.md`; the Reflect ritual files it at the desk.")
            lines.append("")
            lines.append(capped(index, lines: config.memoryIndexLines, closing: "The rest of this page is over its cap; the next Reflect will trim it."))
        } else {
            lines.append("Nothing yet: no session has learned anything about this person, or the `memory/` folder is missing. When they tell you something that will still be true next month, add one dated line to `memory/inbox.md`; the Reflect ritual files it at the desk.")
        }
        return lines.joined(separator: "\n")
    }

    public static func system(session: VaultSession, now: Date = Date()) -> String {
        let desk = resource("SYSTEM")
        var parts = [resource("PHONE").trimmingCharacters(in: .whitespacesAndNewlines)]
        parts += sharedHeadings.compactMap { section($0, of: desk) }
        parts.append(context(session: session, now: now))
        let store = session.store
        if let instructions = store.content(vaultInstructionsPath), !instructions.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            parts.append("""
                ## The vault's own instructions

                This is the vault's `AGENTS.md`, written for the agent at the desk. Use it as the map of how this vault is laid out. Where it describes writing notes or running rituals, that is for the desk; on the phone the rules above win.

                \(capped(instructions, lines: vaultWordsCap, closing: "The rest of `AGENTS.md` is not shown; `read` it if you need it."))
                """)
        }
        if let profile = store.content(profilePath), !profile.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            parts.append("""
                ## Who this person is

                This is `profile.md`, in their own words.

                \(capped(profile, lines: vaultWordsCap, closing: "The rest of `profile.md` is not shown; `read` it if you need it."))
                """)
        }
        return parts.joined(separator: "\n\n")
    }
}
