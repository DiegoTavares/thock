import Foundation

/// The practice notebook: a small, realistic vault dated around today, so the
/// app can be tried, and tested, with no desk and no server.
public enum SampleVault {
    public struct Seed: Sendable {
        public var files: [String: String]
        /// Captures "this phone made" before, so the feed opens with receipts.
        public var captures: [CaptureRecord]
    }

    static let config = """
    schema = 1

    [daily]
    dir      = "daily"
    filename = "YYYY-MM-DD"
    template = "templates/daily.md"

    [weekly]
    dir      = "weekly"
    filename = "GGGG-[W]WW"
    template = "templates/weekly.md"

    [backlog]
    file = "backlog.md"

    [[routines.installed]]
    id      = "timeline"
    enabled = true
    version = 13

    [[routines.installed]]
    id      = "inbox"
    enabled = true
    version = 2

    """

    public static func make(today: VaultDay = .today()) -> Seed {
        let config = VaultConfig(config: Self.config)
        let yesterday = today.adding(days: -1)
        let twoDaysAgo = today.adding(days: -2)
        var files: [String: String] = [
            VaultConfig.configPath: Self.config,
            config.daily.template: Template.defaultDaily,
            config.weekly.template: Template.defaultWeekly,
        ]

        files[config.dailyPath(today)] = """
        # \(today.formatted("dddd, MMMM D, YYYY"))

        ___

        ## Journal

        _What happened, what you noticed, how it went._

        Slept badly, but the morning walk fixed it 🌤️. Coffee with **Ana**, who is
        thinking about changing jobs. I said I'd send her that article.

        **13:02** · Noticed I keep saying yes to things on Thursdays. Worth watching.

        ___

        ## Day planner

        _Timed lines land on the planner beside you, like `- [ ] 09:00 - 10:00 Deep work`._

        - [x] 08:00 - 08:30 Morning walk 🚶
        - [ ] 09:30 - 11:00 Deep work: the budget spreadsheet 📊
        - [ ] 15:00 - 15:30 Call the dentist ☎️
        - [ ] Buy a birthday card for Dad 🎂
        - [ ] Read 20 pages 📖

        ### Calendar

        - [x] 10:00 - 10:30 Standup <!--gcal:9f2c1ab4e7d0-->
        - [ ] 12:30 - 13:30 Lunch with Ana 🥗 <!--gcal:5b7e3c9a1f24-->

        ___

        ## Personal

        _The people, the errands, the small good things._

        - Groceries: eggs, spinach, lemons 🍋
        - Idea 💡: a weekly no-plans Sunday
        - Grateful for: the neighbour who watered the plants

        > "The days are long, but the years are short."

        """

        files[config.dailyPath(yesterday)] = """
        # \(yesterday.formatted("dddd, MMMM D, YYYY"))

        ___

        ## Journal

        A long day that ended well. The budget review ran over, but we left with
        a number everyone could live with.

        **19:40** · Dinner on the balcony. First evening this week without a screen.

        ___

        ## Day planner

        - [x] 09:00 - 10:30 Budget review
        - [x] 11:00 Reply to the landlord
        - [x] Pick up the dry cleaning
        - [ ] ~~Gym~~

        ___

        ## Personal

        | Habit | Done |
        | --- | --- |
        | Walk | yes |
        | Read | 12 pages |

        - Call Mum on Sunday

        # Daily Closure

        You closed three of four planned lines and skipped the gym. The budget
        review is done, which was the week's first goal. Two evenings without a
        screen so far; one more meets the goal you set on Monday.

        """

        files[config.dailyPath(twoDaysAgo)] = """
        # \(twoDaysAgo.formatted("dddd, MMMM D, YYYY"))

        ___

        ## Journal

        Quiet day. Wrote the first half of the proposal before lunch.

        ___

        ## Day planner

        - [x] 09:00 - 12:00 Proposal, first half
        - [x] 14:00 - 14:30 Call the bank

        """

        files[config.weeklyPath(today)] = """
        # Week \(today.isoWeek), \(today.isoWeekYear)

        ___

        ## Goals

        - [x] Finish the budget spreadsheet
        - [ ] Send Ana the article
        - [ ] Two no-screen evenings

        ___

        ## Notes

        A lighter week on purpose. Thursday is the only full day.

        ___

        ## Week review

        _How did it go? The **Week Review** ritual appends its take below yours._

        """

        files[config.backlogFile] = """
        # Backlog

        <!-- Soon = tasks for the coming days. Someday = worth keeping, no commitment.
             Checking a task off in the Backlog panel records it in today's note and
             files it here under Completed with the date. -->

        ## Soon

        - [ ] Renew the passport
        - [ ] Dentist, call back about Friday

        ### Home

        - [ ] Fix the balcony light
        - [ ] Find a plumber for the kitchen tap

        ## Someday

        - [ ] Learn to make sourdough
        - [ ] A long weekend in Lisbon

        ## Completed

        - [x] Book the car service ✅ \(twoDaysAgo.iso)

        """

        let stamp = today.iso
        files["inbox/\(stamp)-0912-ask-ana-for-the-article-on-burnout.md"] = """
        ---
        source:   thock-ios
        capture:  7c1e04b9a2d3
        captured: \(stamp)T09:12:44Z
        title:    Ask Ana for the article on burnout
        ---

        # Ask Ana for the article on burnout

        She mentioned it at coffee. Something about rest being a skill.

        """
        files["inbox/\(stamp)-0731-ship-it-a-practical-guide.md"] = """
        ---
        source:   google-tasks
        capture:  4d1f9a02c7b3
        captured: \(stamp)T07:31:07Z
        title:    Ship It: a practical guide
        url:      https://example.com/essays/ship-it
        ---

        # Ship It: a practical guide

        https://example.com/essays/ship-it

        """

        files[VaultConfig.triageLogPath] = """
        # Triage log

        - \(twoDaysAgo.iso) · Housel on tail events → reference/clips <!--inbox:1b9d6f3e8a70-->
        - \(yesterday.iso) · Birthday card for Dad → Today's note <!--inbox:e02c7a4416bd-->
        - \(yesterday.iso) · Dentist, call back about Friday → Backlog · Soon <!--inbox:93af5d1c2b08-->

        """

        files["reference/clips/housel-on-tail-events.md"] = """
        ---
        source: clip
        url: https://example.com/tail-events
        title: Housel on tail events
        clipped: \(twoDaysAgo.iso)T18:40:00Z
        ---

        # Housel on tail events

        A small number of events account for most outcomes. The practical
        consequence is patience: most of what you try will not matter, and that
        is fine, because a few things will matter enormously.

        """

        files["routines/inbox/triage-policy.md"] = """
        # Triage policy

        | If it looks like… | Propose |
        | --- | --- |
        | A link or article to read | Backlog · Someday, as a task carrying the link |
        | A raw idea or thought | Backlog · Someday |
        | Something clearly urgent | Backlog · Soon |
        | A clip from the phone (a link note pointing at `reference/clips/`) | Backlog · Someday as a task carrying the wikilink; the clip itself stays where it is |

        """

        files["welcome.md"] = """
        # Welcome to Thock

        This folder is your **vault**: your notes, as plain files that belong to you.

        """

        func captured(_ digest: String, _ title: String, _ kind: CaptureKind, daysAgo: Int, hour: Int, minute: Int, path: String?) -> CaptureRecord {
            let day = today.adding(days: -daysAgo)
            var calendar = Calendar(identifier: .gregorian)
            calendar.timeZone = .current
            let date = calendar.date(from: DateComponents(year: day.year, month: day.month, day: day.day, hour: hour, minute: minute)) ?? Date()
            return CaptureRecord(digest: digest, title: title, kind: kind, destination: .inbox, madeAt: date, inboxPath: path)
        }
        let captures = [
            captured("7c1e04b9a2d3", "Ask Ana for the article on burnout", .idea, daysAgo: 0, hour: 9, minute: 12, path: "inbox/\(stamp)-0912-ask-ana-for-the-article-on-burnout.md"),
            captured("93af5d1c2b08", "Dentist, call back about Friday", .idea, daysAgo: 1, hour: 16, minute: 5, path: "inbox/\(yesterday.iso)-1605-dentist-call-back-about-friday.md"),
            captured("e02c7a4416bd", "Birthday card for Dad", .idea, daysAgo: 1, hour: 8, minute: 50, path: "inbox/\(yesterday.iso)-0850-birthday-card-for-dad.md"),
            captured("1b9d6f3e8a70", "Housel on tail events", .link, daysAgo: 2, hour: 18, minute: 40, path: "inbox/\(twoDaysAgo.iso)-1840-housel-on-tail-events.md"),
        ]
        return Seed(files: files, captures: captures)
    }
}

