// SPDX-License-Identifier: Apache-2.0
import CoreVideo
import Foundation
import Metal
import QuartzCore
import SkylatchLogic
import simd

/// Draws NV12 pixel buffers straight from VideoToolbox's IOSurfaces onto a CAMetalLayer.
final class MetalRenderer {
    private struct ColorUniforms {
        var matrix: simd_float3x3
        var uvOrigin: SIMD2<Float>
        var uvScale: SIMD2<Float>
    }

    static let shaderSource = """
    #include <metal_stdlib>
    using namespace metal;

    struct Uniforms {
        float3x3 matrix;     // (Y, Cb, Cr) after range expansion -> RGB
        float2 uvOrigin;
        float2 uvScale;
    };
    struct VertexOut {
        float4 position [[position]];
        float2 uv;
    };

    vertex VertexOut skylatch_vertex(uint vid [[vertex_id]], constant Uniforms &u [[buffer(0)]]) {
        // Triangle strip: (-1,1) (1,1) (-1,-1) (1,-1)
        float2 p = float2((vid & 1) ? 1.0 : -1.0, (vid & 2) ? -1.0 : 1.0);
        VertexOut out;
        out.position = float4(p, 0.0, 1.0);
        out.uv = u.uvOrigin + float2((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5) * u.uvScale;
        return out;
    }

    fragment float4 skylatch_fragment(VertexOut in [[stage_in]],
                                      texture2d<float> yTexture [[texture(0)]],
                                      texture2d<float> cbcrTexture [[texture(1)]],
                                      constant Uniforms &u [[buffer(0)]]) {
        constexpr sampler s(filter::linear, address::clamp_to_edge);
        float y = yTexture.sample(s, in.uv).r;
        float2 cbcr = cbcrTexture.sample(s, in.uv).rg;
        // Video range: Y 16..235, Cb/Cr 16..240 (centered on 128).
        float3 yuv = float3((y - 16.0 / 255.0) * (255.0 / 219.0),
                            (cbcr.x - 128.0 / 255.0) * (255.0 / 224.0),
                            (cbcr.y - 128.0 / 255.0) * (255.0 / 224.0));
        return float4(saturate(u.matrix * yuv), 1.0);
    }
    """

    // Columns map Y, Cb and Cr to RGB.
    private static let bt709 = simd_float3x3(columns: (SIMD3(1, 1, 1), SIMD3(0, -0.1873, 1.8556), SIMD3(1.5748, -0.4681, 0)))
    private static let bt601 = simd_float3x3(columns: (SIMD3(1, 1, 1), SIMD3(0, -0.3441, 1.7720), SIMD3(1.4020, -0.7141, 0)))

    let layer: CAMetalLayer
    private let device: MTLDevice
    private let queue: MTLCommandQueue
    private let pipeline: MTLRenderPipelineState
    private var textureCache: CVMetalTextureCache?
    private let drawLock = NSLock()
    private var lastBuffer: CVPixelBuffer?
    private var drawCount = 0
    /// When set, the drawable size follows the video (hidden smoke rendering has no view to size it).
    private let followsVideoSize: Bool

    init(layer: CAMetalLayer, followsVideoSize: Bool) throws {
        guard let device = MTLCreateSystemDefaultDevice() else { throw DecodeError("no Metal device is available") }
        guard let queue = device.makeCommandQueue() else { throw DecodeError("could not create a Metal command queue") }
        let library: MTLLibrary
        do { library = try device.makeLibrary(source: Self.shaderSource, options: nil) }
        catch { throw DecodeError("could not compile the Metal shaders: \(error.localizedDescription)") }
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.vertexFunction = library.makeFunction(name: "skylatch_vertex")
        descriptor.fragmentFunction = library.makeFunction(name: "skylatch_fragment")
        descriptor.colorAttachments[0].pixelFormat = .bgra8Unorm
        do { pipeline = try device.makeRenderPipelineState(descriptor: descriptor) }
        catch { throw DecodeError("could not create the Metal pipeline: \(error.localizedDescription)") }
        guard CVMetalTextureCacheCreate(kCFAllocatorDefault, nil, device, nil, &textureCache) == kCVReturnSuccess else {
            throw DecodeError("could not create the CVMetalTextureCache")
        }
        self.device = device
        self.queue = queue
        self.layer = layer
        self.followsVideoSize = followsVideoSize
        layer.device = device
        layer.pixelFormat = .bgra8Unorm
        layer.framebufferOnly = true
        layer.maximumDrawableCount = 3
        layer.displaySyncEnabled = false
        layer.isOpaque = true
        layer.colorspace = CGColorSpace(name: CGColorSpace.sRGB)
        layer.backgroundColor = CGColor(gray: 0, alpha: 1)
    }

