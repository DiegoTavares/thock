import SwiftUI
import ThockKit

/// Confirm (V48 §8.2): the words, the four chips, the phone's hint, Save.
/// Leaving without saving keeps the draft; Cancel drops it.
struct CaptureScreen: View {
    @Environment(WatchModel.self) private var model
    @State private var choosingTime = false

    private let columns = [GridItem(.flexible(), spacing: 6), GridItem(.flexible(), spacing: 6)]

    var body: some View {
        NavigationStack {
            Group {
                if let saved = model.saved {
                    SavedView(saved: saved)
                } else if let blocked = model.writeBlocked {
                    Text(blocked)
                        .font(.footnote)
                        .foregroundStyle(WatchTheme.muted)
                        .multilineTextAlignment(.center)
                        .padding()
                } else if let draft = model.draft {
                    confirm(draft)
                } else {
                    listening
                }
            }
            .navigationDestination(isPresented: $choosingTime) {
                RemindMeScreen { due in
                    model.pick(due)
                    choosingTime = false
                }
                .onDisappear { model.closedRemindMe() }
            }
        }
    }

    /// Opened from the mic or with dictation cancelled: one tap to speak.
    private var listening: some View {
        VStack(spacing: 10) {
            Button {
                model.dictate { text in
                    guard let text else { return }
                    model.startDraft(text)
                }
            } label: {
                Image(systemName: "mic.fill")
                    .font(.system(size: 28))
                    .foregroundStyle(WatchTheme.amberInk)
                    .frame(width: 64, height: 64)
                    .background(Circle().fill(WatchTheme.amber))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Speak")
            Text("What's on your mind?")
                .font(.footnote)
                .foregroundStyle(WatchTheme.muted)
        }
    }

    private func confirm(_ draft: WatchOutbox.Draft) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 8) {
                Button {
                    model.redictate()
                } label: {
                    Text(draft.text)
                        .font(.body)
                        .foregroundStyle(WatchTheme.ink)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                        .background(RoundedRectangle(cornerRadius: 10).fill(WatchTheme.surface))
                }
                .buttonStyle(.plain)
                .accessibilityHint("Dictate again")

                LazyVGrid(columns: columns, spacing: 6) {
                    ForEach(CaptureDestination.allCases, id: \.self) { chip in
                        chipButton(chip, draft: draft)
                    }
                }

                Text(model.hint(for: draft))
                    .font(.caption2)
                    .foregroundStyle(WatchTheme.muted)
                    .fixedSize(horizontal: false, vertical: true)

                Button {
                    model.save()
                } label: {
                    Text("Save")
                        .font(.headline)
                        .frame(maxWidth: .infinity)
                }
                .tint(WatchTheme.amber)
                .disabled(draft.chip == .reminder && draft.reminderDue == nil)

                Button("Cancel", role: .destructive) {
                    model.cancelCapture()
                }
                .font(.footnote)
                .buttonStyle(.plain)
                .foregroundStyle(WatchTheme.dim)
                .frame(maxWidth: .infinity)
            }
        }
    }

    private func chipButton(_ chip: CaptureDestination, draft: WatchOutbox.Draft) -> some View {
        let selected = draft.chip == chip
        let title = chip == .reminder ? (draft.reminderDue?.chipText(today: model.today) ?? chip.title) : chip.title
        return Button {
            model.choose(chip)
            if chip == .reminder {
                choosingTime = true
            }
        } label: {
            Text(title)
                .font(.system(size: 13, weight: .medium))
                .lineLimit(1)
                .minimumScaleFactor(0.7)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 7)
                .foregroundStyle(selected ? WatchTheme.amberInk : WatchTheme.ink)
                .background(Capsule().fill(selected ? WatchTheme.amber : WatchTheme.surface))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// The phone's toast, then *Sending to your iPhone* until the phone answers.
struct SavedView: View {
    @Environment(WatchModel.self) private var model
    var saved: WatchModel.Saved

    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 30))
                .foregroundStyle(WatchTheme.good)
            Text(saved.toast)
                .font(.headline)
                .foregroundStyle(WatchTheme.ink)
                .multilineTextAlignment(.center)
            Text(status)
                .font(.footnote)
                .foregroundStyle(WatchTheme.muted)
                .multilineTextAlignment(.center)
        }
        .padding()
    }

    private var status: String {
        switch model.state(of: saved.id) {
        case .waiting?: return "Sending to your iPhone"
        case .refused(let message)?: return message
        case .saved?, nil: return "On your iPhone"
        }
    }
}
