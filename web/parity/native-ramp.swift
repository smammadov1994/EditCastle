import Foundation

/// A clip's speed changing over its footage: from `startSpeed`, easing into the clip's own speed
/// between `inFrom` and `inTo`, then easing out to `endSpeed` between `outFrom` and `outTo`. Times are
/// in the clip's source (seconds of footage), so a trim or a split just shows a different part of the
/// same curve, and each half of a split keeps its piece.
struct SpeedRamp: Codable, Hashable {
    var inFrom: Double
    var inTo: Double
    var outFrom: Double
    var outTo: Double
    var startSpeed: Double
    var endSpeed: Double

    /// Speed at a point in the footage, with `mid` the clip's own speed. Eased in log speed, so going
    /// from 100% to 400% feels as even as 400% to 100%.
    func speed(at s: Double, mid: Double) -> Double {
        if s < inTo {
            if s <= inFrom { return startSpeed }
            return Self.blend(startSpeed, mid, (s - inFrom) / (inTo - inFrom))
        }
        if s > outFrom {
            if s >= outTo { return endSpeed }
            return Self.blend(mid, endSpeed, (s - outFrom) / (outTo - outFrom))
        }
        return mid
    }

    private static func blend(_ a: Double, _ b: Double, _ u: Double) -> Double {
        let e = u * u * (3 - 2 * u)
        return exp(log(a) + (log(b) - log(a)) * e)
    }

    /// The fastest and slowest it plays.
    func extremes(mid: Double) -> (lo: Double, hi: Double) {
        let all = [startSpeed, mid, endSpeed]
        return (all.min()!, all.max()!)
    }

    /// Where the speed is constant: the pieces of footage between these marks.
    private var marks: [Double] { [inFrom, inTo, outFrom, outTo] }

    /// Timeline seconds to play footage from `s0` to `s1` (negative if `s1` is before `s0`).
    func span(from s0: Double, to s1: Double, mid: Double) -> Double {
        if s1 < s0 { return -span(from: s1, to: s0, mid: mid) }
        var total = 0.0
        var a = s0
        for m in marks + [s1] where m > a {
            let b = min(m, s1)
            total += piece(a, b, mid: mid)
            a = b
            if a >= s1 { break }
        }
        return total
    }

    /// One stretch with no mark inside it: exact if the speed is constant, else Simpson's rule.
    private func piece(_ a: Double, _ b: Double, mid: Double) -> Double {
        guard b > a else { return 0 }
        let va = speed(at: a, mid: mid), vb = speed(at: b, mid: mid), vm = speed(at: (a + b) / 2, mid: mid)
        if abs(va - vb) < 1e-12, abs(va - vm) < 1e-12 { return (b - a) / va }
        let n = 24
        let h = (b - a) / Double(n)
        var sum = 1 / va + 1 / vb
        for i in 1..<n { sum += (i % 2 == 0 ? 2 : 4) / speed(at: a + Double(i) * h, mid: mid) }
        return sum * h / 3
    }

    /// The footage reached after playing `dt` timeline seconds from `s0` (backwards if `dt` is negative).
    func source(from s0: Double, after dt: Double, mid: Double) -> Double {
        if abs(dt) < 1e-12 { return s0 }
        let forward = dt > 0
        var left = abs(dt)
        var a = s0
        let stops = forward ? marks.filter { $0 > s0 }.sorted() : marks.filter { $0 < s0 }.sorted(by: >)
        for m in stops {
            let t = abs(span(from: a, to: m, mid: mid))
            if t >= left { return solve(from: a, toward: m, time: left, mid: mid) }
            left -= t
            a = m
        }
        // Past every mark: constant from here.
        let v = speed(at: forward ? a + 1e-9 : a - 1e-9, mid: mid)
        return a + (forward ? 1 : -1) * left * v
    }

    /// Within one stretch: the point `time` timeline seconds from `a` toward `b`.
    private func solve(from a: Double, toward b: Double, time: Double, mid: Double) -> Double {
        var lo = 0.0, hi = 1.0
        for _ in 0..<40 {
            let m = (lo + hi) / 2
            if abs(span(from: a, to: a + (b - a) * m, mid: mid)) < time { lo = m } else { hi = m }
        }
        return a + (b - a) * (lo + hi) / 2
    }

    /// The same curve with the footage measured from somewhere else (`d` seconds later).
    func shifted(by d: Double) -> SpeedRamp {
        SpeedRamp(inFrom: inFrom + d, inTo: inTo + d, outFrom: outFrom + d, outTo: outTo + d, startSpeed: startSpeed, endSpeed: endSpeed)
    }

    /// The same curve measured from the other end of a file `length` long (for reversing a clip).
    func mirrored(length: Double) -> SpeedRamp {
        SpeedRamp(inFrom: length - outTo, inTo: length - outFrom, outFrom: length - inTo, outTo: length - inFrom,
                  startSpeed: endSpeed, endSpeed: startSpeed)
    }

    /// A ramp drawn over a clip's footage `s0...s1`, as the inspector's graph sets it, with each point a
    /// fraction of the way across: `start` speed up to `points.0`, easing to `mid` by `points.1`, holding
    /// until `points.2`, then easing to `end` by `points.3`. Nil when there's no ramp at either end.
    static func drawn(s0: Double, s1: Double, points: (Double, Double, Double, Double), start: Double, end: Double, mid: Double) -> SpeedRamp? {
        let L = s1 - s0
        let (p1, p2, p3, p4) = points
        let hasIn = p2 - p1 > 0.001, hasOut = p4 - p3 > 0.001
        guard hasIn || hasOut, L > 0 else { return nil }
        let at = { (f: Double) in s0 + L * min(1, max(0, f)) }
        let inFrom = hasIn ? at(p1) : s0, inTo = hasIn ? at(p2) : s0
        let outFrom = hasOut ? max(inTo, at(p3)) : s1, outTo = hasOut ? max(inTo, at(p4)) : s1
        return SpeedRamp(inFrom: inFrom, inTo: inTo, outFrom: outFrom, outTo: outTo,
                         startSpeed: hasIn ? start : mid, endSpeed: hasOut ? end : mid)
    }
}


for args in CommandLine.arguments.dropFirst(2) {
  let j = args.split(separator: ",").compactMap { Double($0) }
  if j.count != 10 || !j.allSatisfy({ $0.isFinite }) || j[4] <= 0 || j[5] <= 0 || j[6] <= 0 ||
      j[0] > j[1] || j[1] > j[2] || j[2] > j[3] {
    print("invalid ramp")
    continue
  }
  let r = SpeedRamp(inFrom:j[0], inTo:j[1], outFrom:j[2], outTo:j[3], startSpeed:j[4], endSpeed:j[5])
  print(String(format:"%.6f %.6f",r.span(from:j[7],to:j[8],mid:j[6]),r.source(from:j[7],after:j[9],mid:j[6])))
}
