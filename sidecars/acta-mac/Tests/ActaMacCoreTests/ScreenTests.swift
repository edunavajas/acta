import ActaMacProtocol
import CoreGraphics
import Foundation
import ImageIO
import Testing
@testable import ActaMacCore

private func gradient(reversed: Bool = false, width: Int = 64, height: Int = 48) -> CGImage {
    var px = [UInt8](repeating: 0, count: width * height)
    for y in 0..<height {
        for x in 0..<width {
            let v = UInt8(x * 255 / (width - 1))
            px[y * width + x] = reversed ? 255 - v : v
        }
    }
    let provider = CGDataProvider(data: Data(px) as CFData)!
    return CGImage(
        width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 8, bytesPerRow: width,
        space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.none.rawValue),
        provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent
    )!
}

@Test func `dhash of gray thumbnail sets bits where left is brighter`() {
    var px = [UInt8](repeating: 0, count: 72)
    for y in 0..<8 { for x in 0..<9 { px[y * 9 + x] = UInt8(200 - x * 10) } } // decreciente
    #expect(DHash.hash(gray9x8: px) == UInt64.max)
    #expect(DHash.hex(0xFF) == "00000000000000ff")
    #expect(DHash.distance(0b1010, 0b0110) == 2)
}

@Test func `dhash separates opposite images and matches identical ones`() throws {
    let a = try #require(DHash.hash(gradient()))
    let b = try #require(DHash.hash(gradient()))
    let c = try #require(DHash.hash(gradient(reversed: true)))
    #expect(DHash.distance(a, b) == 0)
    #expect(DHash.distance(a, c) > 40)
}

