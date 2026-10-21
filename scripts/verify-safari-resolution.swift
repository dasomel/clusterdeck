#!/usr/bin/env swift
import Foundation
import CFNetwork

// Exit codes: 0 pass | 1 usage | 2 resolver stall | 3 wrong IP | 4 unexpected
// resolution result | 5 URLSession probe failed or timed out
let usage = """
Usage: swift verify-safari-resolution.swift <hostname> [--expect-ip <ipv4>] [--expect-failure]
  --expect-ip <ipv4>  PASS only if the hostname resolves and <ipv4> is among the answers
  --expect-failure    PASS only if resolution fails fast (clean NXDOMAIN, no stall)
Without either option, the hostname must resolve to at least one IPv4 address.
"""

var hostName: String?
var expectedIp: String?
var expectFailure = false
var argIndex = 1
let arguments = CommandLine.arguments
while argIndex < arguments.count {
    switch arguments[argIndex] {
    case "--expect-ip":
        argIndex += 1
        guard argIndex < arguments.count else { print(usage); exit(1) }
        expectedIp = arguments[argIndex]
    case "--expect-failure":
        expectFailure = true
    case "-h", "--help":
        print(usage)
        exit(0)
    default:
        guard hostName == nil, !arguments[argIndex].hasPrefix("--") else { print(usage); exit(1) }
        hostName = arguments[argIndex]
    }
    argIndex += 1
}
guard let hostName = hostName, !(expectFailure && expectedIp != nil) else {
    print(usage)
    exit(1)
}

print("==================================================")
print("🌐 Testing Safari / Apple Network Framework Stack")
print("Target: \(hostName)")
print("==================================================")

// Step 1: Pure CFNetwork / CFHost resolution (exact DNS stack used by Safari/mDNSResponder)
let startDns = Date()
let host = CFHostCreateWithName(nil, hostName as CFString).takeRetainedValue()
let success = CFHostStartInfoResolution(host, .addresses, nil)
let elapsedDns = Date().timeIntervalSince(startDns)
let elapsedDnsMs = String(format: "%.1f ms", elapsedDns * 1000)

var resolvedIps: [String] = []
if success {
    var resolved: DarwinBoolean = false
    if let addresses = CFHostGetAddressing(host, &resolved)?.takeUnretainedValue() as? [Data], resolved.boolValue {
        resolvedIps = addresses.compactMap { data -> String? in
            data.withUnsafeBytes { rawBuffer -> String? in
                guard let baseAddr = rawBuffer.baseAddress else { return nil }
                let family = baseAddr.assumingMemoryBound(to: sockaddr.self).pointee.sa_family
                if family == sa_family_t(AF_INET) {
                    let addrIn = baseAddr.assumingMemoryBound(to: sockaddr_in.self).pointee
                    var ipBuf = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
                    var sinAddr = addrIn.sin_addr
                    inet_ntop(AF_INET, &sinAddr, &ipBuf, socklen_t(INET_ADDRSTRLEN))
                    return String(cString: ipBuf)
                }
                return nil
            }
        }
    }
}

let stalled = elapsedDns > 4.5
if resolvedIps.isEmpty {
    if stalled {
        print("❌ STALL DETECTED in \(elapsedDnsMs) (exceeded 4.5s threshold)")
        exit(2)
    }
    if expectFailure {
        print("✅ FAST NXDOMAIN in \(elapsedDnsMs), as expected")
        print("==================================================")
        exit(0)
    }
    print("❌ Resolution FAILED in \(elapsedDnsMs): host not found")
    exit(4)
}
print("✅ Apple Network Stack RESOLVED in \(elapsedDnsMs):")
print("   IP Addresses: \(resolvedIps.joined(separator: ", "))")
if expectFailure {
    print("❌ Expected resolution failure but the host resolved")
    exit(4)
}
if let expectedIp = expectedIp, !resolvedIps.contains(expectedIp) {
    print("❌ IP MISMATCH: expected \(expectedIp), got \(resolvedIps.joined(separator: ", "))")
    exit(3)
}

// Step 2: URLSession probe. A bare TCP refusal still proves the resolved IP was reachable.
let url = URL(string: "http://\(hostName)")!
let config = URLSessionConfiguration.ephemeral
config.timeoutIntervalForRequest = 2.0
config.timeoutIntervalForResource = 2.0
let session = URLSession(configuration: config)

final class ProbeResult: @unchecked Sendable { var ok = false }
let probe = ProbeResult()
let sem = DispatchSemaphore(value: 0)
let startHttp = Date()
session.dataTask(with: url) { _, resp, err in
    let elapsedHttpMs = String(format: "%.1f ms", Date().timeIntervalSince(startHttp) * 1000)
    if let http = resp as? HTTPURLResponse {
        print("✅ Safari URLSession HTTP Response: \(http.statusCode) in \(elapsedHttpMs)")
        probe.ok = true
    } else if let err = err as NSError?, err.code == NSURLErrorCannotConnectToHost {
        print("✅ Safari URLSession TCP refused in \(elapsedHttpMs) (target IP reached)")
        probe.ok = true
    } else if let err = err as NSError? {
        print("❌ Safari URLSession failed in \(elapsedHttpMs): [\(err.domain) \(err.code)] \(err.localizedDescription)")
    }
    sem.signal()
}.resume()
if sem.wait(timeout: .now() + 2.5) == .timedOut {
    print("❌ Safari URLSession probe timed out")
    exit(5)
}
print("==================================================")
exit(probe.ok ? 0 : 5)
