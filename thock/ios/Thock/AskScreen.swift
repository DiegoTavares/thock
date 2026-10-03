import SwiftUI
import ThockKit

/// Ask (V35 §5.5): a question about the vault, answered from the vault by
/// the same agent the desk has. The day's thread, the notes each answer was
/// read from, and one composer.
struct AskScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase

    @State private var draft = ""
    @FocusState private var composing: Bool

    /// Why asking is not on offer here, when it is not.
    private var unavailable: String? {
        if model.isPractice {
            return "The practice notebook has no agent behind it. Once this phone is connected to your desk, this is where the Thock Agent answers from your own notes."
        }
        if model.isReadOnly {
            return "Thock Plus has ended, so the agent is resting. Renew at your desk and it answers here again."
        }
        return nil
    }

    var body: some View {
        VStack(spacing: 0) {
            SheetHeader(leading: "", title: "Ask", trailing: "Done", onLeading: {}, onTrailing: { dismiss() })
                .padding(.horizontal, 20)
                .padding(.bottom, 12)

            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        if model.askTurns.isEmpty {
                            opening
                        }
                        ForEach(model.askTurns) { turn in
                            AskTurnView(turn: turn)
                        }
                        if model.askRunningLow {
                            Text("Most of this cycle's Thock Plus allowance is used.")
                                .font(.system(size: 13))
                                .foregroundStyle(Theme.dim)
                        }
                        Color.clear.frame(height: 1).id("end")
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 20)
                    .padding(.bottom, 16)
                }
                .scrollDismissesKeyboard(.interactively)
                .onChange(of: model.askTurns) {
                    withAnimation { proxy.scrollTo("end", anchor: .bottom) }
                }
                .onChange(of: model.askActivity) {
                    proxy.scrollTo("end", anchor: .bottom)
                }
                .onAppear { proxy.scrollTo("end", anchor: .bottom) }
            }

            if unavailable == nil {
                composer
            }
        }
        .onAppear {
            model.loadAskThread()
            composing = unavailable == nil
        }
        .onChange(of: scenePhase) { _, phase in
            // The thread is drawn from the vault, so it leaves the screen
            // with the app; a turn still running finishes without it.
            if phase == .background { dismiss() }
        }
    }

    private var opening: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("ASK · READS YOUR NOTES")
                .font(Theme.label())
                .tracking(0.9)
                .foregroundStyle(Theme.dim)
            Text("Thock Agent")
                .font(Theme.serif(28, style: .title1))
                .foregroundStyle(Theme.ink)
                .padding(.bottom, 12)
            AgentBlock {
                Text(unavailable ?? "Ask about anything you've written: when something happened, who was on it, what helped last time. I answer from your notes and name the ones I read.")
                    .font(.system(size: 17))
                    .foregroundStyle(Theme.ink)
                    .lineSpacing(4)
            }
        }
    }

    private var composer: some View {
        HStack(alignment: .bottom, spacing: 10) {
            TextField("Ask about your notes…", text: $draft, axis: .vertical)
                .lineLimit(1...4)
                .font(.system(size: 17))
                .foregroundStyle(Theme.ink)
                .focused($composing)
                .submitLabel(.send)
                .onSubmit(send)
                // A vertical field takes return as a new line; one that ends
                // the text is the send key.
                .onChange(of: draft) { _, text in
                    if text.hasSuffix("\n") {
                        draft = String(text.dropLast())
                        send()
                    }
                }
                .padding(.vertical, 10)
                .padding(.horizontal, 14)
                .background(Theme.sunken, in: RoundedRectangle(cornerRadius: 20))
                .overlay(RoundedRectangle(cornerRadius: 20).stroke(Theme.rule, lineWidth: 1))
            if model.askingTurn != nil {
                Button(action: model.stopAsking) {
                    Image(systemName: "stop.circle.fill")
                        .font(.system(size: 34))
                        .foregroundStyle(Theme.muted)
                }
                .accessibilityLabel("Stop")
            } else {
                Button(action: send) {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.system(size: 34))
                        .foregroundStyle(canSend ? Theme.amber : Theme.dim)
                }
                .disabled(!canSend)
                .accessibilityLabel("Ask")
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(Theme.surface)
        .overlay(alignment: .top) { Rectangle().fill(Theme.ruleSoft).frame(height: 1) }
    }

    private var canSend: Bool {
        model.askingTurn == nil && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func send() {
        guard canSend else { return }
        model.ask(draft)
        draft = ""
    }
}

/// The agent's voice: amber at the edge, as on the desk's chat panel.
struct AgentBlock<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        content
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 12)
            .padding(.horizontal, 14)
            .background(Theme.amberSoft)
            .overlay(alignment: .leading) { Rectangle().fill(Theme.amber).frame(width: 2) }
            .clipShape(UnevenRoundedRectangle(bottomTrailingRadius: 10, topTrailingRadius: 10))
    }
}

struct AskTurnView: View {
    @Environment(AppModel.self) private var model
    var turn: AskTurn

    private var isRunning: Bool { model.askingTurn == turn.id }

    /// The answer with its bold, italics and note names drawn, and its line
    /// breaks kept.
    private var answer: AttributedString {
        let text = turn.answer ?? ""
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(turn.question)
                .font(.system(size: 17))
                .foregroundStyle(Theme.ink)
                .padding(.vertical, 10)
                .padding(.horizontal, 14)
                .background(Theme.sunken, in: RoundedRectangle(cornerRadius: 18))
                .frame(maxWidth: .infinity, alignment: .trailing)
                .padding(.leading, 44)

            if turn.answer != nil {
                AgentBlock {
                    Text(answer)
                        .font(.system(size: 17))
                        .foregroundStyle(Theme.ink)
                        .lineSpacing(4)
                        .textSelection(.enabled)
                }
                if !turn.sources.isEmpty {
                    VStack(alignment: .leading, spacing: 2) {
                        ForEach(turn.sources, id: \.self) { path in
                            Text(path)
                                .font(Theme.mono(12))
                                .foregroundStyle(Theme.dim)
                        }
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel("Read from " + turn.sources.joined(separator: ", "))
                }
                if turn.kept {
                    Text("Kept in today's note")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.dim)
                } else if !model.isReadOnly {
                    Button("Keep this") { model.keep(turn) }
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(Theme.amber)
                }
            } else if isRunning {
                HStack(spacing: 8) {
                    ProgressView()
                        .controlSize(.small)
                        .tint(Theme.amber)
                    Text(model.askActivity)
                        .font(.system(size: 14))
                        .foregroundStyle(Theme.muted)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                .accessibilityElement(children: .combine)
            } else {
                // A turn with neither an answer nor a reason was cut off by
                // the app closing under it.
                Text(turn.failure ?? "This one didn't finish.")
                    .font(.system(size: 15))
                    .foregroundStyle(Theme.muted)
                Button("Try again") { model.askAgain(turn) }
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Theme.amber)
                    .disabled(model.askingTurn != nil)
            }
        }
    }
}
