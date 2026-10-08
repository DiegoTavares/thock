import CoreText
import SwiftUI
import UIKit

/// The website's tokens and type roles (V33 §13): warm ground and ink, amber
/// for the agent and the primary action only, Petrona for dates and titles,
/// SF for everything the person writes and reads.
enum Theme {
    static func dynamic(dark: UInt32, light: UInt32) -> UIColor {
        UIColor { traits in
            UIColor(hex: traits.userInterfaceStyle == .light ? light : dark)
        }
    }

    static let groundUI = dynamic(dark: 0x151413, light: 0xF3EFE7)
    static let surfaceUI = dynamic(dark: 0x1C1B19, light: 0xFAF7F1)
    static let sunkenUI = dynamic(dark: 0x111010, light: 0xEBE6DC)
    static let ruleUI = dynamic(dark: 0x322E2A, light: 0xD9D1C3)
    static let ruleSoftUI = dynamic(dark: 0x262320, light: 0xE7E1D6)
    static let inkUI = dynamic(dark: 0xECE7DE, light: 0x1C1B19)
    static let mutedUI = dynamic(dark: 0xA89F93, light: 0x5F594F)
    static let dimUI = dynamic(dark: 0x6D675E, light: 0x978F82)
    static let amberUI = dynamic(dark: 0xE2A554, light: 0xA8711F)
    static let amberSoftUI = dynamic(dark: 0x3A2E1C, light: 0xF1E3C9)
    static let amberInkUI = UIColor(hex: 0x171613)
    static let calUI = dynamic(dark: 0x8FA5C0, light: 0x4F6A8C)
    static let goodUI = dynamic(dark: 0x86B48F, light: 0x3F7A4F)
    static let warnUI = dynamic(dark: 0xE08A5A, light: 0xB2541F)

    static let ground = Color(groundUI)
    static let surface = Color(surfaceUI)
    static let sunken = Color(sunkenUI)
    static let rule = Color(ruleUI)
    static let ruleSoft = Color(ruleSoftUI)
    static let ink = Color(inkUI)
    static let muted = Color(mutedUI)
    static let dim = Color(dimUI)
    static let amber = Color(amberUI)
    static let amberSoft = Color(amberSoftUI)
    static let amberInk = Color(amberInkUI)
    static let cal = Color(calUI)
    static let good = Color(goodUI)
    static let warn = Color(warnUI)

    private static var registered = false

    static func registerFonts() {
        guard !registered else { return }
        registered = true
        for name in ["Petrona", "Petrona-Italic"] {
            guard let url = Bundle.main.url(forResource: name, withExtension: "ttf") else { continue }
            CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
        }
    }

    /// Petrona at a weight on its variable axis, scaled with Dynamic Type.
    static func serifUI(
        _ size: CGFloat,
        weight: CGFloat = 500,
        italic: Bool = false,
        style: UIFont.TextStyle = .body
    ) -> UIFont {
        registerFonts()
        let weightAxis = 2_003_265_652
        let descriptor = UIFontDescriptor(fontAttributes: [
            .name: italic ? "Petrona-Italic" : "Petrona",
            UIFontDescriptor.AttributeName(rawValue: kCTFontVariationAttribute as String): [weightAxis: weight],
        ])
        return UIFontMetrics(forTextStyle: style).scaledFont(for: UIFont(descriptor: descriptor, size: size))
    }

    static func serif(
        _ size: CGFloat,
        weight: CGFloat = 500,
        italic: Bool = false,
        style: UIFont.TextStyle = .body
    ) -> Font {
        Font(serifUI(size, weight: weight, italic: italic, style: style))
    }

    static func mono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight, design: .monospaced)
    }

    /// The small-caps label above a card and over the date.
    static func label(_ size: CGFloat = 12) -> Font {
        .system(size: size, weight: .semibold)
    }
}

extension UIColor {
    convenience init(hex: UInt32) {
        self.init(red: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255, blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
    }
}

/// A card's heading: small caps, with an optional note on the right.
struct CardLabel: View {
    var title: String
    var note: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(title.uppercased())
                .font(Theme.label())
                .tracking(1.1)
                .foregroundStyle(Theme.dim)
            Spacer(minLength: 8)
            if let note {
                Text(note)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(Theme.amber)
            }
        }
    }
}

struct Hairline: View {
    var color = Theme.rule

    var body: some View {
        Rectangle().fill(color).frame(height: 1)
    }
}

/// The on/off switch of the design: amber when on, a rule-coloured track off.
struct ThockToggleStyle: ToggleStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack {
            configuration.label
            Spacer(minLength: 12)
            Capsule()
                .fill(configuration.isOn ? Theme.amber : Theme.rule)
                .frame(width: 44, height: 26)
                .overlay(alignment: configuration.isOn ? .trailing : .leading) {
                    Circle()
                        .fill(configuration.isOn ? Theme.amberInk : Theme.surface)
                        .frame(width: 20, height: 20)
                        .padding(3)
                }
                .animation(.snappy(duration: 0.18), value: configuration.isOn)
        }
        .contentShape(Rectangle())
        .onTapGesture { configuration.isOn.toggle() }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityValue(configuration.isOn ? "On" : "Off")
    }
}

/// The sheet header of the design: a quiet action, a title, the primary one.
struct SheetHeader: View {
    var leading: String
    var title: String
    var trailing: String
    var trailingEnabled = true
    var onLeading: () -> Void
    var onTrailing: () -> Void

    var body: some View {
        VStack(spacing: 10) {
            Capsule().fill(Theme.rule).frame(width: 44, height: 5)
            HStack {
                Button(leading, action: onLeading)
                    .font(.system(size: 17))
                    .foregroundStyle(Theme.dim)
                Spacer()
                Button(trailing, action: onTrailing)
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(trailingEnabled ? Theme.amber : Theme.dim)
                    .disabled(!trailingEnabled)
            }
            .overlay {
                Text(title)
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(Theme.ink)
            }
        }
        .padding(.top, 10)
    }
}
