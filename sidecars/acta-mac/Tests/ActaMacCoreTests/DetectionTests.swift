import ActaMacProtocol
import Foundation
import Testing
@testable import ActaMacCore

private func obs(_ bundle: String, mic: Bool = false, window: String? = nil, tab: BrowserTab? = nil, pids: [Int32] = [10]) -> AppObservation {
    AppObservation(
        app: AppInfoWire(pid: 10, bundleId: bundle, name: bundle, audioPids: pids),
        micInUse: mic, meetingWindow: window, hasMeetingWindow: window != nil, meetTab: tab
    )
}

@Test func `differ emits on transitions only`() {
    let t = "com.microsoft.teams2"
    var s = SignalDiffer.diff(old: [:], new: [t: obs(t, mic: true, window: "Daily | Microsoft Teams")])
    #expect(s.map(\.signal) == [.micInUse, .meetingWindowOpen])
    #expect(s[1].windowTitle == "Daily | Microsoft Teams")
    s = SignalDiffer.diff(old: [t: obs(t, mic: true, window: "x")], new: [t: obs(t, mic: true, window: "x")])
    #expect(s.isEmpty)
    s = SignalDiffer.diff(old: [t: obs(t, mic: true, window: "x")], new: [t: obs(t, mic: false)])
    #expect(s.map(\.signal) == [.micReleased, .meetingWindowClosed])
}

@Test func `app quitting releases everything`() {
    let c = "com.google.Chrome"
    let tab = BrowserTab(url: "https://meet.google.com/abc-defg-hij", title: "Meet")
    let s = SignalDiffer.diff(old: [c: obs(c, mic: true, tab: tab)], new: [:])
    #expect(s.map(\.signal) == [.micReleased, .meetTabClosed])
    #expect(s[1].url == tab.url)
}

@Test func `audio pid changes are reported`() {
    let t = "com.microsoft.teams2"
    let s = SignalDiffer.diff(old: [t: obs(t, mic: true, pids: [10])], new: [t: obs(t, mic: true, pids: [10, 11])])
    #expect(s.map(\.signal) == [.audioPidsChanged])
    #expect(s[0].app.audioPids == [10, 11])
}

@Test func `meet url requires a room code`() {
    let p = ["meet.google.com/"]
    #expect(MeetUrl.isMeeting("https://meet.google.com/abc-defg-hij", patterns: p))
    #expect(MeetUrl.isMeeting("https://meet.google.com/abc-defg-hij?authuser=1", patterns: p))
    #expect(!MeetUrl.isMeeting("https://meet.google.com/", patterns: p))
    #expect(!MeetUrl.isMeeting("https://meet.google.com/landing", patterns: p))
    #expect(!MeetUrl.isMeeting("https://www.google.com/search?q=meet", patterns: p))
    #expect(MeetUrl.isMeeting("https://teams.live.com/meet/123", patterns: ["teams.live.com/meet"]))
}

@Test func `browser tab output is parsed`() {
    let tabs = BrowserTabs.parse("https://a.com\tA\nhttps://meet.google.com/abc-defg-hij\tDaily – Meet\nmissing value\t\n")
    #expect(tabs.count == 2)
    #expect(tabs[1] == BrowserTab(url: "https://meet.google.com/abc-defg-hij", title: "Daily – Meet"))
}

@Test func `applescript never launches the browser and uses the right title property`() {
    let chrome = BrowserTabs.script(bundleId: "com.google.Chrome")
    #expect(chrome.contains("if application id \"com.google.Chrome\" is running then"))
    #expect(chrome.contains("(title of t)"))
    #expect(BrowserTabs.script(bundleId: "com.apple.Safari").contains("(name of t)"))
}

@Test func `applescript output comes back from osascript`() async {
    let out = await BrowserTabs.osascript("return \"https://a.com\" & (character id 9) & \"A\"", timeout: 5)
    #expect(BrowserTabs.parse(out ?? "") == [BrowserTab(url: "https://a.com", title: "A")])
}

@Test func `a browser that never answers is cut at the timeout`() async {
    let start = ContinuousClock.now
    let out = await BrowserTabs.osascript("delay 30", timeout: 0.5)
    #expect(out == nil)
    #expect(ContinuousClock.now - start < .seconds(5))
}

@Test func `helpers resolve to their app by bundle prefix`() {
    let known = ["com.google.Chrome", "com.microsoft.teams2"]
    #expect(AppResolution.ownerByPrefix("com.google.Chrome.helper.renderer", known: known) == "com.google.Chrome")
    #expect(AppResolution.ownerByPrefix("com.microsoft.teams2", known: known) == "com.microsoft.teams2")
    #expect(AppResolution.ownerByPrefix("com.google.ChromeX", known: known) == nil)
}

@Test func `processes are grouped by owner with mic and output flags`() {
    let procs = [
        AudioProcessInfo(objectId: 1, pid: 100, bundleId: "com.microsoft.teams2", runningInput: false, runningOutput: true),
        AudioProcessInfo(objectId: 2, pid: 101, bundleId: "com.microsoft.teams2.helper", runningInput: true, runningOutput: false),
        AudioProcessInfo(objectId: 3, pid: 200, bundleId: "com.typeless.app", runningInput: true, runningOutput: false),
    ]
    let apps = AppResolution.group(procs) { p in
        let b = AppResolution.ownerByPrefix(p.bundleId, known: ["com.microsoft.teams2"]) ?? p.bundleId
        return ProcessOwner(bundleId: b, pid: b == "com.microsoft.teams2" ? 100 : p.pid, name: b)
    }
    let teams = apps["com.microsoft.teams2"]
    #expect(teams?.audioPids == [100, 101])
    #expect(teams?.micInUse == true)
    #expect(teams?.outputActive == true)
    #expect(apps.count == 2)
}

@Test func `dictation apps are denylisted`() {
    #expect(DictationDenylist.isDenied(bundleId: "com.superduper.superwhisper", name: "superwhisper"))
    #expect(DictationDenylist.isDenied(bundleId: "com.electron.wispr-flow", name: "Wispr Flow"))
    #expect(DictationDenylist.isDenied(bundleId: "x", name: "Typeless"))
    #expect(DictationDenylist.isDenied(bundleId: "com.goodsnooze.MacWhisper", name: "MacWhisper"))
    #expect(!DictationDenylist.isDenied(bundleId: "com.microsoft.teams2", name: "Microsoft Teams"))
}
