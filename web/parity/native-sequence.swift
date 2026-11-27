import Foundation

// Project.snapFrame(_:) and the supported rates from EditorModel.
let rates: [Double] = [23.976, 24, 25, 29.97, 30, 50, 59.94, 60, 100, 119.88, 120]
for value in CommandLine.arguments.dropFirst(2) {
    let parts = value.split(separator: ",", omittingEmptySubsequences: false)
    guard parts.count == 2, let fps = Double(parts[0]), rates.contains(fps),
          let time = Double(parts[1]), (0..<7200).contains(time) else {
        print("invalid edit time")
        continue
    }
    let snapped = (time * fps).rounded() / fps
    print(Int((snapped * fps).rounded()))
}
