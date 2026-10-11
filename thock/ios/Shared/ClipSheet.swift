import SwiftUI
import ThockKit

/// What the share sheet, or a pasted link, hands to the clip sheet.
struct ClipDraft: Equatable {
    var title = ""
    var url = ""
    var quote: String?
    /// The page as the browser had it, when the share came from Safari.
    var html: String?
    /// Pictures shared with the item, already downsized (V39 §7.3).
    var images: [ImageAttachment] = []
}

/// Pictures shared with no link (V39 §7.3): the thumbnails, a line for a
/// title, and Save. The first line of the text is the note's title.
struct PhotoSheet: View {
    var images: [ImageAttachment]
    var onCancel: () -> Void
    var onSave: (String) -> Void

    @State private var text = ""
    @State private var saving = false
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            SheetHeader(leading: "Cancel", title: "Save to Thock", trailing: saving ? "Saving…" : "Save", trailingEnabled: !saving, onLeading: onCancel) {
                guard !saving else { return }
                saving = true
                onSave(text)
            }
            HStack(spacing: 10) {
                ForEach(Array(images.enumerated()), id: \.offset) { _, image in
                    if let thumbnail = UIImage(data: image.bytes) {
                        Image(uiImage: thumbnail)
                            .resizable()
                            .scaledToFill()
                            .frame(width: 64, height: 64)
                            .clipShape(RoundedRectangle(cornerRadius: 12))
                    }
                }
                Spacer(minLength: 0)
            }
            TextField("A title, and a line if you like", text: $text, axis: .vertical)
                .font(.system(size: 18))
                .foregroundStyle(Theme.ink)
                .lineLimit(1...4)
                .focused($focused)
            (Text("\(images.count == 1 ? "The picture lands" : "The pictures land") in your inbox, under ") + Text("images").font(Theme.mono(12)).foregroundStyle(Theme.amber) + Text("."))
                .font(.system(size: 13))
                .foregroundStyle(Theme.dim)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 20)
        .onAppear { focused = true }
    }
}

enum ClipFetcher {
    /// The page's title and readable text. A page that cannot be fetched or
    /// read yields nothing, and the clip keeps its link.
    static func page(for draft: ClipDraft) async -> Readable.Page? {
        if let html = draft.html, !html.isEmpty {
            return Readable.extract(html: html)
        }
        guard let url = URL(string: draft.url), let scheme = url.scheme, scheme.hasPrefix("http") else { return nil }
        var request = URLRequest(url: url, timeoutInterval: 12)
        request.setValue("text/html,application/xhtml+xml", forHTTPHeaderField: "Accept")
        guard let (data, response) = try? await URLSession.shared.data(for: request),
              let status = (response as? HTTPURLResponse)?.statusCode, (200..<300).contains(status),
              data.count < 6_000_000
        else { return nil }
        let html = String(data: data, encoding: .utf8) ?? String(decoding: data, as: UTF8.self)
        return Readable.extract(html: html)
    }
}

/// The clip sheet (V33 §10): the page, one line about why you kept it, and
/// two switches.
struct ClipSheet: View {
    @State var draft: ClipDraft
    var asksForLink = false
    var onCancel: () -> Void
    var onSave: (PhoneWrites.Clip) -> Void

    @State private var why = ""
    @State private var keepText = true
    @State private var makeTask = false
    @State private var saving = false
    @FocusState private var focus: Field?

    enum Field {
        case link
        case why
    }

    private var hasLink: Bool {
        URL(string: draft.url.trimmingCharacters(in: .whitespaces))?.scheme?.hasPrefix("http") ?? false
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            SheetHeader(leading: "Cancel", title: "Save to Thock", trailing: saving ? "Saving…" : "Save", trailingEnabled: hasLink && !saving, onLeading: onCancel, onTrailing: save)

            VStack(alignment: .leading, spacing: 4) {
                if asksForLink {
                    TextField("Paste a link", text: $draft.url)
                        .font(Theme.mono(14))
                        .foregroundStyle(Theme.ink)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .focused($focus, equals: .link)
                } else {
                    Text(draft.title.isEmpty ? draft.url : draft.title)
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(2)
                    Text(draft.url)
                        .font(Theme.mono(12))
                        .foregroundStyle(Theme.dim)
                        .lineLimit(2)
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.ground, in: RoundedRectangle(cornerRadius: 14))

            TextField("Why keep it? One line is plenty.", text: $why, axis: .vertical)
                .font(.system(size: 18))
                .foregroundStyle(Theme.ink)
                .lineLimit(1...4)
                .focused($focus, equals: .why)

            if let quote = draft.quote, !quote.isEmpty {
                Text(quote)
                    .font(.system(size: 15))
                    .foregroundStyle(Theme.muted)
                    .lineLimit(4)
                    .padding(.leading, 12)
                    .overlay(alignment: .leading) { Rectangle().fill(Theme.rule).frame(width: 2) }
            }

            VStack(spacing: 12) {
                Toggle("Keep the article text for later", isOn: $keepText)
                Toggle("Also make a task to read it", isOn: $makeTask)
            }
            .toggleStyle(ThockToggleStyle())
            .font(.system(size: 16))
            .foregroundStyle(Theme.ink)

            (Text("Link lands in your inbox. ") + Text(keepText ? "Text is kept under " : "") + Text(keepText ? "reference/clips" : "").font(Theme.mono(12)).foregroundStyle(Theme.amber) + Text(keepText ? "." : ""))
                .font(.system(size: 13))
                .foregroundStyle(Theme.dim)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 20)
        .onAppear {
            focus = asksForLink && draft.url.isEmpty ? .link : .why
        }
    }

    private func save() {
        guard hasLink, !saving else { return }
        saving = true
        var draft = draft
        draft.url = draft.url.trimmingCharacters(in: .whitespaces)
        Task {
            var page: Readable.Page?
            if keepText || draft.title.isEmpty {
                page = await ClipFetcher.page(for: draft)
            }
            let title = draft.title.isEmpty ? page?.title ?? draft.url : draft.title
            let clip = PhoneWrites.Clip(title: title, url: draft.url, why: why, quote: draft.quote, article: keepText ? page?.markdown : nil, makeTask: makeTask)
            await MainActor.run { onSave(clip) }
        }
    }
}