/// The practice notebook's server and desk, saved to one file so a relaunch
/// finds them as it left them.
public final class LocalWorld: @unchecked Sendable {
    struct Saved: Codable {
        var backend: LocalBackend.State
        var desk: SimulatedDesk.State
    }

    public let backend: LocalBackend
    public let desk: SimulatedDesk
    public let isNew: Bool
    private let url: URL?
    private let queue = DispatchQueue(label: "app.thock.local-world")
    private var saved: Saved

    public init(url: URL?, today: VaultDay = .today()) {
        self.url = url
        if let url, let data = try? Data(contentsOf: url), let saved = try? JSONDecoder().decode(Saved.self, from: data) {
            self.saved = saved
            isNew = false
        } else {
            var key = Data(count: 32)
            for index in 0..<32 {
                key[index] = UInt8.random(in: 0...255)
            }
            saved = Saved(backend: LocalBackend.State(), desk: SimulatedDesk.State(disk: SampleVault.make(today: today).files, key: key))
            isNew = true
        }
        backend = LocalBackend(state: saved.backend)
        desk = SimulatedDesk(backend: backend, state: saved.desk)
    }

    public func start() async throws {
        await backend.observe { [weak self] state in
            self?.save { $0.backend = state }
        }
        await desk.observe { [weak self] state in
            self?.save { $0.desk = state }
        }
        try await desk.open()
    }

    private func save(_ change: @escaping (inout Saved) -> Void) {
        queue.async { [weak self] in
            guard let self else { return }
            change(&self.saved)
            guard let url = self.url, let data = try? JSONEncoder().encode(self.saved) else { return }
            try? data.write(to: url, options: .atomic)
        }
    }

    public func reset() {
        if let url {
            try? FileManager.default.removeItem(at: url)
        }
    }
}
