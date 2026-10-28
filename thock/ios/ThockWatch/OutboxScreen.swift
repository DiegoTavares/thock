import SwiftUI
import ThockKit

/// What is waiting for the phone (V48 §8.4): each request with its words
/// and its state. A refused one shows the phone's message, with *Try again*
/// and *Delete*; nothing goes without that tap.
struct OutboxScreen: View {
    @Environment(WatchModel.self) private var model

    var body: some View {
        List {
            if model.outbox.unsent.isEmpty {
                Text("Nothing waiting")
                    .font(.footnote)
                    .foregroundStyle(WatchTheme.muted)
                    .listRowBackground(Color.clear)
            }
            ForEach(model.outbox.unsent) { item in
                VStack(alignment: .leading, spacing: 4) {
                    Text(words(item.request))
                        .foregroundStyle(WatchTheme.ink)
                        .lineLimit(3)
                    switch item.state {
                    case .refused(let message):
                        Text(message)
                            .font(.caption2)
                            .foregroundStyle(WatchTheme.warn)
                        HStack {
                            Button("Try again") { model.retry(item.id) }
                                .tint(WatchTheme.amber)
                            Button("Delete", role: .destructive) { model.delete(item.id) }
                        }
                        .font(.footnote)
                    case .waiting, .saved:
                        Text("Waiting for your iPhone")
                            .font(.caption2)
                            .foregroundStyle(WatchTheme.muted)
                        // A request that never arrives is not stuck for good.
                        Button("Delete", role: .destructive) { model.delete(item.id) }
                            .font(.footnote)
                    }
                }
                .padding(.vertical, 2)
            }
        }
        .navigationTitle("Waiting")
    }

    private func words(_ request: WatchOutbox.Request) -> String {
        switch request {
        case .capture(let capture):
            return capture.text
        case .tick(let tick):
            let row = VaultDay(iso: tick.day).flatMap(model.reading)?.rows.first { $0.hash == tick.hash && $0.ordinal == tick.ordinal }
            return "Tick: " + (row?.label ?? tick.day)
        }
    }
}
