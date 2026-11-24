// SPDX-License-Identifier: Apache-2.0
#pragma once
#include "common.hpp"
#include <d3d11.h>
#include <d3d10.h>
#include <dxgi1_2.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <wmcodecdsp.h>
#include <codecapi.h>
#include <wrl/client.h>
#include <cstdio>
#include <cstring>
#include <deque>

namespace skylatch {
using Microsoft::WRL::ComPtr;
inline void hrcheck(HRESULT hr, const char* where) {
    if (FAILED(hr)) {
        char hex[12]; std::snprintf(hex, sizeof hex, "0x%08lX", static_cast<unsigned long>(hr));
        throw std::runtime_error(std::string(where) + " (" + hex + ")");
    }
}
struct ComRuntime {
    ComRuntime() { hrcheck(CoInitializeEx(nullptr, COINIT_MULTITHREADED), "CoInitializeEx"); }
    ~ComRuntime() { CoUninitialize(); }
};
struct MfRuntime {
    MfRuntime() { hrcheck(MFStartup(MF_VERSION, MFSTARTUP_LITE), "MFStartup"); }
    ~MfRuntime() { MFShutdown(); }
};
inline RECT letterbox(LONG w, LONG h, unsigned vw, unsigned vh) {
    if (!vw || !vh || w <= 0 || h <= 0) return {};
    LONG ww = w, hh = h;
    if (int64_t(w) * vh > int64_t(h) * vw) ww = std::max<LONG>(1, LONG(int64_t(h) * vw / vh));
    else hh = std::max<LONG>(1, LONG(int64_t(w) * vh / vw));
    LONG x = (w - ww) / 2, y = (h - hh) / 2;
    return {x, y, x + ww, y + hh};
}
inline uint16_t normalize(LONG p, LONG start, LONG end) {
    if (end - start <= 1) return 0;
    return uint16_t(int64_t(std::clamp(p, start, end - 1) - start) * UINT16_MAX / (end - start - 1));
}
struct FrameInfo { uint32_t generation, seq; unsigned width, height; Clock::time_point arrival; };
struct VideoFormat { unsigned width{}, height{}; LONG stride{}; RECT aperture{}; bool limited{true}, bt709{true}; };

class Renderer {
    HWND hwnd;
    ComPtr<ID3D11DeviceContext> context;
    ComPtr<IDXGISwapChain1> swap;
    ComPtr<ID3D11VideoDevice> video;
    ComPtr<ID3D11VideoContext> vctx;
    ComPtr<ID3D11VideoProcessorEnumerator> enumerator;
    ComPtr<ID3D11VideoProcessor> processor;
    ComPtr<ID3D11VideoProcessorOutputView> target;
    ComPtr<ID3D11Texture2D> upload;
    UINT outW{}, outH{}, inW{}, inH{}, uploadW{}, uploadH{};
    void setup(UINT tw, UINT th, UINT w, UINT h) {
        const bool resize = w != outW || h != outH;
        if (resize) {
            target.Reset();
            hrcheck(swap->ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, 0), "ResizeBuffers");
            outW = w; outH = h;
        }
        if (!processor || resize || tw != inW || th != inH) {
            target.Reset(); processor.Reset(); enumerator.Reset(); inW = tw; inH = th;
            D3D11_VIDEO_PROCESSOR_CONTENT_DESC d{};
            d.InputFrameFormat = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
            d.InputFrameRate = {60, 1}; d.InputWidth = tw; d.InputHeight = th;
            d.OutputFrameRate = {60, 1}; d.OutputWidth = w; d.OutputHeight = h;
            d.Usage = D3D11_VIDEO_USAGE_PLAYBACK_NORMAL;
            hrcheck(video->CreateVideoProcessorEnumerator(&d, &enumerator), "CreateVideoProcessorEnumerator");
            UINT f{};
            hrcheck(enumerator->CheckVideoProcessorFormat(DXGI_FORMAT_NV12, &f), "CheckVideoProcessorFormat(NV12)");
            require((f & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT) != 0, "D3D11 video processor cannot read NV12");
            hrcheck(enumerator->CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM, &f), "CheckVideoProcessorFormat(BGRA)");
            require((f & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT) != 0, "D3D11 video processor cannot write BGRA");
            hrcheck(video->CreateVideoProcessor(enumerator.Get(), 0, &processor), "CreateVideoProcessor");
            ComPtr<ID3D11Texture2D> back;
            hrcheck(swap->GetBuffer(0, IID_PPV_ARGS(&back)), "swap chain GetBuffer");
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC ov{}; ov.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
            hrcheck(video->CreateVideoProcessorOutputView(back.Get(), enumerator.Get(), &ov, &target), "CreateVideoProcessorOutputView");
            vctx->VideoProcessorSetStreamFrameFormat(processor.Get(), 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vctx->VideoProcessorSetStreamAutoProcessingMode(processor.Get(), 0, FALSE);
            D3D11_VIDEO_COLOR black{}; black.RGBA.A = 1.0f;
            vctx->VideoProcessorSetOutputBackgroundColor(processor.Get(), FALSE, &black);
            D3D11_VIDEO_PROCESSOR_COLOR_SPACE cs{}; cs.RGB_Range = 0; cs.Nominal_Range = D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255;
            vctx->VideoProcessorSetOutputColorSpace(processor.Get(), &cs);
        }
    }
    void uploadCpu(IMFMediaBuffer* buffer, const VideoFormat& fmt) {
        const UINT w = (fmt.width + 1) & ~1u, h = (fmt.height + 1) & ~1u;
        if (!upload || uploadW != w || uploadH != h) {
            upload.Reset(); D3D11_TEXTURE2D_DESC d{};
            d.Width = w; d.Height = h; d.MipLevels = 1; d.ArraySize = 1; d.Format = DXGI_FORMAT_NV12;
            d.SampleDesc.Count = 1; d.Usage = D3D11_USAGE_DEFAULT; d.BindFlags = D3D11_BIND_DECODER;
            hrcheck(device->CreateTexture2D(&d, nullptr, &upload), "CreateTexture2D(CPU NV12)");
            uploadW = w; uploadH = h;
        }
        ComPtr<IMF2DBuffer2> two;
        BYTE* pixels = nullptr; LONG pitch = fmt.stride; DWORD length = 0;
        if (SUCCEEDED(buffer->QueryInterface(IID_PPV_ARGS(&two)))) {
            BYTE* start{};
            hrcheck(two->Lock2DSize(MF2DBuffer_LockFlags_Read, &pixels, &pitch, &start, &length), "NV12 Lock2DSize");
            struct Unlock { IMF2DBuffer2* b; ~Unlock() { b->Unlock2D(); } } unlock{two.Get()};
            require(pixels >= start && size_t(pixels - start) <= length && pitch >= LONG(w) &&
                    size_t(pitch) * (h + h / 2) <= length - size_t(pixels - start), "invalid 2D NV12 layout");
            context->UpdateSubresource(upload.Get(), 0, nullptr, pixels, UINT(pitch), 0);
        } else {
            hrcheck(buffer->Lock(&pixels, nullptr, &length), "NV12 Lock");
            struct Unlock { IMFMediaBuffer* b; ~Unlock() { b->Unlock(); } } unlock{buffer};
            if (pitch < LONG(w) || size_t(pitch) * (h + h / 2) > length) pitch = LONG(w);
            require(size_t(pitch) * (h + h / 2) <= length, "truncated NV12 buffer");
            context->UpdateSubresource(upload.Get(), 0, nullptr, pixels, UINT(pitch), 0);
        }
    }
public:
    ComPtr<ID3D11Device> device;
    explicit Renderer(HWND window) : hwnd(window) {
        constexpr D3D_FEATURE_LEVEL levels[]{D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0};
        hrcheck(D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr,
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            levels, UINT(std::size(levels)), D3D11_SDK_VERSION, &device, nullptr, &context), "D3D11CreateDevice");
        ComPtr<ID3D10Multithread> mt; hrcheck(context.As(&mt), "ID3D10Multithread"); mt->SetMultithreadProtected(TRUE);
        hrcheck(device.As(&video), "ID3D11VideoDevice"); hrcheck(context.As(&vctx), "ID3D11VideoContext");
        ComPtr<IDXGIDevice> dxgi; ComPtr<IDXGIAdapter> adapter; ComPtr<IDXGIFactory2> factory;
        hrcheck(device.As(&dxgi), "IDXGIDevice"); hrcheck(dxgi->GetAdapter(&adapter), "GetAdapter");
        hrcheck(adapter->GetParent(IID_PPV_ARGS(&factory)), "IDXGIFactory2");
        RECT rc{}; GetClientRect(hwnd, &rc);
        DXGI_SWAP_CHAIN_DESC1 d{}; d.Width = outW = UINT(std::max<LONG>(1, rc.right)); d.Height = outH = UINT(std::max<LONG>(1, rc.bottom));
        d.Format = DXGI_FORMAT_B8G8R8A8_UNORM; d.SampleDesc.Count = 1; d.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
        d.BufferCount = 2; d.Scaling = DXGI_SCALING_STRETCH; d.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD; d.AlphaMode = DXGI_ALPHA_MODE_IGNORE;
        hrcheck(factory->CreateSwapChainForHwnd(device.Get(), hwnd, &d, nullptr, nullptr, &swap), "CreateSwapChainForHwnd");
        factory->MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
        ComPtr<IDXGIDevice1> dxgi1; if (SUCCEEDED(dxgi.As(&dxgi1))) dxgi1->SetMaximumFrameLatency(1);
    }
    struct Result { bool presented, dxgiBacked; };
    Result draw(IMFSample* sample, const VideoFormat& fmt, const FrameInfo& frame) {
        ComPtr<IMFMediaBuffer> buffer;
        hrcheck(sample->GetBufferByIndex(0, &buffer), "decoded sample GetBufferByIndex");
        ComPtr<IMFDXGIBuffer> db;
        ComPtr<ID3D11Texture2D> decoded;
        UINT sub = 0;
        const bool gpu = SUCCEEDED(buffer.As(&db));
        RECT rc{}; GetClientRect(hwnd, &rc);
        if (rc.right <= 0 || rc.bottom <= 0 || IsIconic(hwnd)) return {false, gpu};
        if (gpu) {
            hrcheck(db->GetResource(IID_PPV_ARGS(&decoded)), "IMFDXGIBuffer::GetResource");
            hrcheck(db->GetSubresourceIndex(&sub), "IMFDXGIBuffer::GetSubresourceIndex");
        } else {
            ComPtr<IMFMediaBuffer> contiguous;
            hrcheck(sample->ConvertToContiguousBuffer(&contiguous), "NV12 contiguous buffer");
            uploadCpu(contiguous.Get(), fmt); decoded = upload;
        }
        D3D11_TEXTURE2D_DESC d{}; decoded->GetDesc(&d);
        require(d.Format == DXGI_FORMAT_NV12 && d.MipLevels, "decoder did not return an NV12 texture");
        setup(d.Width, d.Height, UINT(rc.right), UINT(rc.bottom));
        D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC iv{}; iv.ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D;
        iv.Texture2D.MipSlice = sub % d.MipLevels; iv.Texture2D.ArraySlice = sub / d.MipLevels;
        ComPtr<ID3D11VideoProcessorInputView> source;
        hrcheck(video->CreateVideoProcessorInputView(decoded.Get(), enumerator.Get(), &iv, &source), "CreateVideoProcessorInputView");
        RECT src = fmt.aperture;
        if (src.right <= src.left || src.bottom <= src.top || src.left < 0 || src.top < 0 ||
            src.right > LONG(d.Width) || src.bottom > LONG(d.Height)) {
            src = {0, 0, LONG(std::min(d.Width, frame.width)), LONG(std::min(d.Height, frame.height))};
        }
        const RECT dst = letterbox(rc.right, rc.bottom, frame.width, frame.height);
        D3D11_VIDEO_PROCESSOR_COLOR_SPACE cs{};
        cs.YCbCr_Matrix = fmt.bt709 ? 1 : 0;
        cs.Nominal_Range = fmt.limited ? D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235 : D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_0_255;
        vctx->VideoProcessorSetStreamColorSpace(processor.Get(), 0, &cs);
        vctx->VideoProcessorSetOutputTargetRect(processor.Get(), TRUE, &rc);
        vctx->VideoProcessorSetStreamSourceRect(processor.Get(), 0, TRUE, &src);
        vctx->VideoProcessorSetStreamDestRect(processor.Get(), 0, TRUE, &dst);
        D3D11_VIDEO_PROCESSOR_STREAM stream{}; stream.Enable = TRUE; stream.pInputSurface = source.Get();
        hrcheck(vctx->VideoProcessorBlt(processor.Get(), target.Get(), 0, 1, &stream), "VideoProcessorBlt");
        const HRESULT p = swap->Present(0, 0);
        hrcheck(p, "Present(0, 0)");
        return {p != DXGI_STATUS_OCCLUDED, gpu};
    }
};

