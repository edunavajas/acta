// detect.start / detect.stop → eventos `detect.signal` crudos. La fusión y el
// antirrebote los hace Rust (acta-mac-bridge::detection::fusion).
//
// Cada `poll_ms`:
//   1. Procesos de la HAL agrupados por app dueña → mic en uso por app.
//   2. Apps nativas (Teams): ventana de llamada por AX (solo si el mic está
//      en uso o ya había ventana: así no recorremos el árbol sin motivo).
//   3. Navegadores: URL de pestañas por AppleScript (mismo criterio), solo
//      si el navegador está abierto (nunca lo lanza).
//   4. Diferencia con la foto anterior → señales.
// Solo cuentan las apps de `rules` y nunca las de dictado (Typeless, Wispr
// Flow, superwhisper, Orca, MacWhisper).

import ActaMacProtocol
import AppKit
import Foundation

struct AppObservation: Equatable, Sendable {
    var app: AppInfoWire
    var micInUse: Bool
    /// Nativa: título de la ventana de llamada (nil = no hay).
    var meetingWindow: String?
    var hasMeetingWindow: Bool
    /// Navegador: pestaña de Meet.
    var meetTab: BrowserTab?
}

struct BrowserTab: Equatable, Sendable {
    var url: String
    var title: String
}

enum SignalDiffer {
    static func diff(old: [String: AppObservation], new: [String: AppObservation]) -> [DetectSignalEvent] {
        var out: [DetectSignalEvent] = []
        for key in Set(old.keys).union(new.keys).sorted() {
            let o = old[key]
            let n = new[key]
            guard let app = n?.app ?? o?.app else { continue }
            let wasMic = o?.micInUse ?? false
            let isMic = n?.micInUse ?? false
            if let o, let n, o.app.audioPids != n.app.audioPids || o.app.pid != n.app.pid {
                out.append(DetectSignalEvent(signal: .audioPidsChanged, app: n.app, windowTitle: nil, url: nil))
            }
            if !wasMic, isMic { out.append(DetectSignalEvent(signal: .micInUse, app: app, windowTitle: nil, url: nil)) }
            if wasMic, !isMic { out.append(DetectSignalEvent(signal: .micReleased, app: app, windowTitle: nil, url: nil)) }

            let wasWin = o?.hasMeetingWindow ?? false
            let isWin = n?.hasMeetingWindow ?? false
            if !wasWin, isWin {
                out.append(DetectSignalEvent(signal: .meetingWindowOpen, app: app, windowTitle: n?.meetingWindow, url: nil))
            } else if wasWin, !isWin {
                out.append(DetectSignalEvent(signal: .meetingWindowClosed, app: app, windowTitle: o?.meetingWindow, url: nil))
            }

            let oldTab = o?.meetTab
            let newTab = n?.meetTab
            if let t = newTab, t.url != oldTab?.url {
                out.append(DetectSignalEvent(signal: .meetTabOpen, app: app, windowTitle: t.title, url: t.url))
            } else if newTab == nil, let t = oldTab {
                out.append(DetectSignalEvent(signal: .meetTabClosed, app: app, windowTitle: t.title, url: t.url))
            }
        }
        return out
    }
}

enum MeetUrl {
    /// Código de sala de Meet: abc-defg-hij.
    static let roomPattern = try! NSRegularExpression(pattern: #"meet\.google\.com/[a-z]{3}-[a-z]{4}-[a-z]{3}(?:[/?#]|$)"#, options: [.caseInsensitive])

    /// Cuenta si contiene un patrón; para los de meet.google.com exige además
    /// un código de sala (la portada y "landing" no son reuniones).
    static func isMeeting(_ url: String, patterns: [String]) -> Bool {
        let low = url.lowercased()
        for p in patterns where low.contains(p.lowercased()) {
            if p.lowercased().contains("meet.google.com") {
                let r = NSRange(url.startIndex..., in: url)
                if roomPattern.firstMatch(in: url, range: r) != nil { return true }
            } else {
                return true
            }
        }
        return false
    }
}

enum BrowserTabs {
    static func script(bundleId: String) -> String {
        let titleProp = bundleId == "com.apple.Safari" ? "name" : "title"
        return """
        if application id "\(bundleId)" is running then
          with timeout of 3 seconds
            tell application id "\(bundleId)"
              set out to ""
              repeat with w in windows
                repeat with t in tabs of w
                  set out to out & (URL of t) & (character id 9) & (\(titleProp) of t) & (character id 10)
                end repeat
              end repeat
              return out
            end tell
          end timeout
        end if
        return ""
        """
    }

