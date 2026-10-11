// The perf suite's screen probe: times what reaches the screen
// for a terminal app, the same way for Auk and for Ghostty.
//
//   screen-probe check
//       {"screen_recording": bool, "accessibility": bool}
//   screen-probe latency --pid PID [--trials N]
//       Posts `x` key presses to PID (CGEvent, as a keyboard's reach the app) into a program that
//       echoes them, and for each finds the first captured frame of the app's window that differs
//       from the frame before the press. Latency = that frame's display time - the post time.
//       {"latencies_ms": [...]}
//       With --type-via SOCKET, the presses go through an Auk app's automation endpoint instead
//       (`type x`: an NSEvent into its window, the in-app path), which needs no Accessibility.
//       With --write-tty TTY, an `x` is written to the terminal's tty instead, as program output:
//       output to screen, the same for any terminal, no Accessibility needed.
//   screen-probe marker --pid PID --go FIFO [--timeout S]
//       Writes "go" to FIFO (the terminal's script starts its command on it) and waits for the
//       first frame of the app's window showing the marker: a pure green (0,255,0) bar at least a
//       third of the window wide. {"ms": display time of that frame - the write}
//
// Frames come from ScreenCaptureKit (an SCStream on the app's largest window, every frame the
// window server composes, at up to 240 Hz); each frame's display time is its SCStreamFrameInfo
// display time, in host (mach) time, as is the post or write time. Needs Screen Recording; the
// latency mode also needs Accessibility to post events.

import AppKit
import CoreGraphics
import CoreMedia
import Foundation
import ScreenCaptureKit

func fail(_ message: String) -> Never {
    FileHandle.standardError.write((message + "\n").data(using: .utf8)!)
    exit(1)
}

func emit(_ object: Any) {
    let data = try! JSONSerialization.data(withJSONObject: object, options: [])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write("\n".data(using: .utf8)!)
}

var timebase = mach_timebase_info_data_t()
mach_timebase_info(&timebase)
func ms(_ from: UInt64, _ to: UInt64) -> Double {
    let delta = Double(Int64(bitPattern: to &- from))
    return delta * Double(timebase.numer) / Double(timebase.denom) / 1e6
}

let args = CommandLine.arguments
func flag(_ name: String) -> String? {
    guard let i = args.firstIndex(of: name), i + 1 < args.count else { return nil }
    return args[i + 1]
}

guard args.count >= 2 else { fail("usage: screen-probe check|latency|marker ...") }
let mode = args[1]

if mode == "check" {
    emit([
        "screen_recording": CGPreflightScreenCaptureAccess(),
        "accessibility": CGPreflightPostEventAccess(),
    ])
    exit(0)
}

guard let pidText = flag("--pid"), let pid = pid_t(pidText) else { fail("missing --pid") }
// ScreenCaptureKit needs this process's window server connection.
NSApplication.shared.setActivationPolicy(.prohibited)
if !CGPreflightScreenCaptureAccess() {
    fail("Screen Recording is not granted to the app running the tests")
}

/// One captured frame: its display time and a few facts about its pixels.
struct Frame {
    let displayTime: UInt64
    let checksum: UInt64
    let marker: Bool
    /// The largest share of marker-green pixels in a row, for diagnostics.
    let greenest: Double
}

final class Capture: NSObject, SCStreamOutput, SCStreamDelegate {
    let lock = NSLock()
    var frames: [Frame] = []
    var error: Error?

