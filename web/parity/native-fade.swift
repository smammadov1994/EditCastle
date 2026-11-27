import Foundation

// FadeCurve as used by the native audio and picture inspectors.
enum FadeCurve: String, Codable, CaseIterable {
    case linear, easeIn, easeOut, smooth

    var label: String {
        switch self {
        case .linear: return "Linear"
        case .easeIn: return "Exponential"
        case .easeOut: return "Logarithmic"
        case .smooth: return "S-Curve"
        }
    }

    /// Maps fade progress (0 = silent/transparent, 1 = full) through the curve.
    func apply(_ x: Double) -> Double {
        let x = min(1, max(0, x))
        switch self {
        case .linear: return x
        case .easeIn: return x * x * x
        case .easeOut: return 1 - pow(1 - x, 3)
        case .smooth: return x * x * (3 - 2 * x)
        }
    }
}
for raw in CommandLine.arguments.dropFirst(2) {
    let fields = raw.split(separator: ",", omittingEmptySubsequences: false)
    guard fields.count == 2, let curve = FadeCurve(rawValue: String(fields[0])), let at = Double(fields[1]), at.isFinite else { print("invalid fade"); continue }
    print(String(format: "%.6f", curve.apply(at)))
}