    /// Size of the most recent picture, in pixels (zero until one has been drawn).
    var videoSize: (width: Int, height: Int) {
        drawLock.lock()
        defer { drawLock.unlock() }
        guard let buffer = lastBuffer else { return (0, 0) }
        let clean = CVImageBufferGetCleanRect(buffer)
        return (Int(clean.width), Int(clean.height))
    }

    /// Draws `buffer` letterboxed on the layer. Returns false if no drawable was available.
    @discardableResult
    func draw(_ buffer: CVPixelBuffer) -> Bool {
        drawLock.lock()
        defer { drawLock.unlock() }
        lastBuffer = buffer
        return render(buffer)
    }

    /// Redraws the last picture, e.g. after the view resized while the host sends no new frames.
    func redraw() {
        drawLock.lock()
        defer { drawLock.unlock() }
        if let buffer = lastBuffer { _ = render(buffer) }
    }

    private func render(_ buffer: CVPixelBuffer) -> Bool {
        guard let cache = textureCache, CVPixelBufferGetPlaneCount(buffer) == 2 else { return false }
        let clean = CVImageBufferGetCleanRect(buffer)
        let fullWidth = CVPixelBufferGetWidth(buffer), fullHeight = CVPixelBufferGetHeight(buffer)
        if followsVideoSize {
            let size = CGSize(width: clean.width, height: clean.height)
            if layer.drawableSize != size { layer.drawableSize = size }
        }

        var yRef: CVMetalTexture?, cRef: CVMetalTexture?
        guard CVMetalTextureCacheCreateTextureFromImage(
                kCFAllocatorDefault, cache, buffer, nil, .r8Unorm, fullWidth, fullHeight, 0, &yRef) == kCVReturnSuccess,
              CVMetalTextureCacheCreateTextureFromImage(
                kCFAllocatorDefault, cache, buffer, nil, .rg8Unorm,
                CVPixelBufferGetWidthOfPlane(buffer, 1), CVPixelBufferGetHeightOfPlane(buffer, 1), 1, &cRef) == kCVReturnSuccess,
              let yRef, let cRef, let yTexture = CVMetalTextureGetTexture(yRef),
              let cTexture = CVMetalTextureGetTexture(cRef) else { return false }

        guard let drawable = layer.nextDrawable(), let commands = queue.makeCommandBuffer() else { return false }
        let pass = MTLRenderPassDescriptor()
        pass.colorAttachments[0].texture = drawable.texture
        pass.colorAttachments[0].loadAction = .clear
        pass.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 1)
        pass.colorAttachments[0].storeAction = .store
        guard let encoder = commands.makeRenderCommandEncoder(descriptor: pass) else { return false }

        let target = Geometry.letterbox(width: Double(drawable.texture.width), height: Double(drawable.texture.height),
                                        videoWidth: Double(clean.width), videoHeight: Double(clean.height))
        encoder.setViewport(MTLViewport(originX: target.x, originY: target.y, width: target.width,
                                        height: target.height, znear: 0, zfar: 1))
        var uniforms = ColorUniforms(
            matrix: Self.matrix(for: buffer),
            uvOrigin: SIMD2(Float(clean.origin.x) / Float(fullWidth), Float(clean.origin.y) / Float(fullHeight)),
            uvScale: SIMD2(Float(clean.width) / Float(fullWidth), Float(clean.height) / Float(fullHeight)))
        encoder.setRenderPipelineState(pipeline)
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<ColorUniforms>.stride, index: 0)
        encoder.setFragmentBytes(&uniforms, length: MemoryLayout<ColorUniforms>.stride, index: 0)
        encoder.setFragmentTexture(yTexture, index: 0)
        encoder.setFragmentTexture(cTexture, index: 1)
        encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
        encoder.endEncoding()
        commands.present(drawable)
        commands.commit()
        // Keep the CVMetalTextures alive until the GPU is done with them.
        commands.waitUntilCompleted()
        withExtendedLifetime((yRef, cRef)) {}
        drawCount += 1
        if drawCount % 120 == 0 { CVMetalTextureCacheFlush(cache, 0) }
        return commands.status == .completed
    }

    private static func matrix(for buffer: CVPixelBuffer) -> simd_float3x3 {
        if let attachment = CVBufferCopyAttachment(buffer, kCVImageBufferYCbCrMatrixKey, nil) {
            let name = attachment as! CFString
            if CFEqual(name, kCVImageBufferYCbCrMatrix_ITU_R_601_4) { return bt601 }
            if CFEqual(name, kCVImageBufferYCbCrMatrix_ITU_R_709_2) { return bt709 }
        }
        // No tag: the host encodes BT.709, but standard-definition video conventionally means BT.601.
        return CVPixelBufferGetHeight(buffer) < 720 ? bt601 : bt709
    }
}
