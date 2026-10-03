import SwiftUI
import ThockKit
import UIKit
import UniformTypeIdentifiers

/// The Share Sheet entry (V33 §10): the clip is saved without Thock ever
/// opening. It lands in the shared store and is sent on when the app next runs.
final class ShareViewController: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        Theme.registerFonts()
        view.backgroundColor = Theme.surfaceUI
        overrideUserInterfaceStyle = UserDefaults(suiteName: ThockEnvironment.appGroup)?.string(forKey: "appearance") == "light" ? .light : .dark
        Task { @MainActor in
            let draft = await loadDraft()
            present(draft)
        }
    }

    private func present(_ draft: ClipDraft) {
        let store = try? ThockEnvironment.openStore()
        let content: AnyView
        if let store, store.isConnected, !store.isReadOnly, !draft.url.isEmpty {
            content = AnyView(ClipSheet(draft: draft) { [weak self] in
                self?.finish()
            } onSave: { [weak self] clip in
                do {
                    try VaultSession(store: store).clip(clip)
                    UINotificationFeedbackGenerator().notificationOccurred(.success)
                    self?.finish()
                } catch {
                    self?.show(message: "That didn't save. Try again.")
                }
            })
        } else if draft.url.isEmpty {
            content = AnyView(ShareNotice(text: "There is no link here to keep.") { [weak self] in self?.finish() })
        } else if store?.isReadOnly == true {
            content = AnyView(ShareNotice(text: "This phone is read-only until Thock Plus is renewed.") { [weak self] in self?.finish() })
        } else {
            content = AnyView(ShareNotice(text: "Open Thock once and connect it to your desk. After that, anything you share lands in your inbox.") { [weak self] in self?.finish() })
        }
        let host = UIHostingController(rootView: content.tint(Theme.amber))
        host.view.backgroundColor = .clear
        addChild(host)
        host.view.frame = view.bounds
        host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.addSubview(host.view)
        host.didMove(toParent: self)
    }

    private func show(message: String) {
        let alert = UIAlertController(title: nil, message: message, preferredStyle: .alert)
        alert.addAction(UIAlertAction(title: "OK", style: .default))
        present(alert, animated: true)
    }

    private func finish() {
        extensionContext?.completeRequest(returningItems: nil)
    }

    /// Safari hands over the page through `ClipPage.js`; every other app
    /// hands over a link, sometimes with a line of text.
    private func loadDraft() async -> ClipDraft {
        var draft = ClipDraft()
        let items = extensionContext?.inputItems.compactMap { $0 as? NSExtensionItem } ?? []
        for item in items {
            if draft.title.isEmpty, let title = item.attributedTitle?.string ?? item.attributedContentText?.string, !title.hasPrefix("http") {
                draft.title = String(title.prefix(200)).trimmingCharacters(in: .whitespacesAndNewlines)
            }
            for provider in item.attachments ?? [] {
                if provider.hasItemConformingToTypeIdentifier(UTType.propertyList.identifier),
                   let dictionary = try? await provider.loadItem(forTypeIdentifier: UTType.propertyList.identifier) as? [String: Any],
                   let page = dictionary[NSExtensionJavaScriptPreprocessingResultsKey] as? [String: Any]
                {
                    draft.url = page["url"] as? String ?? draft.url
                    draft.title = page["title"] as? String ?? draft.title
                    draft.html = page["html"] as? String
                    if let selection = (page["selection"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines), !selection.isEmpty {
                        draft.quote = selection
                    }
                } else if draft.url.isEmpty, provider.hasItemConformingToTypeIdentifier(UTType.url.identifier),
                          let url = try? await provider.loadItem(forTypeIdentifier: UTType.url.identifier) as? URL
                {
                    draft.url = url.absoluteString
                } else if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier),
                          let text = try? await provider.loadItem(forTypeIdentifier: UTType.plainText.identifier) as? String
                {
                    let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
                    if draft.url.isEmpty, trimmed.hasPrefix("http"), !trimmed.contains(" ") {
                        draft.url = trimmed
                    } else if draft.quote == nil, !trimmed.isEmpty {
                        draft.quote = String(trimmed.prefix(1200))
                    }
                }
            }
        }
        return draft
    }
}

private struct ShareNotice: View {
    var text: String
    var onDone: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            SheetHeader(leading: "", title: "Save to Thock", trailing: "Done", onLeading: {}, onTrailing: onDone)
            Text(text)
                .font(.system(size: 17))
                .foregroundStyle(Theme.ink)
                .lineSpacing(4)
            Spacer()
        }
        .padding(.horizontal, 20)
    }
}
