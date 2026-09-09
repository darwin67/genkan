import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let colors: [(String, [CGFloat])] = [
    ("00-red.png", [1, 0, 0, 1]),
    ("06-green.png", [0, 1, 0, 1]),
    ("12-blue.png", [0, 0, 1, 1]),
    ("18-white.png", [1, 1, 1, 1]),
]

guard CommandLine.arguments.count == 2 else {
    fatalError("usage: swift generate-png-inputs.swift OUTPUT_DIRECTORY")
}
let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

for (name, components) in colors {
    guard let colorSpace = CGColorSpace(name: CGColorSpace.sRGB),
          let context = CGContext(
        data: nil,
        width: 8,
        height: 8,
        bitsPerComponent: 8,
        bytesPerRow: 32,
        space: colorSpace,
        bitmapInfo: CGBitmapInfo.byteOrder32Big.union(
            CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue)
        ).rawValue
    ) else {
        fatalError("could not create image context")
    }
    context.setFillColor(CGColor(colorSpace: context.colorSpace!, components: components)!)
    context.fill(CGRect(x: 0, y: 0, width: 8, height: 8))
    guard let image = context.makeImage() else {
        fatalError("could not create image")
    }

    let output = directory.appendingPathComponent(name) as CFURL
    guard let destination = CGImageDestinationCreateWithURL(
        output,
        UTType.png.identifier as CFString,
        1,
        nil
    ) else {
        fatalError("could not create PNG destination")
    }
    CGImageDestinationAddImage(destination, image, nil)
    precondition(CGImageDestinationFinalize(destination))
}