class Decoder {
    ComPtr<IMFDXGIDeviceManager> manager;
    ComPtr<IMFTransform> mft;
    ComPtr<ICodecAPI> codec;
    VideoFormat fmt{};
    bool typed = false, haveOutput = false, started = false, d3dAware = false;
    LONGLONG nextTime = 0;
    struct Pending { LONGLONG time; FrameInfo frame; Clock::time_point feed; };
    std::deque<Pending> pending;
    HRESULT lowLatencyResult = E_NOINTERFACE;

    bool selectOutput() {
        for (DWORD i = 0;; ++i) {
            ComPtr<IMFMediaType> t; const HRESULT hr = mft->GetOutputAvailableType(0, i, &t);
            if (hr == MF_E_NO_MORE_TYPES || hr == MF_E_TRANSFORM_TYPE_NOT_SET) return false;
            hrcheck(hr, "GetOutputAvailableType");
            GUID subtype{};
            if (FAILED(t->GetGUID(MF_MT_SUBTYPE, &subtype)) || subtype != MFVideoFormat_NV12) continue;
            if (FAILED(mft->SetOutputType(0, t.Get(), 0))) continue;
            hrcheck(MFGetAttributeSize(t.Get(), MF_MT_FRAME_SIZE, &fmt.width, &fmt.height), "NV12 frame size");
            require(fmt.width && fmt.height, "invalid NV12 frame size");
            UINT32 stride{};
            fmt.stride = SUCCEEDED(t->GetUINT32(MF_MT_DEFAULT_STRIDE, &stride)) ? LONG(stride) : LONG(fmt.width);
            UINT32 attr = 0;
            fmt.limited = FAILED(t->GetUINT32(MF_MT_VIDEO_NOMINAL_RANGE, &attr)) || attr != MFNominalRange_0_255;
            fmt.bt709 = FAILED(t->GetUINT32(MF_MT_YUV_MATRIX, &attr)) ? fmt.height >= 720 : attr != MFVideoTransferMatrix_BT601;
            MFVideoArea aperture{}; fmt.aperture = {};
            if (SUCCEEDED(t->GetBlob(MF_MT_MINIMUM_DISPLAY_APERTURE, reinterpret_cast<UINT8*>(&aperture), sizeof aperture, nullptr)) ||
                SUCCEEDED(t->GetBlob(MF_MT_GEOMETRIC_APERTURE, reinterpret_cast<UINT8*>(&aperture), sizeof aperture, nullptr))) {
                fmt.aperture = {aperture.OffsetX.value, aperture.OffsetY.value,
                    aperture.OffsetX.value + aperture.Area.cx, aperture.OffsetY.value + aperture.Area.cy};
            }
            haveOutput = true; return true;
        }
    }
    template <typename Output, typename Drop> void pump(Output&& output, Drop&& drop) {
        if (!haveOutput && !selectOutput()) return;
        for (unsigned loops = 0; loops < 2048; ++loops) {
            MFT_OUTPUT_STREAM_INFO info{}; hrcheck(mft->GetOutputStreamInfo(0, &info), "GetOutputStreamInfo");
            ComPtr<IMFSample> work;
            if (!(info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES))) {
                ComPtr<IMFMediaBuffer> buf;
                const DWORD size = std::max(info.cbSize, DWORD(((fmt.width + 1) & ~1u) * ((fmt.height + 1) & ~1u) * 3 / 2));
                if (info.cbAlignment > 1) hrcheck(MFCreateAlignedMemoryBuffer(size, info.cbAlignment - 1, &buf), "MFT aligned output buffer");
                else hrcheck(MFCreateMemoryBuffer(size, &buf), "MFT output buffer");
                hrcheck(MFCreateSample(&work), "MFCreateSample(output)"); hrcheck(work->AddBuffer(buf.Get()), "output AddBuffer");
            }
            MFT_OUTPUT_DATA_BUFFER out{}; out.pSample = work.Get(); DWORD status = 0;
            const HRESULT hr = mft->ProcessOutput(0, 1, &out, &status);
            const auto decodedAt = Clock::now();
            ComPtr<IMFCollection> events; events.Attach(out.pEvents);
            ComPtr<IMFSample> result = work;
            if (out.pSample != work.Get()) result.Attach(out.pSample);
            if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) return;
            if (hr == MF_E_TRANSFORM_STREAM_CHANGE) {
                haveOutput = false; require(selectOutput(), "MFT stream change did not offer NV12"); continue;
            }
            hrcheck(hr, "H.264 ProcessOutput");
            if ((out.dwStatus & MFT_OUTPUT_DATA_BUFFER_NO_SAMPLE) == MFT_OUTPUT_DATA_BUFFER_NO_SAMPLE || !result) continue;
            if (pending.empty()) continue;
            LONGLONG time{};
            if (SUCCEEDED(result->GetSampleTime(&time))) {
                auto found = std::find_if(pending.begin(), pending.end(), [=](const Pending& p) { return p.time == time; });
                if (found == pending.end()) continue;
                // The relay encoder streams in display order. ACK missing frames as drops before this one.
                const auto skip = found - pending.begin();
                for (std::ptrdiff_t i = 0; i < skip; ++i) { drop(pending.front().frame); pending.pop_front(); }
            }
            const auto p = pending.front(); pending.pop_front();
            output(result.Get(), fmt, p.frame, elapsedUs(p.feed, decodedAt));
        }
        throw std::runtime_error("H.264 MFT output did not drain");
    }
