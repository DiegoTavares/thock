import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// A picture ready to be written to the vault's images folder (V39 §7.1):
/// already downsized, with the name it will be filed under.
public struct ImageAttachment: Equatable, Sendable {
    /// The source's stem, which becomes the slug in the file name and the
    /// link's alt text; `photo` when the source had none.
    public var name: String
    public var bytes: Data
    /// `jpg` or `png`.
    public var fileExtension: String

    public init(name: String, bytes: Data, fileExtension: String) {
        self.name = name
        self.bytes = bytes
        self.fileExtension = fileExtension
    }
}

/// Makes a picture small enough to send (V39 §7.1): at most `maxEdge` on
/// the long side, JPEG with the quality stepped down until it is under
/// `budget`, so the write stays well inside the 2 MB payload the desk
/// accepts. A PNG that is already small stays PNG, so a screenshot keeps
/// its text crisp. ImageIO only, so the share extension's memory ceiling
/// is respected and the same code runs in the package's tests.
public enum ImageDownsizer {
    public static let maxEdge = 1600
    public static let budget = 1_000_000

    public static func prepare(_ data: Data, name: String) -> ImageAttachment? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil), CGImageSourceGetCount(source) > 0 else { return nil }
        let slug = Slug.make(name, fallback: "photo")
        let type = CGImageSourceGetType(source).flatMap { UTType($0 as String) }
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        let width = properties?[kCGImagePropertyPixelWidth] as? Int ?? 0
        let height = properties?[kCGImagePropertyPixelHeight] as? Int ?? 0
        if type == .png, data.count <= budget, max(width, height) <= maxEdge, width > 0 {
            return ImageAttachment(name: slug, bytes: data, fileExtension: "png")
        }
        for edge in [maxEdge, 1200, 800] {
            let options: [CFString: Any] = [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: edge,
            ]
            guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
            for quality in [0.8, 0.7, 0.6, 0.5] {
                guard let encoded = jpeg(image, quality: quality) else { return nil }
                if encoded.count <= budget {
                    return ImageAttachment(name: slug, bytes: encoded, fileExtension: "jpg")
                }
            }
        }
        return nil
    }

    static func jpeg(_ image: CGImage, quality: Double) -> Data? {
        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(output, UTType.jpeg.identifier as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return output as Data
    }

    /// The pixel size of an encoded picture, for tests and thumbnails.
    public static func pixelSize(of data: Data) -> (width: Int, height: Int)? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = properties[kCGImagePropertyPixelWidth] as? Int,
              let height = properties[kCGImagePropertyPixelHeight] as? Int
        else { return nil }
        return (width, height)
    }
}
