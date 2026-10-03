// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "ThockKit",
    platforms: [.iOS(.v18), .macOS(.v14)],
    products: [
        .library(name: "ThockKit", targets: ["ThockKit"])
    ],
    targets: [
        .target(
            name: "ThockKit",
            resources: [.copy("Ask/Prompts")],
            swiftSettings: [.swiftLanguageMode(.v5)],
            linkerSettings: [.linkedLibrary("sqlite3")]
        ),
        .testTarget(
            name: "ThockKitTests",
            dependencies: ["ThockKit"],
            resources: [.copy("Fixtures")],
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
