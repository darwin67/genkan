import CoreGraphics
import CryptoKit
import Foundation
import ImageIO

let expectedPixels: [[UInt8]] = [
    [255, 0, 0, 255],
    [0, 255, 0, 255],
    [0, 0, 255, 255],
    [255, 255, 255, 255],
]
let allProperties = ["h24", "solar", "apr"]
let appleDesktopNamespace = "http://ns.apple.com/namespace/1.0/"
let expectedHashes = [
    "synthetic-all-properties.heic": "1c38aa9d81234a69873d24a81eec13ca94a5fae0f9fabe0cf034f87f31255f00",
    "imageio-wallpapper-h24.heic": "2f82da28545f7d26eb79132d918f1b969d50b29044351ce3732197283e41474f",
]

guard (2...3).contains(CommandLine.arguments.count) else {
    fatalError("usage: swift verify-imageio.swift FIXTURE.heic [h24,solar,apr]")
}
let expectedProperties = CommandLine.arguments.count == 3
    ? CommandLine.arguments[2].split(separator: ",").map(String.init)
    : allProperties
let fixtureURL = URL(fileURLWithPath: CommandLine.arguments[1])
let fixture = try Data(contentsOf: fixtureURL)
let fixtureHash = SHA256.hash(data: fixture).map { String(format: "%02x", $0) }.joined()
precondition(fixtureHash == expectedHashes[fixtureURL.lastPathComponent])

guard let source = CGImageSourceCreateWithURL(fixtureURL as CFURL, nil) else {
    fatalError("ImageIO could not open the fixture")
}
precondition(CGImageSourceGetCount(source) == expectedPixels.count)

func dictionary(_ value: Any, keys: Set<String>) -> [String: Any] {
    guard let dictionary = value as? [String: Any] else {
        fatalError("expected a dictionary, got \(type(of: value))")
    }
    precondition(Set(dictionary.keys) == keys)
    return dictionary
}

func integer(_ value: Any?) -> Int {
    guard let value = value as? NSNumber else {
        fatalError("expected an integer")
    }
    precondition(CFGetTypeID(value) != CFBooleanGetTypeID())
    return value.intValue
}

func double(_ value: Any?) -> Double {
    guard let value = value as? NSNumber else {
        fatalError("expected a number")
    }
    precondition(CFGetTypeID(value) != CFBooleanGetTypeID())
    return value.doubleValue
}

func validateAppearance(_ value: Any?) {
    let appearance = dictionary(value as Any, keys: ["d", "l"])
    precondition(integer(appearance["d"]) == 3)
    precondition(integer(appearance["l"]) == 0)
}

func validateProperty(_ property: String, value: Any) {
    switch property {
    case "h24":
        let root = dictionary(value, keys: ["ap", "ti"])
        validateAppearance(root["ap"])
        guard let points = root["ti"] as? [Any] else {
            fatalError("h24 ti must be an array")
        }
        precondition(points.count == 4)
        for (index, pointValue) in points.enumerated() {
            let point = dictionary(pointValue, keys: ["i", "t"])
            precondition(integer(point["i"]) == index)
            precondition(double(point["t"]) == Double(index) / 4.0)
        }
    case "solar":
        let root = dictionary(value, keys: ["ap", "si"])
        validateAppearance(root["ap"])
        guard let points = root["si"] as? [Any] else {
            fatalError("solar si must be an array")
        }
        let expected: [(Int, Double, Double)] = [
            (0, -20, 0),
            (1, 15, 90),
            (2, 55, 180),
            (3, 10, 270),
        ]
        precondition(points.count == expected.count)
        for (pointValue, expectedPoint) in zip(points, expected) {
            let point = dictionary(pointValue, keys: ["a", "i", "z"])
            precondition(integer(point["i"]) == expectedPoint.0)
            precondition(double(point["a"]) == expectedPoint.1)
            precondition(double(point["z"]) == expectedPoint.2)
        }
    case "apr":
        validateAppearance(value)
    default:
        fatalError("unknown expected Apple property \(property)")
    }
}

var observed: [[String: Any]] = []
for index in 0..<expectedPixels.count {
    guard let image = CGImageSourceCreateImageAtIndex(source, index, nil) else {
        fatalError("ImageIO could not decode image \(index)")
    }
    precondition(image.width == 8 && image.height == 8)

    var pixel = [UInt8](repeating: 0, count: 4)
    pixel.withUnsafeMutableBytes { buffer in
        let bitmapInfo = CGBitmapInfo.byteOrder32Big.union(
            CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue)
        )
        guard let colorSpace = CGColorSpace(name: CGColorSpace.sRGB),
              let context = CGContext(
                  data: buffer.baseAddress,
                  width: 1,
                  height: 1,
                  bitsPerComponent: 8,
                  bytesPerRow: 4,
                  space: colorSpace,
                  bitmapInfo: bitmapInfo.rawValue
              ) else {
            fatalError("could not create pixel context")
        }
        context.interpolationQuality = .none
        context.draw(image, in: CGRect(x: 0, y: 0, width: 1, height: 1))
    }
    precondition(zip(pixel, expectedPixels[index]).allSatisfy {
        abs(Int($0.0) - Int($0.1)) <= 2
    })

    let metadata = CGImageSourceCopyMetadataAtIndex(source, index, nil)
    var properties: [String] = []
    if let metadata {
        for property in allProperties {
            let path = "apple_desktop:\(property)" as CFString
            if let tag = CGImageMetadataCopyTagWithPath(metadata, nil, path) {
                precondition(CGImageMetadataTagCopyNamespace(tag) as String == appleDesktopNamespace)
                guard let encoded = CGImageMetadataTagCopyValue(tag) as? String,
                      let data = Data(base64Encoded: encoded) else {
                    fatalError("\(property) must contain Base64 data")
                }
                var format = PropertyListSerialization.PropertyListFormat.binary
                let value = try PropertyListSerialization.propertyList(
                    from: data,
                    options: [],
                    format: &format
                )
                precondition(format == .binary)
                validateProperty(property, value: value)
                properties.append(property)
            }
        }
    }
    precondition(properties == (index == 0 ? expectedProperties : []))
    observed.append([
        "index": index,
        "width": image.width,
        "height": image.height,
        "rgba": pixel,
        "appleProperties": properties,
    ])
}

let output = try JSONSerialization.data(
    withJSONObject: [
        "fixtureSHA256": fixtureHash,
        "imageIOCount": CGImageSourceGetCount(source),
        "images": observed,
    ],
    options: [.prettyPrinted, .sortedKeys]
)
FileHandle.standardOutput.write(output)
FileHandle.standardOutput.write(Data("\n".utf8))