    func stream(_ stream: SCStream, didOutputSampleBuffer buffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(buffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let info = attachments.first,
              let raw = info[.status] as? Int,
              SCFrameStatus(rawValue: raw) == .complete,
              let pixels = CMSampleBufferGetImageBuffer(buffer)
        else { return }
        let displayTime = (info[.displayTime] as? UInt64) ?? mach_absolute_time()
        CVPixelBufferLockBaseAddress(pixels, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixels, .readOnly) }
        let width = CVPixelBufferGetWidth(pixels)
        let height = CVPixelBufferGetHeight(pixels)
        let stride = CVPixelBufferGetBytesPerRow(pixels)
        guard let base = CVPixelBufferGetBaseAddress(pixels)?.assumingMemoryBound(to: UInt8.self) else { return }
        // FNV-1a over every pixel, and the marker test on every other row.
        var hash: UInt64 = 0xcbf29ce484222325
        var marker = false
        var greenest = 0
        for y in 0..<height {
            let row = base + y * stride
            row.withMemoryRebound(to: UInt64.self, capacity: width / 2) { words in
                for i in 0..<(width / 2) {
                    hash = (hash ^ words[i]) &* 0x100000001b3
                }
            }
            if !marker && y % 2 == 0 {
                var green = 0
                for x in 0..<width {
                    let p = row + x * 4 // BGRA, sRGB
                    if p[1] > 200 && p[0] < 90 && p[2] < 90 { green += 1 }
                }
                greenest = max(greenest, green)
                if green * 3 >= width { marker = true }
            }
        }
        lock.lock()
        frames.append(Frame(displayTime: displayTime, checksum: hash, marker: marker,
                            greenest: Double(greenest) / Double(max(width, 1))))
        lock.unlock()
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        lock.lock()
        self.error = error
        lock.unlock()
    }

    func snapshot() -> [Frame] {
        lock.lock()
        defer { lock.unlock() }
        return frames
    }
}

/// Runs an async block to completion from this synchronous program.
func await_<T>(_ body: @escaping () async throws -> T) -> T {
    let done = DispatchSemaphore(value: 0)
    var result: Result<T, Error>!
    Task {
        do { result = .success(try await body()) } catch { result = .failure(error) }
        done.signal()
    }
    done.wait()
    switch result! {
    case .success(let v): return v
    case .failure(let e): fail("ScreenCaptureKit: \(e)")
    }
}

let content = await_ { try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true) }
let windows = content.windows.filter { $0.owningApplication?.processID == pid && $0.windowLayer == 0 }
guard let window = windows.max(by: { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height }) else {
    fail("no on-screen window of process \(pid)")
}
let filter = SCContentFilter(desktopIndependentWindow: window)
let config = SCStreamConfiguration()
let scale = CGFloat(filter.pointPixelScale)
config.width = Int(window.frame.width * scale)
config.height = Int(window.frame.height * scale)
config.minimumFrameInterval = CMTime(value: 1, timescale: 240)
config.queueDepth = 8
config.pixelFormat = kCVPixelFormatType_32BGRA
config.colorSpaceName = CGColorSpace.sRGB
config.showsCursor = false
let capture = Capture()
let stream = SCStream(filter: filter, configuration: config, delegate: capture)
let queue = DispatchQueue(label: "probe frames", qos: .userInteractive)
do {
    try stream.addStreamOutput(capture, type: .screen, sampleHandlerQueue: queue)
} catch {
    fail("add the stream output: \(error)")
}
await_ { try await stream.startCapture() }
if let app = NSRunningApplication(processIdentifier: pid) {
    app.activate()
}

/// Sends `type x` to an Auk automation endpoint; returns when it was sent (host time), after
/// reading the reply.
func typeThrough(_ path: String) -> UInt64 {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    guard fd >= 0 else { fail("socket: \(errno)") }
    defer { close(fd) }
    var addr = sockaddr_un()
    addr.sun_family = sa_family_t(AF_UNIX)
    let bytes = Array(path.utf8)
    guard bytes.count < MemoryLayout.size(ofValue: addr.sun_path) else { fail("socket path too long") }
    withUnsafeMutableBytes(of: &addr.sun_path) { raw in
        for (i, b) in bytes.enumerated() { raw[i] = b }
    }
    let ok = withUnsafePointer(to: &addr) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
        }
    }
    guard ok == 0 else { fail("connect \(path): \(errno)") }
    let line = Array("type x\n".utf8)
    let sent = mach_absolute_time()
    guard write(fd, line, line.count) == line.count else { fail("write \(path)") }
    var reply = [UInt8](repeating: 0, count: 256)
    _ = read(fd, &reply, reply.count)
    return sent
}

