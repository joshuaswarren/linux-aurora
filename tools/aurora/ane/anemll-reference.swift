import Foundation
import CoreML

// Native reference only. CPU_AND_NE allows fallback; hardware traces must
// establish which work actually reached ANE before making a hardware claim.
DispatchQueue.global(qos: .background).asyncAfter(deadline: .now() + 180) {
    fputs("REFERENCE_TIMEOUT\n", stderr)
    exit(124)
}

struct Reference: Decodable {
    let generated_tokens: [Int]
    let comparisons: [Comparison]
    struct Comparison: Decodable { let reference_argmax: Int }
}

func provider(token: Int, position: Int) throws -> MLDictionaryFeatureProvider {
    let ids = try MLMultiArray(shape: [1, 1], dataType: .int32)
    let pos = try MLMultiArray(shape: [1], dataType: .int32)
    let current = try MLMultiArray(shape: [1], dataType: .int32)
    let mask = try MLMultiArray(shape: [1, 1, 1, 32], dataType: .float16)
    ids[0] = NSNumber(value: token)
    pos[0] = NSNumber(value: position)
    current[0] = NSNumber(value: position)
    for i in 0..<32 { mask[i] = NSNumber(value: Float(i <= position ? 0 : -65504)) }
    return try MLDictionaryFeatureProvider(dictionary: [
        "input_ids": ids, "position_ids": pos, "current_pos": current,
        "causal_mask": mask
    ])
}

func evaluate(_ model: MLModel, tokens: [Int], count: Int, label: String) throws -> [[Float]] {
    let state = model.makeState()
    var results: [[Float]] = []
    for position in 0..<count {
        let prediction = try model.prediction(from: provider(token: tokens[position], position: position), using: state)
        guard let values = prediction.featureValue(for: "logits")?.multiArrayValue,
              values.count == 512 else { throw NSError(domain: "BadLogitShape", code: 1) }
        let row = (0..<values.count).map { values[$0].floatValue }
        guard row.allSatisfy({ $0.isFinite }) else { throw NSError(domain: "NonfiniteLogits", code: 1) }
        results.append(row)
        print("\(label)_STEP position=\(position) input=\(tokens[position]) argmax=\(row.indices.max(by: { row[$0] < row[$1] })!)")
        fflush(stdout)
    }
    return results
}

do {
    guard CommandLine.arguments.count == 5,
          let count = Int(CommandLine.arguments[4]), (1...17).contains(count) else {
        throw NSError(domain: "Usage: anemll-reference model.mlpackage reference.json output.json steps", code: 1)
    }
    let reference = try JSONDecoder().decode(Reference.self, from: Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[2])))
    guard reference.generated_tokens.count >= count, reference.comparisons.count >= count else {
        throw NSError(domain: "ShortReference", code: 1)
    }
    let compiled = try MLModel.compileModel(at: URL(fileURLWithPath: CommandLine.arguments[1]))
    defer { try? FileManager.default.removeItem(at: compiled) }
    let cpuConfig = MLModelConfiguration()
    cpuConfig.computeUnits = .cpuOnly
    let cpu = try MLModel(contentsOf: compiled, configuration: cpuConfig)
    let control = try evaluate(cpu, tokens: reference.generated_tokens, count: count, label: "CPU")
    for i in 0..<count {
        guard control[i].indices.max(by: { control[i][$0] < control[i][$1] }) == reference.comparisons[i].reference_argmax else {
            throw NSError(domain: "CoreMLCPUReferenceTokenMismatch", code: i)
        }
    }
    print("CPU_REFERENCE_PASS steps=\(count)")
    fflush(stdout)
    let configuration = MLModelConfiguration()
    configuration.computeUnits = .cpuAndNeuralEngine
    print("CANDIDATE_LOAD_BEGIN")
    fflush(stdout)
    let model = try MLModel(contentsOf: compiled, configuration: configuration)
    print("CANDIDATE_LOADED_WAITING_FOR_CAPTURE seconds=15")
    fflush(stdout)
    Thread.sleep(forTimeInterval: 15)
    let outputs = try evaluate(model, tokens: reference.generated_tokens, count: count, label: "CPU_AND_ANE")
    var errors: [Float] = []
    for i in 0..<count {
        errors.append(zip(control[i], outputs[i]).map { abs($0 - $1) }.max()!)
    }
    let result: [String: Any] = ["steps": count, "cpu_logits": control, "candidate_logits": outputs,
                               "max_abs_errors": errors, "linux_ane_tested": false,
                               "native_ane_execution_requires_trace": true]
    let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
    try data.write(to: URL(fileURLWithPath: CommandLine.arguments[3]), options: .atomic)
    print("NATIVE_REFERENCE_SAVED max_abs_error=\(errors.max()!) hardware_execution_requires_trace=1")
    fflush(stdout)
} catch {
    fputs("REFERENCE_FAILED: \(error)\n", stderr)
    exit(1)
}