@Test func `slide change needs stability and a big enough difference`() {
    var d = SlideChangeDetector(threshold: 10, stableFrames: 2)
    let slide1: UInt64 = 0x0F0F_0F0F_0F0F_0F0F
    let slide2: UInt64 = 0xF0F0_F0F0_F0F0_F0F0
    do { let r = d.observe(slide1); #expect(!r, "first frame not yet stable") }
    do { let r = d.observe(slide1); #expect(r, "stable twice → first slide saved") }
    do { let r = d.observe(slide1); #expect(!r, "same slide never saved again") }
    do { let r = d.observe(slide1 ^ 0b11); #expect(!r, "tiny noise is not a new slide") }
    do { let r = d.observe(slide2); #expect(!r, "transition frame") }
    do { let r = d.observe(slide2); #expect(r, "new slide once stable") }
    d.reset()
    do { let r = d.observe(slide2); #expect(!r) }
    do { let r = d.observe(slide2); #expect(r, "after reset the current slide counts again") }
}

@Test func `frame ring keeps the window and finds the nearest frame`() {
    var r = FrameRing(seconds: 20)
    for t in stride(from: Int64(0), through: 30_000, by: 1000) {
        r.append(RingFrame(atUnixMs: t, jpeg: Data([1]), phash: UInt64(t), width: 1, height: 1))
    }
    #expect(r.frames.first?.atUnixMs == 10_000)
    #expect(r.nearest(14_400)?.atUnixMs == 14_000)
    #expect(r.nearest(31_000)?.atUnixMs == 30_000)
    #expect(r.nearest(-60_000) == nil)
}

@Test func `capture file names`() {
    #expect(CaptureFiles.name(atUnixMs: 1_700_000_000_123, reason: .slideChange) == "1700000000123-slide_change.jpg")
    #expect(CaptureFiles.name(atUnixMs: 5, reason: .candidate) == "5.jpg")
}

@Test func `capture write is atomic and returns the event`() throws {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent("acta-test-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: dir) }
    let jpeg = try #require(Jpeg.encode(gradient(), quality: 0.8))
    let frame = RingFrame(atUnixMs: 42, jpeg: jpeg, phash: 0xAB, width: 64, height: 48)
    let ev = try CaptureFiles.write(frame, reason: .keyword, dir: dir.path, keyword: "deadline")
    #expect(ev.path.hasSuffix("/42-keyword.jpg"))
    #expect(ev.phash == "00000000000000ab")
    #expect(ev.keyword == "deadline")
    #expect(FileManager.default.fileExists(atPath: ev.path))
    #expect(!FileManager.default.fileExists(atPath: ev.path + ".tmp"))
    let again = try CaptureFiles.write(frame, reason: .keyword, dir: dir.path, keyword: nil)
    #expect(again.path != ev.path, "never overwrites an existing capture")
}

// MARK: - Seguir la ventana entre pantallas (hasta 5 monitores, escalas distintas)

private func win(_ id: UInt32, _ title: String, _ w: Double, _ h: Double, scale: Double = 2) -> WindowCandidate {
    WindowCandidate(id: id, title: title, width: w, height: h, scale: scale)
}

@Test func `the shared content window wins over the bigger main Teams window`() {
    let wins = [
        win(1, "Chat | Microsoft Teams", 1600, 1000),
        win(2, "Daily del equipo | Microsoft Teams", 1200, 800),
        win(3, "Contenido compartido de Ana | Microsoft Teams", 1000, 700),
    ]
    #expect(WindowTracking.pick(wins, titleHint: "Daily del equipo")?.id == 3)
    #expect(WindowTracking.pick(Array(wins.prefix(2)), titleHint: "Daily del equipo")?.id == 2)
    #expect(WindowTracking.pick(Array(wins.prefix(2)), titleHint: nil)?.id == 1, "sin pista, la más grande")
    #expect(WindowTracking.pick([win(9, "x", 100, 80)], titleHint: nil) == nil, "demasiado pequeña")
}

@Test func `moving to a display with another scale or resizing reconfigures the stream`() {
    let a = WindowGeometry(id: 2, width: 1200, height: 800, scale: 2)
    #expect(WindowTracking.needsUpdate(current: nil, new: a))
    #expect(!WindowTracking.needsUpdate(current: a, new: a))
    #expect(!WindowTracking.needsUpdate(current: a, new: WindowGeometry(id: 2, width: 1210, height: 805, scale: 2)), "ruido de unos px")
    #expect(WindowTracking.needsUpdate(current: a, new: WindowGeometry(id: 2, width: 1200, height: 800, scale: 1)), "pantalla no Retina")
    #expect(WindowTracking.needsUpdate(current: a, new: WindowGeometry(id: 2, width: 1600, height: 1000, scale: 2)), "redimensionada")
    #expect(WindowTracking.needsUpdate(current: a, new: WindowGeometry(id: 3, width: 1200, height: 800, scale: 2)), "otra ventana")
}

@Test func `stream size keeps the aspect ratio under max width`() {
    let (w, h) = WindowTracking.outputSize(WindowGeometry(id: 1, width: 1440, height: 900, scale: 2), maxWidth: 1920)
    #expect(w == 1920 && h == 1200)
    let (w1, h1) = WindowTracking.outputSize(WindowGeometry(id: 1, width: 1000, height: 600, scale: 1), maxWidth: 1920)
    #expect(w1 == 1000 && h1 == 600)
}

// MARK: - Contenido estático (19-nov-2026: sin ninguna captura)

/// ScreenCaptureKit solo entrega frames `.complete` cuando cambia algo: con una
/// diapositiva quieta llega UNO y el detector, que pedía dos iguales, no
/// guardaba nunca. Un "sigue igual" cuenta como otro frame igual.
@Test func `a static slide is saved even if only one complete frame arrives`() {
    var d = SlideChangeDetector(threshold: 10, stableFrames: 2)
    do { let r = d.observeUnchanged(); #expect(!r, "nada que repetir todavía") }
    let slide: UInt64 = 0x0F0F_0F0F_0F0F_0F0F
    do { let r = d.observe(slide); #expect(!r) }
    do { let r = d.observeUnchanged(); #expect(r, "quieta un intervalo → guardada") }
    do { let r = d.observeUnchanged(); #expect(!r, "y solo una vez") }
}

@Test func `an unchanged frame is repeated once per frame interval without new frames`() {
    #expect(!StaticFrames.shouldRepeat(lastObservedMs: nil, nowMs: 10_000, fps: 1))
    #expect(!StaticFrames.shouldRepeat(lastObservedMs: 9_500, nowMs: 10_000, fps: 1), "aún dentro del intervalo")
    #expect(!StaticFrames.shouldRepeat(lastObservedMs: 8_900, nowMs: 10_000, fps: 1), "un frame que llega algo tarde no es «sin cambios»")
    #expect(StaticFrames.shouldRepeat(lastObservedMs: 8_400, nowMs: 10_000, fps: 1), "1,5 intervalos sin frames")
    #expect(StaticFrames.shouldRepeat(lastObservedMs: 9_200, nowMs: 10_000, fps: 2), "a 2 fps el intervalo es 500 ms")
}

// MARK: - Sin app: la pantalla principal

@Test func `without a meeting app the main display is captured`() {
    #expect(ScreenTarget(pid: 0) == .display)
    #expect(ScreenTarget(pid: -1) == .display)
    #expect(ScreenTarget(pid: 4242) == .window(pid: 4242))
}

@Test func `whole screen mode never saves automatic captures`() {
    #expect(!ScreenTarget.display.savesAutomaticCaptures(axSharing: nil))
    #expect(!ScreenTarget.display.savesAutomaticCaptures(axSharing: true), "ni aunque AX diga que comparte")
    #expect(ScreenTarget.window(pid: 42).savesAutomaticCaptures(axSharing: nil))
    #expect(ScreenTarget.window(pid: 42).savesAutomaticCaptures(axSharing: true))
    #expect(!ScreenTarget.window(pid: 42).savesAutomaticCaptures(axSharing: false), "AX dice que no comparte")
}

@Test func `acta itself is excluded from display captures`() {
    #expect(ScreenTarget.excludedPids(selfPid: 10, parentPid: 9) == [10, 9])
    #expect(ScreenTarget.excludedPids(selfPid: 10, parentPid: 1) == [10], "launchd no es acta")
}

// MARK: - Recortes del portapapeles

@Test func `only new non-private images on the clipboard are imported`() {
    #expect(ClipboardPolicy.imageType(in: ["public.png", "public.tiff"]) == "public.png")
    #expect(ClipboardPolicy.imageType(in: ["public.tiff"]) == "public.tiff")
    #expect(ClipboardPolicy.imageType(in: ["public.jpeg"]) == "public.jpeg")
    #expect(ClipboardPolicy.imageType(in: ["public.utf8-plain-text"]) == nil, "texto no")
    #expect(ClipboardPolicy.imageType(in: ["public.png", "org.nspasteboard.ConcealedType"]) == nil, "contraseñas y similares")
    #expect(ClipboardPolicy.imageType(in: ["public.png", "org.nspasteboard.TransientType"]) == nil)
    // Copiar un fichero en Finder trae su icono como TIFF: eso no es un recorte.
    #expect(ClipboardPolicy.imageType(in: ["public.file-url", "public.tiff"]) == nil)
    #expect(ClipboardPolicy.isNew(changeCount: 5, last: 4))
    #expect(!ClipboardPolicy.isNew(changeCount: 4, last: 4))
}

@Test func `a clipboard image is encoded as jpeg under max width`() throws {
    let png = NSMutableData()
    let dest = try #require(CGImageDestinationCreateWithData(png as CFMutableData, "public.png" as CFString, 1, nil))
    CGImageDestinationAddImage(dest, gradient(width: 400, height: 300), nil)
    #expect(CGImageDestinationFinalize(dest))
    let img = try #require(ClipboardImage.decode(png as Data, maxWidth: 200))
    #expect(img.width == 200 && img.height == 150)
    #expect(img.jpeg.starts(with: [0xFF, 0xD8]), "JPEG")
    let full = try #require(ClipboardImage.decode(png as Data, maxWidth: 1920))
    #expect(full.width == 400 && full.height == 300, "nunca se agranda")
    #expect(ClipboardImage.decode(Data([1, 2, 3]), maxWidth: 100) == nil)
}
