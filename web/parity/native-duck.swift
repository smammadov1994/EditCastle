import Foundation

// AudioLevels.duckGain from the Mac editor.
enum AudioLevels {
    static func duckGain(at t: Double, spans: [(start: Double, end: Double)], low: Double) -> Double {
        let attack = 0.15, release = 0.5
        var g = 1.0
        for s in spans {
            if t < s.start - attack || t > s.end + release { continue }
            let f: Double
            if t < s.start { f = (s.start - t) / attack } else if t > s.end { f = (t - s.end) / release } else { f = 0 }
            g = min(g, low + (1 - low) * f)
        }
        return g
    }
}

for value in CommandLine.arguments.dropFirst(2) {
    let fields = value.split(separator: ",").compactMap { Double($0) }
    guard fields.count >= 2, fields.count.isMultiple(of: 2) else { exit(1) }
    let spans = stride(from: 2, to: fields.count, by: 2).map { (start: fields[$0], end: fields[$0 + 1]) }
    let gain = AudioLevels.duckGain(at: fields[1], spans: spans, low: pow(10, fields[0] / 20))
    print(String(format: "%.6f", gain))
}
