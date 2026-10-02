import SwiftUI
import ThockKit
import VisionKit

/// The first screen: connect to the desk by scanning its code, or try the
/// practice notebook that needs no desk at all.
struct WelcomeView: View {
    @Environment(AppModel.self) private var model
    @State private var scanning = false
    @State private var pasting = false
    @State private var pasted = ""

    private var canScan: Bool {
        DataScannerViewController.isSupported && DataScannerViewController.isAvailable
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("THOCK ON IPHONE")
                    .font(Theme.label())
                    .tracking(1.1)
                    .foregroundStyle(Theme.dim)
                    .padding(.top, 48)
                Text("A pen for your vault")
                    .font(Theme.serif(40, style: .largeTitle))
                    .foregroundStyle(Theme.ink)
                Text("The desk is where you plan your days. This catches what happens in between: an idea, a few lines in the journal, an article to keep.")
                    .font(.system(size: 17))
                    .foregroundStyle(Theme.muted)
                    .lineSpacing(4)

                Hairline().padding(.vertical, 6)

                CardLabel(title: "Connect to your desk")
                Text(canScan ? "In Thock on your computer, choose Connect your phone. It shows a code; scan it here."
                    : "In Thock on your computer, choose Connect your phone. This phone has no camera to scan its code with, so paste the link it gives you.")
                    .font(.system(size: 16))
                    .foregroundStyle(Theme.ink)
                    .lineSpacing(3)

                if canScan {
                    primaryButton("Scan the code") { scanning = true }
                    Button("Paste the code instead") { pasting.toggle() }
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(Theme.muted)
                }
                if pasting || !canScan {
                    TextField("thock://pair…", text: $pasted, axis: .vertical)
                        .font(Theme.mono(13))
                        .foregroundStyle(Theme.ink)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .lineLimit(2...4)
                        .padding(12)
                        .background(Theme.surface, in: RoundedRectangle(cornerRadius: 12))
                        .overlay(RoundedRectangle(cornerRadius: 12).stroke(Theme.rule, lineWidth: 1))
                    primaryButton("Connect") {
                        Task { await model.pair(text: pasted) }
                    }
                    .disabled(pasted.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }

                if let error = model.pairingError {
                    Text(error)
                        .font(.system(size: 15))
                        .foregroundStyle(Theme.warn)
                }

                Hairline().padding(.vertical, 6)

                CardLabel(title: "Or just look around")
                Text("The practice notebook is a sample vault that lives only on this phone, with a pretend desk behind it. Nothing leaves the phone.")
                    .font(.system(size: 16))
                    .foregroundStyle(Theme.ink)
                    .lineSpacing(3)
                Button {
                    Task { await model.startPractice() }
                } label: {
                    Text("Open the practice notebook")
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(Theme.ink)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 14)
                        .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.rule, lineWidth: 1))
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal, 24)
            .padding(.bottom, 32)
        }
        .disabled(model.isPairing)
        .overlay {
            if model.isPairing {
                ProgressView().tint(Theme.amber)
            }
        }
        .sheet(isPresented: $scanning) {
            CodeScanner { code in
                scanning = false
                Task { await model.pair(text: code) }
            }
            .ignoresSafeArea()
        }
    }

    private func primaryButton(_ title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(Theme.amberInk)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 14)
                .background(Theme.amber, in: RoundedRectangle(cornerRadius: 14))
        }
        .buttonStyle(.plain)
    }
}

/// Reads the pairing QR with the system scanner.
struct CodeScanner: UIViewControllerRepresentable {
    var onCode: (String) -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(onCode: onCode)
    }

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(recognizedDataTypes: [.barcode(symbologies: [.qr])], isHighlightingEnabled: true)
        scanner.delegate = context.coordinator
        try? scanner.startScanning()
        return scanner
    }

    func updateUIViewController(_ controller: DataScannerViewController, context: Context) {}

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        var onCode: (String) -> Void
        private var done = false

        init(onCode: @escaping (String) -> Void) {
            self.onCode = onCode
        }

        func dataScanner(_ dataScanner: DataScannerViewController, didAdd addedItems: [RecognizedItem], allItems: [RecognizedItem]) {
            for item in addedItems {
                if case .barcode(let barcode) = item, let payload = barcode.payloadStringValue, payload.hasPrefix("thock://pair"), !done {
                    done = true
                    dataScanner.stopScanning()
                    onCode(payload)
                }
            }
        }
    }
}
