import CoreGraphics
import Foundation
import ImageIO

let expectedPixels: [[UInt8]] = [
    [255, 0, 0, 255],
    [0, 255, 0, 255],
    [0, 0, 255, 255],
    [255, 255, 255, 255],
]
let allProperties = ["h24", "solar", "apr"]

guard (2...3).contains(CommandLine.arguments.count) else {
    fatalError("usage: swift verify-imageio.swift FIXTURE.heic [h24,solar,apr]")
}
let expectedProperties = CommandLine.arguments.count == 3
    ? CommandLine.arguments[2].split(separator: ",").map(String.init)
    : allProperties
let url = URL(fileURLWithPath: CommandLine.arguments[1]) as CFURL
guard let source = CGImageSourceCreateWithURL(url, nil) else {
    fatalError("ImageIO could not open the fixture")
}
precondition(CGImageSourceGetCount(source) == expectedPixels.count)

var observed: [[String: Any]] = []
for index in 0..<expectedPixels.count {
    guard let image = CGImageSourceCreateImageAtIndex(source, index, nil) else {
        fatalError("ImageIO could not decode image \(index)")
    }
    precondition(image.width == 8 && image.height == 8)

    var pixel = [UInt8](repeating: 0, count: 4)
    guard let context = CGContext(
        data: &pixel,
        width: 1,
        height: 1,
        bitsPerComponent: 8,
        bytesPerRow: 4,
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
        fatalError("could not create pixel context")
    }
    context.interpolationQuality = .none
    context.draw(image, in: CGRect(x: 0, y: 0, width: 1, height: 1))
    precondition(zip(pixel, expectedPixels[index]).allSatisfy {
        abs(Int($0.0) - Int($0.1)) <= 2
    })

    let metadata = CGImageSourceCopyMetadataAtIndex(source, index, nil)
    var properties: [String] = []
    if let metadata {
        for property in allProperties {
            let path = "apple_desktop:\(property)" as CFString
            if let tag = CGImageMetadataCopyTagWithPath(metadata, nil, path),
               CGImageMetadataTagCopyValue(tag) is String {
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
        "imageIOCount": CGImageSourceGetCount(source),
        "images": observed,
    ],
    options: [.prettyPrinted, .sortedKeys]
)
FileHandle.standardOutput.write(output)
FileHandle.standardOutput.write(Data("\n".utf8))
