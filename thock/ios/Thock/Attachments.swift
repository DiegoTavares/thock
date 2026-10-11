import PhotosUI
import SwiftUI
import ThockKit
import UIKit

/// The pictures attached to a capture (V39 §7.1): thumbnails above the
/// chips, each removable with a tap, a photo button for the library and a
/// camera button beside it. Every picture is downsized as it arrives, so
/// what the strip shows is what will be written.
struct AttachmentStrip: View {
    @Binding var images: [ImageAttachment]
    @State private var picked: [PhotosPickerItem] = []
    @State private var showingCamera = false

    private static let limit = 4

    var body: some View {
        HStack(spacing: 10) {
            ForEach(Array(images.enumerated()), id: \.offset) { index, image in
                Button {
                    images.remove(at: index)
                    UISelectionFeedbackGenerator().selectionChanged()
                } label: {
                    ZStack(alignment: .topTrailing) {
                        if let thumbnail = UIImage(data: image.bytes) {
                            Image(uiImage: thumbnail)
                                .resizable()
                                .scaledToFill()
                                .frame(width: 52, height: 52)
                                .clipShape(RoundedRectangle(cornerRadius: 10))
                        } else {
                            RoundedRectangle(cornerRadius: 10).fill(Theme.rule).frame(width: 52, height: 52)
                        }
                        Image(systemName: "xmark.circle.fill")
                            .font(.system(size: 14))
                            .foregroundStyle(Theme.ink, Theme.surface)
                            .offset(x: 4, y: -4)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Picture \(index + 1)")
                .accessibilityHint("Removes it")
            }
            if images.count < Self.limit {
                PhotosPicker(selection: $picked, maxSelectionCount: Self.limit - images.count, matching: .images) {
                    stripButton("photo.on.rectangle", label: "Photo")
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Add a picture from your photos")
                if UIImagePickerController.isSourceTypeAvailable(.camera) {
                    Button {
                        showingCamera = true
                    } label: {
                        stripButton("camera", label: "Camera")
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Take a picture")
                }
            }
            Spacer(minLength: 0)
        }
        .onChange(of: picked) { _, items in
            guard !items.isEmpty else { return }
            picked = []
            Task {
                for item in items {
                    guard let data = try? await item.loadTransferable(type: Data.self) else { continue }
                    let name = item.itemIdentifier.map { String($0.prefix(8)) } ?? "photo"
                    if let attachment = ImageDownsizer.prepare(data, name: name), images.count < Self.limit {
                        images.append(attachment)
                    }
                }
            }
        }
        .fullScreenCover(isPresented: $showingCamera) {
            CameraPicker { data in
                if let data, let attachment = ImageDownsizer.prepare(data, name: "photo"), images.count < Self.limit {
                    images.append(attachment)
                }
                showingCamera = false
            }
            .ignoresSafeArea()
        }
    }

    private func stripButton(_ symbol: String, label: String) -> some View {
        VStack(spacing: 3) {
            Image(systemName: symbol)
                .font(.system(size: 18))
            Text(label)
                .font(.system(size: 10, weight: .medium))
        }
        .foregroundStyle(Theme.muted)
        .frame(width: 52, height: 52)
        .background(Theme.ground, in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Theme.rule, lineWidth: 1))
    }
}

/// The system camera, handing back the shot as JPEG bytes, or `nil` when
/// the person cancels.
struct CameraPicker: UIViewControllerRepresentable {
    var onFinish: (Data?) -> Void

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {}

    func makeCoordinator() -> Coordinator {
        Coordinator(onFinish: onFinish)
    }

    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let onFinish: (Data?) -> Void

        init(onFinish: @escaping (Data?) -> Void) {
            self.onFinish = onFinish
        }

        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            let image = info[.originalImage] as? UIImage
            onFinish(image?.jpegData(compressionQuality: 0.92))
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            onFinish(nil)
        }
    }
}