/// Waits until a frame after index `after` satisfies `test`, for at most `timeout` seconds.
func waitFrame(after: Int, timeout: Double, _ test: (Frame) -> Bool) -> Frame? {
    let deadline = Date().addingTimeInterval(timeout)
    while Date() < deadline {
        let frames = capture.snapshot()
        if frames.count > after, let f = frames[after...].first(where: test) {
            return f
        }
        if let e = capture.error { fail("capture stopped: \(e)") }
        usleep(500)
    }
    return nil
}

/// Lets the window settle: no new picture for `quiet` seconds (up to `limit`). The window server
/// may deliver "complete" frames at the display rate while nothing in the window changed, so a
/// frame counts only when its pixels differ from the one before.
func settle(quiet: Double = 0.3, limit: Double = 5) {
    let start = Date()
    var seen = capture.snapshot().count
    var last = capture.snapshot().last?.checksum
    var since = Date()
    while Date().timeIntervalSince(start) < limit {
        usleep(20_000)
        let frames = capture.snapshot()
        for f in frames[seen...] where f.checksum != last {
            last = f.checksum
            since = Date()
        }
        seen = frames.count
        if Date().timeIntervalSince(since) >= quiet {
            return
        }
    }
}

// The window server sends a first frame when the stream starts.
_ = waitFrame(after: 0, timeout: 5) { _ in true }
usleep(500_000)
settle()

switch mode {
case "latency":
    let typeVia = flag("--type-via")
    let writeTty = flag("--write-tty")
    let tty = writeTty.map { path -> FileHandle in
        guard let h = FileHandle(forWritingAtPath: path) else { fail("open \(path)") }
        return h
    }
    if typeVia == nil && tty == nil && !CGPreflightPostEventAccess() {
        fail("Accessibility is not granted to the app running the tests")
    }
    let trials = Int(flag("--trials") ?? "") ?? 30
    let source = CGEventSource(stateID: .hidSystemState)
    var latencies: [Double] = []
    for trial in 0..<trials {
        settle(quiet: 0.15)
        let frames = capture.snapshot()
        guard let base = frames.last else { fail("no frames") }
        let index = frames.count
        // A random phase against the display's refresh.
        usleep(UInt32.random(in: 0..<8_333))
        guard let down = CGEvent(keyboardEventSource: source, virtualKey: 7, keyDown: true),
              let up = CGEvent(keyboardEventSource: source, virtualKey: 7, keyDown: false)
        else { fail("couldn't make a key event") }
        let posted: UInt64
        if let socketPath = typeVia {
            posted = typeThrough(socketPath)
        } else if let tty = tty {
            posted = mach_absolute_time()
            tty.write("x".data(using: .utf8)!)
        } else {
            posted = mach_absolute_time()
            down.postToPid(pid)
            up.postToPid(pid)
        }
        guard let shown = waitFrame(after: index, timeout: 2, { $0.checksum != base.checksum }) else {
            fail("trial \(trial): the key press never changed the window")
        }
        latencies.append(ms(posted, shown.displayTime))
    }
    emit(["latencies_ms": latencies])
case "marker":
    guard let go = flag("--go") else { fail("missing --go") }
    let timeout = Double(flag("--timeout") ?? "") ?? 120
    if capture.snapshot().contains(where: { $0.marker }) {
        fail("the marker is on screen before the command started")
    }
    let index = capture.snapshot().count
    guard let fifo = FileHandle(forWritingAtPath: go) else { fail("open \(go)") }
    let written = mach_absolute_time()
    fifo.write("go\n".data(using: .utf8)!)
    try? fifo.close()
    guard let shown = waitFrame(after: index, timeout: timeout, { $0.marker }) else {
        let frames = capture.snapshot()
        let greenest = frames.map { $0.greenest }.max() ?? 0
        fail("the marker never appeared within \(timeout) s (\(frames.count - index) frames of \(Int(window.frame.width))x\(Int(window.frame.height)) pt captured; the greenest row was \(Int(greenest * 100))% marker green)")
    }
    emit(["ms": ms(written, shown.displayTime)])
default:
    fail("unknown mode \(mode)")
}
await_ { try await stream.stopCapture() }
exit(0)