    static func parse(_ text: String) -> [BrowserTab] {
        text.split(separator: "\n").compactMap { line in
            let parts = line.split(separator: "\t", maxSplits: 1, omittingEmptySubsequences: false)
            guard let url = parts.first.map(String.init), !url.isEmpty, url != "missing value" else { return nil }
            return BrowserTab(url: url, title: parts.count > 1 ? String(parts[1]) : "")
        }
    }

    /// `nil` si falla (p. ej. Automatización denegada) o el navegador no contesta.
    static func tabs(bundleId: String) async -> [BrowserTab]? {
        guard let out = await osascript(script(bundleId: bundleId), timeout: 4) else {
            Log.debug("applescript \(bundleId): failed or timed out")
            return nil
        }
        return parse(out)
    }

    /// Fuera de proceso a propósito: con NSAppleScript dentro del sidecar, un
    /// navegador que no contesta colgaba la llamada para siempre (`with timeout`
    /// no la corta) y mientras tanto el main actor dejaba de leer stdin, así que
    /// comandos como capture.start se quedaban sin respuesta.
    static func osascript(_ source: String, timeout: TimeInterval) async -> String? {
        await withCheckedContinuation { c in
            DispatchQueue.global(qos: .utility).async {
                let p = Process()
                p.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
                p.arguments = ["-e", source]
                let out = Pipe()
                p.standardOutput = out
                p.standardError = FileHandle.nullDevice
                do { try p.run() } catch {
                    c.resume(returning: nil)
                    return
                }
                let pid = p.processIdentifier
                let timer = DispatchWorkItem { kill(pid, SIGKILL) }
                DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: timer)
                // Leer antes de esperar: con muchas pestañas la salida llena el pipe.
                let data = out.fileHandleForReading.readDataToEndOfFile()
                p.waitUntilExit()
                timer.cancel()
                c.resume(returning: p.terminationStatus == 0 ? String(decoding: data, as: UTF8.self) : nil)
            }
        }
    }
}

public actor DetectionService {
    private let output: Output
    private var task: Task<Void, Never>?

    public init(output: Output) { self.output = output }

    public func start(_ args: DetectStartArgs) {
        task?.cancel()
        let output = output
        task = Task.detached(priority: .utility) {
            var last: [String: AppObservation] = [:]
            let poll = Duration.milliseconds(Int(max(args.pollMs, 250)))
            while !Task.isCancelled {
                let now = await Self.observe(args, previous: last)
                for sig in SignalDiffer.diff(old: last, new: now) {
                    await output.emit(.detectSignal(sig))
                }
                last = now
                try? await Task.sleep(for: poll)
            }
        }
    }

    public func stop() {
        task?.cancel()
        task = nil
    }

    static func observe(_ args: DetectStartArgs, previous: [String: AppObservation]) async -> [String: AppObservation] {
        let rules = args.rules.filter { r in
            !DictationDenylist.isDenied(bundleId: r.bundleId, name: r.bundleId)
        }
        let known = rules.map(\.bundleId)
        let audio = LiveOwners.apps(known: known)
        var out: [String: AppObservation] = [:]
        for rule in rules {
            let running = NSRunningApplication.runningApplications(withBundleIdentifier: rule.bundleId).first { !$0.isTerminated }
            let a = audio[rule.bundleId]
            guard let pid = running?.processIdentifier ?? a?.mainPid else { continue }
            let name = running?.localizedName ?? a?.name ?? rule.bundleId
            if DictationDenylist.isDenied(bundleId: rule.bundleId, name: name) { continue }
            var obs = AppObservation(
                app: AppInfoWire(pid: pid, bundleId: rule.bundleId, name: name, audioPids: a?.audioPids ?? [pid]),
                micInUse: a?.micInUse ?? false,
                meetingWindow: nil,
                hasMeetingWindow: false,
                meetTab: nil
            )
            let prev = previous[rule.bundleId]
            if rule.isBrowser {
                if obs.micInUse || prev?.meetTab != nil {
                    if let tabs = await BrowserTabs.tabs(bundleId: rule.bundleId) {
                        obs.meetTab = tabs.first { MeetUrl.isMeeting($0.url, patterns: args.meetUrlPatterns) }
                    } else {
                        obs.meetTab = prev?.meetTab // error puntual: no inventar un cierre
                    }
                }
            } else if obs.micInUse || prev?.hasMeetingWindow == true, AXSnapshot.isTrusted() {
                let reading = TeamsAX.readLive(pid: pid)
                obs.hasMeetingWindow = reading.isMeeting
                obs.meetingWindow = reading.meetingWindowTitle
            }
            out[rule.bundleId] = obs
        }
        return out
    }
}
