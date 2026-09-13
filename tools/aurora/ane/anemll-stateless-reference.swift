import Foundation
import CoreML

DispatchQueue.global(qos: .background).asyncAfter(deadline: .now() + 180) {
    fputs("REFERENCE_TIMEOUT\n", stderr); exit(124)
}

func values(_ model: MLModel, _ input: MLMultiArray) throws -> [Float] {
    let provider = try MLDictionaryFeatureProvider(dictionary: ["input": input])
    guard let output = try model.prediction(from: provider).featureValue(for: "output")?.multiArrayValue,
          output.count == 16384 else { throw NSError(domain: "BadOutputShape", code: 1) }
    let floats = (0..<output.count).map { output[$0].floatValue }
    guard floats.allSatisfy({$0.isFinite}) else { throw NSError(domain: "NonfiniteOutput", code: 1) }
    return floats
}

do {
    guard CommandLine.arguments.count == 4 else {
        throw NSError(domain: "Usage: anemll-stateless-reference model.mlpackage input.bin output-prefix", code: 1)
    }
    let inputBytes = try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[2]))
    guard inputBytes.count == 4096 else { throw NSError(domain: "BadInputSize", code: 1) }
    let input = try MLMultiArray(shape: [1, 64, 1, 32], dataType: .float16)
    inputBytes.withUnsafeBytes { source in
        for i in 0..<2048 {
            let bits = source.loadUnaligned(fromByteOffset: i * 2, as: UInt16.self)
            input[i] = NSNumber(value: Float(Float16(bitPattern: UInt16(littleEndian: bits))))
        }
    }
    let compiled = try MLModel.compileModel(at: URL(fileURLWithPath: CommandLine.arguments[1]))
    defer { try? FileManager.default.removeItem(at: compiled) }
    let cc = MLModelConfiguration(); cc.computeUnits = .cpuOnly
    let cpu = try MLModel(contentsOf: compiled, configuration: cc)
    let reference = try values(cpu, input)
    print("CPU_REFERENCE_COMPLETE values=\(reference.count)"); fflush(stdout)
    let nc = MLModelConfiguration(); nc.computeUnits = .cpuAndNeuralEngine
    print("CANDIDATE_LOAD_BEGIN"); fflush(stdout)
    let candidate = try MLModel(contentsOf: compiled, configuration: nc)
    print("CANDIDATE_LOADED_WAITING_FOR_CAPTURE seconds=15"); fflush(stdout)
    Thread.sleep(forTimeInterval: 15)
    let actual = try values(candidate, input)
    var argmaxMatches = 0
    for position in 0..<32 {
        let indices = (0..<512).map { $0 * 32 + position }
        if indices.max(by: {reference[$0] < reference[$1]}) == indices.max(by: {actual[$0] < actual[$1]}) {
            argmaxMatches += 1
        }
    }
    let maxError = zip(actual, reference).map { abs($0-$1) }.max()!
    for (suffix, array) in [("cpu",reference),("candidate",actual)] {
        var data = Data(capacity: array.count * 2)
        for value in array {
            var bits = Float16(value).bitPattern.littleEndian
            withUnsafeBytes(of: &bits) { data.append(contentsOf: $0) }
        }
        try data.write(to: URL(fileURLWithPath: CommandLine.arguments[3]+"-"+suffix+".bin"),options:.atomic)
    }
    print("NATIVE_REFERENCE_SAVED values=16384 argmax_matches=\(argmaxMatches)/32 max_abs_error=\(maxError) hardware_execution_requires_trace=1")
    fflush(stdout)
    guard argmaxMatches == 32 else { throw NSError(domain: "NativeTokenMismatch",code:1) }
} catch { fputs("REFERENCE_FAILED: \(error)\n",stderr); exit(1) }
