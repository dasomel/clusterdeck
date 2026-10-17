#!/usr/bin/env swift
import Foundation
import CFNetwork

let arguments = CommandLine.arguments
guard arguments.count >= 2 else {
    print("Usage: swift verify-safari-resolution.swift <hostname>")
    exit(1)
}

let hostName = arguments[1]
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

if !resolvedIps.isEmpty {
    print("✅ Apple Network Stack RESOLVED in \(elapsedDnsMs):")
    print("   IP Addresses: \(resolvedIps.joined(separator: ", "))")
} else {
    if elapsedDns > 4.5 && !hostName.hasSuffix(".local") {
        print("❌ D-DEAD STALL DETECTED in \(elapsedDnsMs) (Exceeded 4.5s threshold!)")
        exit(2)
    } else if elapsedDns > 4.5 && hostName.hasSuffix(".local") {
        print("ℹ️ NXDOMAIN for .local in \(elapsedDnsMs) (Apple mDNS multicast probe fallback)")
    } else {
        print("ℹ️ FAST NXDOMAIN in \(elapsedDnsMs): Host not found (Clean negative answer)")
    }
}

// Step 2: URLSession quick probe (if IP was resolved)
if !resolvedIps.isEmpty {
    let url = URL(string: "http://\(hostName)")!
    let config = URLSessionConfiguration.ephemeral
    config.timeoutIntervalForRequest = 2.0
    config.timeoutIntervalForResource = 2.0
    let session = URLSession(configuration: config)
    
    let sem = DispatchSemaphore(value: 0)
    let startHttp = Date()
    session.dataTask(with: url) { _, resp, err in
        let elapsedHttp = Date().timeIntervalSince(startHttp)
        let elapsedHttpMs = String(format: "%.1f ms", elapsedHttp * 1000)
        if let http = resp as? HTTPURLResponse {
            print("✅ Safari URLSession HTTP Response: \(http.statusCode) in \(elapsedHttpMs)")
        } else if let err = err as? NSError {
            if err.code == -1004 { // Could not connect to server (port closed)
                print("✅ Safari URLSession TCP Connected/Refused in \(elapsedHttpMs) (Target IP reached)")
            } else {
                print("ℹ️ Safari URLSession Result in \(elapsedHttpMs): [\(err.domain) \(err.code)] \(err.localizedDescription)")
            }
        }
        sem.signal()
    }.resume()
    _ = sem.wait(timeout: .now() + 2.5)
}
print("==================================================")