public:
    explicit Decoder(ID3D11Device* device, bool useDxva = true) {
        hrcheck(CoCreateInstance(CLSID_CMSH264DecoderMFT, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&mft)), "CoCreateInstance(CMSH264DecoderMFT)");
        ComPtr<IMFAttributes> attrs;
        if (SUCCEEDED(mft->GetAttributes(&attrs))) {
            attrs->SetUINT32(MF_LOW_LATENCY, TRUE);
            UINT32 aware = 0;
            d3dAware = SUCCEEDED(attrs->GetUINT32(MF_SA_D3D11_AWARE, &aware)) && aware;
        }
        if (SUCCEEDED(mft.As(&codec))) {
            VARIANT v{}; v.vt = VT_BOOL; v.boolVal = VARIANT_TRUE;
            lowLatencyResult = codec->SetValue(&CODECAPI_AVLowLatencyMode, &v);
            if (FAILED(lowLatencyResult)) {
                v = {}; v.vt = VT_UI4; v.ulVal = TRUE;
                lowLatencyResult = codec->SetValue(&CODECAPI_AVLowLatencyMode, &v);
            }
            v = {}; v.vt = VT_BOOL; v.boolVal = useDxva ? VARIANT_TRUE : VARIANT_FALSE;
            codec->SetValue(&CODECAPI_AVDecVideoAcceleration_H264, &v);
        }
        if (useDxva && d3dAware) {
            UINT reset{};
            hrcheck(MFCreateDXGIDeviceManager(&reset, &manager), "MFCreateDXGIDeviceManager");
            hrcheck(manager->ResetDevice(device, reset), "IMFDXGIDeviceManager::ResetDevice");
            const HRESULT hr = mft->ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, reinterpret_cast<ULONG_PTR>(manager.Get()));
            if (FAILED(hr)) manager.Reset();
        }
    }
    ~Decoder() {
        if (started) {
            mft->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            mft->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            mft->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
    }
    const char* mode(bool gpu) const {
        if (codec && manager) {
            VARIANT v{};
            if (SUCCEEDED(codec->GetValue(&CODECAPI_AVDecVideoDXVAMode, &v))) {
                const auto mode = (v.vt == VT_UI4 || v.vt == VT_I4) ? v.ulVal : 0;
                VariantClear(&v);
                if (mode == eAVDecVideoDXVAMode_SW) return "SW";
                if (mode >= eAVDecVideoDXVAMode_MC && mode <= eAVDecVideoDXVAMode_VLD) return "HW";
            }
        }
        return gpu && manager ? "HW (DXGI surfaces)" : "SW";
    }
    template <typename Output, typename Drop> void feed(View data, const FrameInfo& frame, bool key, Output&& output, Drop&& drop) {
        const auto start = Clock::now();
        if (!typed) {
            ComPtr<IMFMediaType> t; hrcheck(MFCreateMediaType(&t), "MFCreateMediaType");
            hrcheck(t->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video), "H.264 major type");
            hrcheck(t->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_H264), "H.264 subtype");
            hrcheck(MFSetAttributeSize(t.Get(), MF_MT_FRAME_SIZE, frame.width, frame.height), "H.264 frame size");
            hrcheck(t->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive), "H.264 interlace mode");
            hrcheck(mft->SetInputType(0, t.Get(), 0), "MFT SetInputType(H.264)");
            require(selectOutput(), "H.264 MFT did not offer NV12"); typed = true;
            hrcheck(mft->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0), "MFT begin streaming"); started = true;
            hrcheck(mft->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0), "MFT start of stream");
        }
        ComPtr<IMFSample> sample; ComPtr<IMFMediaBuffer> buffer;
        hrcheck(MFCreateSample(&sample), "MFCreateSample(H.264)");
        hrcheck(MFCreateMemoryBuffer(DWORD(data.size()), &buffer), "MFCreateMemoryBuffer(H.264)");
        BYTE* p = nullptr; hrcheck(buffer->Lock(&p, nullptr, nullptr), "H.264 buffer Lock");
        memcpy(p, data.data(), data.size()); buffer->Unlock();
        hrcheck(buffer->SetCurrentLength(DWORD(data.size())), "H.264 buffer length");
        hrcheck(sample->AddBuffer(buffer.Get()), "H.264 sample AddBuffer");
        const LONGLONG time = nextTime; nextTime += 166667;
        hrcheck(sample->SetSampleTime(time), "SetSampleTime"); sample->SetSampleDuration(166667);
        if (key) sample->SetUINT32(MFSampleExtension_CleanPoint, TRUE);
        HRESULT hr = mft->ProcessInput(0, sample.Get(), 0);
        if (hr == MF_E_NOTACCEPTING) { pump(output, drop); hr = mft->ProcessInput(0, sample.Get(), 0); }
        hrcheck(hr, "H.264 ProcessInput");
        pending.push_back({time, frame, start});
        pump(output, drop);
    }
};
}
