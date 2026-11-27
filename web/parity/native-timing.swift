import Foundation

enum VideoRenderTiming {
    static let fps = 24
    static let shortestRender = 124
    static let maximumShotSeconds = 15.0
    static let maximumShots = 24

    static func frames(for seconds: Double) -> Int { Int((seconds * Double(fps)).rounded()) }

    static func renderFrames(for visibleFrames: Int) -> Int {
        let frames = max(shortestRender, visibleFrames)
        return frames + (5 - frames % 17 + 17) % 17
    }

    static func label(for frames: Int) -> String {
        let seconds = Double(frames) / Double(fps)
        return frames.isMultiple(of: fps) ? "\(Int(seconds)) s" : String(format: "%.1f s", seconds)
    }
}

let mode = CommandLine.arguments[1]
for value in CommandLine.arguments.dropFirst(2) {
    switch mode {
    case "timing":
        if let seconds = Double(value), seconds.isFinite, (1...15).contains(seconds) {
            let visible = VideoRenderTiming.frames(for: seconds)
            let rendered = VideoRenderTiming.renderFrames(for: visible)
            print("\(visible) \(rendered) \((rendered + 23) / 24)")
        } else {
            print("invalid duration")
        }
    default:
        fatalError("unknown command")
    }
}
