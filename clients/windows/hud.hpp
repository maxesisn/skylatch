// SPDX-License-Identifier: Apache-2.0
#pragma once
#include "common.hpp"
#include <d2d1_1.h>
#include <dwrite.h>
#include <wrl/client.h>
#include <cmath>
#include <cstdio>
#include <mutex>
#include <optional>

namespace skylatch {
using Microsoft::WRL::ComPtr;

// In-window viewer UI: a top-edge latch strip that expands into a control pill, short toasts, a draggable
// performance readout, and a disconnect / reconnect card. State and layout live here (UI thread writes,
// renderer thread reads under one lock); drawing goes through a Direct2D device context that targets the
// swap chain's own back buffer, so the HUD needs no extra window and cannot flicker or lose z-order.

struct HudText {
    const wchar_t *monitor, *capture, *swap, *fullscreen;
    const wchar_t *audio, *audioMuted, *audioOn, *audioOff;
    const wchar_t *captureOn, *captureOff, *fullscreenOn, *fullscreenOff, *swapOn, *swapOff;
    const wchar_t *lostTitle, *reconnectingTitle, *reconnect, *reconnecting, *close, *userDisconnected;
    const wchar_t *stats, *perfDecode, *perfWait, *perfRate, *perfAudio, *direct, *relay, *audioOffState, *noData;
};
inline const HudText& hudText() {
    static const HudText zh{L"屏 %u", L"系统键", L"Ctrl↔Cmd", L"全屏",
        L"音频", L"已静音", L"已开启远端音频", L"已静音远端音频",
        L"已捕获系统键，双按右 Ctrl 释放", L"已释放系统键，双按右 Ctrl 捕获",
        L"全屏已捕获系统键，长按 Esc 退出", L"已退出全屏", L"Ctrl ↔ Cmd 互换已开启", L"Ctrl ↔ Cmd 互换已关闭",
        L"与 %ls 的连接已中断", L"正在重新连接 %ls…", L"立即重连", L"重连中…", L"关闭窗口", L"你已主动断开连接",
        L"数据", L"解", L"等", L"码率", L"音", L"直连", L"中继", L"关闭", L"无数据"};
    static const HudText en{L"Screen %u", L"System keys", L"Ctrl↔Cmd", L"Fullscreen",
        L"Audio", L"Muted", L"Remote audio on", L"Remote audio muted",
        L"System keys captured. Double-tap Right Ctrl to release", L"System keys released. Double-tap Right Ctrl to capture",
        L"Fullscreen with system keys captured. Hold Esc to exit", L"Exited fullscreen", L"Ctrl ↔ Cmd swap on", L"Ctrl ↔ Cmd swap off",
        L"Connection to %ls was lost", L"Reconnecting to %ls…", L"Reconnect now", L"Reconnecting…", L"Close window", L"You disconnected",
        L"Stats", L"Dec", L"Wait", L"Rate", L"Audio", L"Direct", L"Relay", L"Off", L"No data"};
    static const bool chinese = PRIMARYLANGID(GetUserDefaultUILanguage()) == LANG_CHINESE;
    return chinese ? zh : en;
}

// The seven pill buttons are consecutive so a button index maps straight to its action.
enum class HudAction { None, Surface, Monitor, Audio, Capture, Swap, Fullscreen, Perf, Disconnect, Reconnect, Close, PerfPanel };
enum class HudOverlay { None, Lost, Reconnecting };

// The performance readout steps hidden -> card -> strip -> hidden.
enum class PerfMode { Hidden, Card, Strip };
enum class PerfPath { Direct, Relay, Unknown }; // Unknown: the core picked the path and does not report it.
enum class PerfAudio { Off, Muted, On };
// One refresh of the readout: averages over the last second of the session.
struct PerfReadout {
    unsigned width{}, height{};
    const wchar_t* decoder = L"";
    PerfPath path{PerfPath::Unknown};
    PerfAudio audio{PerfAudio::Off};
    double fps{}, mbps{};
    std::optional<double> decodeMs, waitMs, audioMs;
};

class Hud {
    struct Rect {
        float l{}, t{}, r{}, b{};
        bool has(float x, float y) const { return x >= l && x < r && y >= t && y < b; }
        float w() const { return r - l; }
        float h() const { return b - t; }
    };
    struct Layout {
        Rect trigger, strip, pill, logo, name, stat, sep, btn[7], card, icon, title, reason, cbtn[2], perf;
        bool vis[7]{}, showStat{};
    };
    struct View { UINT w{1}, h{1}, dpi{96}; };

    static constexpr float pillH = 40, pillTop = 8, speakerSize = 14, toastSeconds = 1.2f, collapseSeconds = 1.8f, expandSeconds = 0.18f;
    static constexpr float perfCardW = 214, perfRowH = 17, perfPadX = 9, perfPadY = 6, perfStripH = 25, perfDot = 6, perfDotGap = 6;

    static D2D1_COLOR_F hex(uint32_t rgb, float a = 1.f) {
        return {float((rgb >> 16) & 255) / 255.f, float((rgb >> 8) & 255) / 255.f, float(rgb & 255) / 255.f, a};
    }
    static float lerp(float a, float b, float t) { return a + (b - a) * t; }
    static D2D1_COLOR_F mix(D2D1_COLOR_F a, D2D1_COLOR_F b, float t) {
        return {lerp(a.r, b.r, t), lerp(a.g, b.g, t), lerp(a.b, b.b, t), lerp(a.a, b.a, t)};
    }
    static float ease(float t) { const float u = 1.f - std::clamp(t, 0.f, 1.f); return 1.f - u * u * u; }

    struct Painter {
        ID2D1DeviceContext* c;
        ID2D1SolidColorBrush* b;
        void fill(const Rect& r, float rad, D2D1_COLOR_F col) const {
            b->SetColor(col);
            if (rad > 0.01f) c->FillRoundedRectangle(D2D1::RoundedRect(D2D1::RectF(r.l, r.t, r.r, r.b), rad, rad), b);
            else c->FillRectangle(D2D1::RectF(r.l, r.t, r.r, r.b), b);
        }
        void stroke(const Rect& r, float rad, D2D1_COLOR_F col, float width) const {
            b->SetColor(col);
            const float h = width / 2;
            c->DrawRoundedRectangle(D2D1::RoundedRect(D2D1::RectF(r.l + h, r.t + h, r.r - h, r.b - h),
                std::max(0.f, rad - h), std::max(0.f, rad - h)), b, width);
        }
        void line(float x0, float y0, float x1, float y1, D2D1_COLOR_F col, float width) const {
            b->SetColor(col);
            c->DrawLine(D2D1::Point2F(x0, y0), D2D1::Point2F(x1, y1), b, width);
        }
        void dot(float x, float y, float rad, D2D1_COLOR_F col) const {
            b->SetColor(col);
            c->FillEllipse(D2D1::Ellipse(D2D1::Point2F(x, y), rad, rad), b);
        }
        void text(const std::wstring& s, IDWriteTextFormat* f, const Rect& r, D2D1_COLOR_F col) const {
            if (s.empty() || !f) return;
            b->SetColor(col);
            c->DrawText(s.c_str(), UINT32(s.size()), f, D2D1::RectF(r.l, r.t, r.r, r.b), b, D2D1_DRAW_TEXT_OPTIONS_CLIP);
        }
    };

    mutable std::mutex m;
    bool ready{};
    ComPtr<IDWriteFactory> dw;
    ComPtr<IDWriteTextFormat> fName, fStat, fBtn, fToast, fTitle, fBody, fCardBtn, fPerf, fPerfRight;
    ComPtr<IDWriteTypography> tabular;
    std::wstring device, stat, toastText, reason, label[7];
    unsigned monitorActive{};
    size_t monitorCount{};
    bool capture{}, swap{}, fullscreen{}, audioAvailable{}, audioOn{true};
    View view;
    float nameW{}, statW{}, toastW{}, labelW[7]{};
    bool metricsDirty{true};
    HudOverlay overlayState{HudOverlay::None};
    bool expandTarget{}, inside{};
    float prog{};
    std::optional<Clock::time_point> leaveAt, toastAt;
    Clock::time_point last{};
    float toastAlpha{};
    HudAction hover{HudAction::None}, pressed{HudAction::None};
    PerfMode perfMode{PerfMode::Hidden};
    PerfReadout perf;
    std::wstring perfRows[3][2], perfStrip;
    float perfStripW{}, perfX{14}, perfY{14}; // Where the user put the readout (DIPs); layout clamps it into view.
    std::optional<D2D1_POINT_2F> perfGrab;    // Pointer offset inside the readout while it is dragged.

    ComPtr<IDWriteTextFormat> makeFormat(float size, DWRITE_FONT_WEIGHT weight, DWRITE_TEXT_ALIGNMENT align,
        DWRITE_PARAGRAPH_ALIGNMENT para, bool wrap, bool ellipsis) {
        ComPtr<IDWriteTextFormat> f;
        require(SUCCEEDED(dw->CreateTextFormat(L"Segoe UI", nullptr, weight, DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL, size, L"en-us", &f)), "CreateTextFormat failed");
        f->SetTextAlignment(align);
        f->SetParagraphAlignment(para);
        f->SetWordWrapping(wrap ? DWRITE_WORD_WRAPPING_WRAP : DWRITE_WORD_WRAPPING_NO_WRAP);
        if (ellipsis) {
            ComPtr<IDWriteInlineObject> sign;
            if (SUCCEEDED(dw->CreateEllipsisTrimmingSign(f.Get(), &sign))) {
                const DWRITE_TRIMMING trim{DWRITE_TRIMMING_GRANULARITY_CHARACTER, 0, 0};
                f->SetTrimming(&trim, sign.Get());
            }
        }
        return f;
    }
    float measure(IDWriteTextFormat* f, const std::wstring& s) const {
        if (s.empty() || !f) return 0;
        ComPtr<IDWriteTextLayout> l;
        if (FAILED(dw->CreateTextLayout(s.c_str(), UINT32(s.size()), f, 2000.f, 100.f, &l))) return 0;
        DWRITE_TEXT_METRICS tm{};
        l->GetMetrics(&tm);
        return std::ceil(tm.widthIncludingTrailingWhitespace);
    }
    void refreshMetrics() {
        if (!metricsDirty) return;
        metricsDirty = false;
        const auto& t = hudText();
        wchar_t buf[64]{};
        swprintf_s(buf, t.monitor, monitorActive + 1);
        label[0] = buf; label[1] = audioOn ? t.audio : t.audioMuted; label[2] = t.capture; label[3] = t.swap;
        label[4] = t.fullscreen; label[5] = t.stats; label[6].clear();
        for (int i = 0; i < 6; ++i) labelW[i] = measure(fBtn.Get(), label[i]);
        labelW[1] = std::max(measure(fBtn.Get(), t.audio), measure(fBtn.Get(), t.audioMuted)) + speakerSize + 4;
        labelW[5] += speakerSize + 4;
        nameW = measure(fName.Get(), device);
        statW = measure(fStat.Get(), stat);
    }
    Layout layout() {
        refreshMetrics();
        const float s = float(view.dpi) / 96.f, W = float(view.w) / s, H = float(view.h) / s, cx = W / 2;
        Layout L;
        L.strip = {cx - 28, -2, cx + 28, 4};
        L.trigger = {cx - 120, 0, cx + 120, 14};

        constexpr float padX = 10, gap = 8, btnGap = 4, logoSize = 22;
        L.vis[0] = monitorCount > 1;
        L.vis[1] = audioAvailable;
        for (int i = 2; i < 7; ++i) L.vis[i] = true;
        const auto buttonW = [&](int i) { return i == 6 ? 30.f : labelW[i] + 22.f; };
        float buttons = 0;
        int shown = 0;
        for (int i = 0; i < 7; ++i) if (L.vis[i]) buttons += buttonW(i) + (shown++ ? btnGap : 0);
        float nW = std::min(nameW, 150.f);
        const auto total = [&](bool withStat, float n) {
            return padX + logoSize + gap + n + (withStat ? gap + statW : 0) + gap + 1 + gap + buttons + padX - 2;
        };
        bool withStat = statW > 0;
        const float avail = std::max(120.f, W - 16);
        if (total(withStat, nW) > avail) withStat = false;
        if (total(withStat, nW) > avail) nW = std::max(30.f, nW - (total(withStat, nW) - avail));
        const float pw = total(withStat, nW), pl = cx - pw / 2;
        L.pill = {pl, pillTop, pl + pw, pillTop + pillH};
        float x = pl + padX;
        L.logo = {x, pillTop + (pillH - logoSize) / 2, x + logoSize, pillTop + (pillH + logoSize) / 2};
        x += logoSize + gap;
        L.name = {x, pillTop, x + nW, pillTop + pillH};
        x += nW;
        if (withStat) { x += gap; L.stat = {x, pillTop, x + statW, pillTop + pillH}; x += statW; }
        L.showStat = withStat;
        x += gap;
        L.sep = {x, pillTop + 10, x + 1, pillTop + pillH - 10};
        x += 1 + gap;
        for (int i = 0; i < 7; ++i) {
            if (!L.vis[i]) continue;
            L.btn[i] = {x, pillTop + 6, x + buttonW(i), pillTop + pillH - 6};
            x += buttonW(i) + btnGap;
        }

        if (perfMode != PerfMode::Hidden) {
            const bool card = perfMode == PerfMode::Card;
            const float w = card ? perfCardW : perfPadX * 2 + perfDot + perfDotGap + perfStripW;
            const float h = card ? perfPadY * 2 + perfRowH * 3 : perfStripH;
            const float px = std::round(std::clamp(perfX, 0.f, std::max(0.f, W - w)));
            const float py = std::round(std::clamp(perfY, 0.f, std::max(0.f, H - h)));
            L.perf = {px, py, px + w, py + h};
        }

        const float cw = std::min(400.f, std::max(120.f, W - 32)), ch = 216;
        L.card = {cx - cw / 2, H / 2 - ch / 2, cx + cw / 2, H / 2 + ch / 2};
        L.icon = {cx - 20, L.card.t + 26, cx + 20, L.card.t + 66};
        L.title = {L.card.l + 20, L.card.t + 78, L.card.r - 20, L.card.t + 106};
        L.reason = {L.card.l + 24, L.card.t + 108, L.card.r - 24, L.card.t + 148};
        const float bw = (cw - 48 - 12) / 2, by = L.card.b - 24 - 36;
        L.cbtn[0] = {L.card.l + 24, by, L.card.l + 24 + bw, by + 36};
        L.cbtn[1] = {L.cbtn[0].r + 12, by, L.card.r - 24, by + 36};
        return L;
    }
    HudAction hitDip(const Layout& L, float x, float y) const {
        if (overlayState != HudOverlay::None) {
            if (overlayState == HudOverlay::Lost && L.cbtn[0].has(x, y)) return HudAction::Reconnect;
            if (L.cbtn[1].has(x, y)) return HudAction::Close;
            return HudAction::Surface;
        }
        if ((expandTarget || prog > 0.01f) && L.pill.has(x, y)) {
            for (int i = 0; i < 7; ++i)
                if (L.vis[i] && L.btn[i].has(x, y)) return HudAction(int(HudAction::Monitor) + i);
            return HudAction::Surface;
        }
        if (perfMode != PerfMode::Hidden && L.perf.has(x, y)) return HudAction::PerfPanel;
        return L.trigger.has(x, y) ? HudAction::Surface : HudAction::None;
    }
    HudAction hitPx(POINT p) {
        const float s = float(view.dpi) / 96.f;
        return hitDip(layout(), float(p.x) / s, float(p.y) / s);
    }

    void drawLogo(const Painter& p, const Rect& r, float a) const {
        const float u = r.w() / 64.f;
        const auto box = [&](float x0, float y0, float x1, float y1, float rad, D2D1_COLOR_F c) {
            p.fill({r.l + x0 * u, r.t + y0 * u, r.l + x1 * u, r.t + y1 * u}, rad * u, c);
        };
        const auto ink = hex(0x1A1A18, a), paper = hex(0xFCFBF8, a), cobalt = hex(0x2563EB, a);
        box(1.5f, 1.5f, 62.5f, 62.5f, 15.5f, ink);
        box(4.5f, 4.5f, 59.5f, 59.5f, 12.5f, paper);
        box(14, 14, 50, 50, 0, ink);
        box(18, 18, 46, 46, 0, paper);
        box(24, 29, 48, 35, 0, cobalt);
        box(20, 24, 24, 40, 0, ink);
    }
    void drawSpeaker(const Painter& p, float cx, float cy, bool muted, D2D1_COLOR_F col) const {
        constexpr float w = 1.4f, pi = 3.14159265f;
        const float x = cx - speakerSize / 2, y = cy - speakerSize / 2;
        const float body[7][2] = {{0, 5}, {3.5f, 5}, {7.5f, 1.5f}, {7.5f, 12.5f}, {3.5f, 9}, {0, 9}, {0, 5}};
        for (int i = 0; i < 6; ++i) p.line(x + body[i][0], y + body[i][1], x + body[i + 1][0], y + body[i + 1][1], col, w);
        if (muted) {
            p.line(x + 1.5f, y + 0.5f, x + 13, y + 13.5f, col, w);
            return;
        }
        for (const float radius : {3.6f, 6.4f}) {
            float px = 0, py = 0;
            for (int i = 0; i <= 6; ++i) {
                const float angle = (-45.f + 15.f * float(i)) * pi / 180.f;
                const float qx = x + 7.5f + radius * std::cos(angle), qy = y + 7 + radius * std::sin(angle);
                if (i) p.line(px, py, qx, qy, col, w);
                px = qx; py = qy;
            }
        }
    }
    // Line-chart glyph: a baseline and a rising polyline.
    void drawChart(const Painter& p, float cx, float cy, D2D1_COLOR_F col) const {
        constexpr float w = 1.4f;
        const float x = cx - speakerSize / 2, y = cy - speakerSize / 2;
        p.line(x + 0.5f, y + 1.5f, x + 0.5f, y + 13, col, w);
        p.line(x + 0.5f, y + 13, x + 13.5f, y + 13, col, w);
        const float pts[4][2] = {{3, 10}, {6, 6}, {8.5f, 8.5f}, {13, 3}};
        for (int i = 0; i < 3; ++i) p.line(x + pts[i][0], y + pts[i][1], x + pts[i + 1][0], y + pts[i + 1][1], col, w);
    }
    ComPtr<IDWriteTextLayout> perfLayout(const std::wstring& s, IDWriteTextFormat* f, float w, float h) const {
        ComPtr<IDWriteTextLayout> l;
        if (s.empty() || !f || FAILED(dw->CreateTextLayout(s.c_str(), UINT32(s.size()), f, w, h, &l))) return nullptr;
        if (tabular) l->SetTypography(tabular.Get(), {0, UINT32(s.size())});
        return l;
    }
    void perfText(const Painter& p, const std::wstring& s, IDWriteTextFormat* f, const Rect& r, D2D1_COLOR_F col) const {
        if (const auto l = perfLayout(s, f, r.w(), r.h())) {
            p.b->SetColor(col);
            p.c->DrawTextLayout(D2D1::Point2F(r.l, r.t), l.Get(), p.b, D2D1_DRAW_TEXT_OPTIONS_CLIP);
        }
    }
    template <typename... Args> static std::wstring format(const wchar_t* f, Args... args) {
        wchar_t buf[96]{};
        swprintf_s(buf, f, args...);
        return buf;
    }
    static std::wstring join(std::initializer_list<std::wstring> parts) {
        std::wstring out;
        for (const auto& part : parts) {
            if (part.empty()) continue;
            if (!out.empty()) out += L"\u2003";
            out += part;
        }
        return out;
    }
    void formatPerf() {
        const auto& t = hudText();
        const auto& r = perf;
        const auto ms = [](const std::optional<double>& v) { return v ? format(L"%.1fms", std::max(0.0, *v)) : std::wstring(L"--"); };
        const auto fps = format(L"%.0f FPS", std::max(0.0, r.fps));
        const auto res = r.width && r.height ? format(L"%u\u00D7%u", r.width, r.height) : std::wstring();
        const auto mbps = format(r.mbps >= 100 ? L"%.0f Mbps" : L"%.1f Mbps", std::max(0.0, r.mbps));
        const std::wstring path = r.path == PerfPath::Direct ? t.direct : r.path == PerfPath::Relay ? t.relay : L"";
        auto decoder = std::wstring(r.decoder);
        decoder += (decoder.empty() ? L"" : L" ") + ms(r.decodeMs);
        const std::wstring audio = r.audio == PerfAudio::Off ? std::wstring(t.audioOffState)
            : r.audio == PerfAudio::Muted ? std::wstring(t.audioMuted)
            : r.audioMs ? format(L"Opus %.0fms", std::max(0.0, *r.audioMs)) : std::wstring(t.noData);
        perfRows[0][0] = join({fps, res}); perfRows[0][1] = path;
        perfRows[1][0] = std::wstring(t.perfDecode) + L" " + decoder; perfRows[1][1] = std::wstring(t.perfWait) + L" " + ms(r.waitMs);
        perfRows[2][0] = std::wstring(t.perfRate) + L" " + mbps; perfRows[2][1] = std::wstring(t.perfAudio) + L" " + audio;
        perfStrip = join({fps, res, decoder, mbps, path});
        perfStripW = 0;
        if (const auto l = perfLayout(perfStrip, fPerf.Get(), 2000.f, 100.f)) {
            DWRITE_TEXT_METRICS tm{};
            l->GetMetrics(&tm);
            perfStripW = std::ceil(tm.widthIncludingTrailingWhitespace);
        }
    }
    void drawPerf(const Painter& p, const Layout& L) const {
        if (perfMode == PerfMode::Hidden) return;
        const Rect& r = L.perf;
        const bool lit = hover == HudAction::PerfPanel || perfGrab;
        p.fill(r, 7, hex(0x121212, lit ? .74f : .56f));
        p.stroke(r, 7, hex(0xFFFFFF, .11f), 1.f);
        const bool card = perfMode == PerfMode::Card;
        const float rowH = card ? perfRowH : r.h(), top = card ? r.t + perfPadY : r.t;
        p.dot(r.l + perfPadX + perfDot / 2, top + rowH / 2, perfDot / 2, perf.fps < 30 ? hex(0xF2C14E, .9f) : hex(0x6BCB8B, .9f));
        const float left = r.l + perfPadX + perfDot + perfDotGap, right = r.r - perfPadX;
        const auto ink = hex(0xEDEDED, .88f);
        if (!card) { perfText(p, perfStrip, fPerf.Get(), {left, r.t, right + perfPadX, r.b}, ink); return; }
        for (int i = 0; i < 3; ++i) {
            const Rect row{left, top + perfRowH * float(i), right, top + perfRowH * float(i + 1)};
            perfText(p, perfRows[i][0], fPerf.Get(), row, ink);
            perfText(p, perfRows[i][1], fPerfRight.Get(), row, ink);
        }
    }
    void drawPill(const Painter& p, const Layout& L) const {
        const float e = ease(prog);
        const auto stripColor = capture ? hex(0x2563EB, .96f) : hex(0x8E8C85, .6f);
        const Rect cur{lerp(L.strip.l, L.pill.l, e), lerp(L.strip.t, L.pill.t, e),
            lerp(L.strip.r, L.pill.r, e), lerp(L.strip.b, L.pill.b, e)};
        const float rad = lerp(2.f, pillH / 2, e);
        p.fill(cur, rad, mix(stripColor, hex(0x1A1A18, .92f), e));
        p.stroke(cur, rad, mix(hex(0x1A1A18, .45f), hex(0xFFFFFF, .16f), e), 1.f);
        if (e <= 0.55f) return;
        const float a = std::clamp((e - 0.55f) / 0.45f, 0.f, 1.f);
        drawLogo(p, L.logo, a);
        p.text(device, fName.Get(), L.name, hex(0xF4F2EC, a));
        if (L.showStat) p.text(stat, fStat.Get(), L.stat, hex(0xA9A69C, a));
        p.fill(L.sep, 0, hex(0xFFFFFF, .14f * a));
        for (int i = 0; i < 7; ++i) {
            if (!L.vis[i]) continue;
            const Rect& r = L.btn[i];
            const auto action = HudAction(int(HudAction::Monitor) + i);
            const bool hot = hover == action, down = hot && pressed == action;
            if (i == 6) {
                p.fill(r, 8, down ? hex(0xB91C1C, a) : hot ? hex(0xEF4444, a) : hex(0xDC2626, .85f * a));
                const float cx = (r.l + r.r) / 2, cy = (r.t + r.b) / 2;
                p.line(cx - 4.5f, cy - 4.5f, cx + 4.5f, cy + 4.5f, hex(0xFFFFFF, a), 1.7f);
                p.line(cx - 4.5f, cy + 4.5f, cx + 4.5f, cy - 4.5f, hex(0xFFFFFF, a), 1.7f);
                continue;
            }
            if (i == 1) {
                const auto amber = [&](float alpha) { return hex(0xFBBF24, alpha * a); };
                if (audioOn) {
                    p.fill(r, 8, hex(0xFFFFFF, (down ? .24f : hot ? .16f : .07f) * a));
                } else {
                    p.fill(r, 8, amber(down ? .34f : hot ? .28f : .2f));
                    p.stroke(r, 8, amber(.6f), 1.f);
                }
                const auto col = audioOn ? hex(0xF4F2EC, a) : amber(1.f);
                const float textW = labelW[1] - speakerSize - 4, content = speakerSize + 4 + textW, left = (r.l + r.r) / 2 - content / 2;
                drawSpeaker(p, left + speakerSize / 2, (r.t + r.b) / 2, !audioOn, col);
                p.text(label[1], fBtn.Get(), {left + speakerSize + 4, r.t, left + content, r.b}, col);
                continue;
            }
            if (i == 5) {
                const bool lit = perfMode != PerfMode::Hidden;
                p.fill(r, 8, hex(0xFFFFFF, ((down ? .24f : hot ? .16f : .07f) + (lit ? .06f : 0.f)) * a));
                if (lit) p.stroke(r, 8, hex(0xFFFFFF, .22f * a), 1.f);
                const auto col = hex(0xF4F2EC, a);
                const float content = labelW[5], left = (r.l + r.r) / 2 - content / 2;
                drawChart(p, left + speakerSize / 2, (r.t + r.b) / 2, col);
                p.text(label[5], fBtn.Get(), {left + speakerSize + 4, r.t, left + content, r.b}, col);
                continue;
            }
            const bool on = (i == 2 && capture) || (i == 3 && swap) || (i == 4 && fullscreen);
            p.fill(r, 8, on ? hex(down ? 0x1D4ED8 : hot ? 0x3B73F0 : 0x2563EB, a)
                            : hex(0xFFFFFF, (down ? .24f : hot ? .16f : .07f) * a));
            p.text(label[i], fBtn.Get(), r, on ? hex(0xFFFFFF, a) : hex(0xF4F2EC, a));
        }
    }
    void drawToast(const Painter& p, const Layout& L, float W) const {
        if (toastAlpha <= 0.01f || toastText.empty()) return;
        const float w = std::min(toastW + 32, std::max(80.f, W - 16)), top = prog > 0.5f ? pillTop + pillH + 8 : 14.f;
        const Rect r{W / 2 - w / 2, top, W / 2 + w / 2, top + 32};
        (void)L;
        p.fill({r.l - 1, r.t + 1, r.r + 1, r.b + 3}, 17, hex(0x000000, .18f * toastAlpha));
        p.fill(r, 16, hex(0x1A1A18, .92f * toastAlpha));
        p.stroke(r, 16, hex(0xFFFFFF, .14f * toastAlpha), 1.f);
        p.text(toastText, fToast.Get(), r, hex(0xF4F2EC, toastAlpha));
    }
    void drawCard(const Painter& p, const Layout& L, float W, float H) const {
        const auto& t = hudText();
        const bool lost = overlayState == HudOverlay::Lost;
        p.fill({0, 0, W, H}, 0, hex(0x000000, .5f));
        for (int i = 3; i >= 1; --i)
            p.fill({L.card.l - i * 2.f, L.card.t - i * 2.f + 4, L.card.r + i * 2.f, L.card.b + i * 2.f + 4}, 16 + i * 2.f, hex(0x000000, .07f));
        p.fill(L.card, 16, hex(0xFCFBF8));
        p.stroke(L.card, 16, hex(0x1A1A18, .14f), 1.f);
        const float cx = (L.icon.l + L.icon.r) / 2, cy = (L.icon.t + L.icon.b) / 2;
        p.dot(cx, cy, 20, lost ? hex(0xDC2626) : hex(0x2563EB));
        if (lost) {
            p.fill({cx - 1.7f, cy - 10, cx + 1.7f, cy + 3}, 1.7f, hex(0xFFFFFF));
            p.dot(cx, cy + 9, 2.1f, hex(0xFFFFFF));
        } else {
            for (int i = -1; i <= 1; ++i) p.dot(cx + i * 9.f, cy, 2.3f, hex(0xFFFFFF));
        }
        wchar_t buf[512]{};
        swprintf_s(buf, lost ? t.lostTitle : t.reconnectingTitle, device.c_str());
        p.text(buf, fTitle.Get(), L.title, hex(0x1A1A18));
        p.text(lost ? reason : std::wstring(), fBody.Get(), L.reason, hex(0x6B6A63));
        const bool hotR = lost && hover == HudAction::Reconnect, downR = hotR && pressed == HudAction::Reconnect;
        p.fill(L.cbtn[0], 10, !lost ? hex(0xD8D6CE) : hex(downR ? 0x1E40AF : hotR ? 0x1D4ED8 : 0x2563EB));
        p.text(lost ? t.reconnect : t.reconnecting, fCardBtn.Get(), L.cbtn[0], !lost ? hex(0x8C8A82) : hex(0xFFFFFF));
        const bool hotC = hover == HudAction::Close, downC = hotC && pressed == HudAction::Close;
        if (hotC) p.fill(L.cbtn[1], 10, hex(0x1A1A18, downC ? .12f : .06f));
        p.stroke(L.cbtn[1], 10, hex(0x1A1A18, .28f), 1.f);
        p.text(t.close, fCardBtn.Get(), L.cbtn[1], hex(0x1A1A18));
    }

public:
    struct Update { bool repaint; UINT nextMs; };

    Hud() {
        try {
            require(SUCCEEDED(DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED, __uuidof(IDWriteFactory),
                reinterpret_cast<IUnknown**>(dw.GetAddressOf()))), "DWriteCreateFactory failed");
            const auto C = DWRITE_TEXT_ALIGNMENT_CENTER, Lead = DWRITE_TEXT_ALIGNMENT_LEADING;
            const auto mid = DWRITE_PARAGRAPH_ALIGNMENT_CENTER, near_ = DWRITE_PARAGRAPH_ALIGNMENT_NEAR;
            fName = makeFormat(13.f, DWRITE_FONT_WEIGHT_SEMI_BOLD, Lead, mid, false, true);
            fStat = makeFormat(12.f, DWRITE_FONT_WEIGHT_NORMAL, Lead, mid, false, false);
            fBtn = makeFormat(12.f, DWRITE_FONT_WEIGHT_NORMAL, C, mid, false, false);
            fToast = makeFormat(12.5f, DWRITE_FONT_WEIGHT_NORMAL, C, mid, false, true);
            fTitle = makeFormat(16.f, DWRITE_FONT_WEIGHT_SEMI_BOLD, C, mid, false, true);
            fBody = makeFormat(12.5f, DWRITE_FONT_WEIGHT_NORMAL, C, near_, true, false);
            fCardBtn = makeFormat(13.5f, DWRITE_FONT_WEIGHT_SEMI_BOLD, C, mid, false, false);
            fPerf = makeFormat(11.5f, DWRITE_FONT_WEIGHT_NORMAL, Lead, mid, false, false);
            fPerfRight = makeFormat(11.5f, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING, mid, false, false);
            if (SUCCEEDED(dw->CreateTypography(&tabular)))
                tabular->AddFontFeature({DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, 1});
            formatPerf();
            ready = true;
        } catch (const std::exception&) {
            ready = false; // The viewer keeps working without the HUD.
        }
    }

    void setDevice(std::wstring v) { std::lock_guard g(m); if (v != device) { device = std::move(v); metricsDirty = true; } }
    void setStat(std::wstring v) { std::lock_guard g(m); if (v != stat) { stat = std::move(v); metricsDirty = true; } }
    void setMonitors(unsigned active, size_t count) {
        std::lock_guard g(m);
        if (active != monitorActive || count != monitorCount) { monitorActive = active; monitorCount = count; metricsDirty = true; }
    }
    void setFlags(bool captureOn, bool swapOn, bool fullscreenOn) {
        std::lock_guard g(m);
        capture = captureOn; swap = swapOn; fullscreen = fullscreenOn;
    }
    void setAudioAvailable(bool available) { std::lock_guard g(m); audioAvailable = available; metricsDirty = true; }
    void setAudio(bool on) { std::lock_guard g(m); if (on != audioOn) { audioOn = on; metricsDirty = true; } }
    PerfMode cyclePerf() {
        std::lock_guard g(m);
        perfMode = perfMode == PerfMode::Hidden ? PerfMode::Card : perfMode == PerfMode::Card ? PerfMode::Strip : PerfMode::Hidden;
        perfGrab.reset();
        return perfMode;
    }
    void setPerf(const PerfReadout& r) {
        std::lock_guard g(m);
        perf = r;
        if (ready) formatPerf();
    }
    // Starts dragging the readout from client pixel `p`.
    void beginPerfDrag(POINT p) {
        std::lock_guard g(m);
        const float s = float(view.dpi) / 96.f;
        const auto L = layout();
        perfX = L.perf.l; perfY = L.perf.t;
        perfGrab = D2D1::Point2F(float(p.x) / s - L.perf.l, float(p.y) / s - L.perf.t);
    }
    void setView(UINT w, UINT h, UINT dpi) { std::lock_guard g(m); view = {std::max(1u, w), std::max(1u, h), std::max(96u, dpi)}; }
    void toast(std::wstring text, Clock::time_point now) {
        std::lock_guard g(m);
        if (!ready) return;
        toastText = std::move(text);
        toastW = measure(fToast.Get(), toastText);
        toastAt = now;
        toastAlpha = 0;
    }
    void showLost(std::wstring why) {
        std::lock_guard g(m);
        reason = std::move(why); overlayState = HudOverlay::Lost; hover = pressed = HudAction::None;
        expandTarget = false; prog = 0; toastAt.reset(); toastAlpha = 0;
    }
    void showReconnecting() { std::lock_guard g(m); overlayState = HudOverlay::Reconnecting; hover = pressed = HudAction::None; }
    void clearOverlay() { std::lock_guard g(m); overlayState = HudOverlay::None; hover = pressed = HudAction::None; }
    HudOverlay overlay() const { std::lock_guard g(m); return overlayState; }

    // Pointer position in client pixels. What is under it, if the HUD owns that spot.
    HudAction hit(POINT p) { std::lock_guard g(m); return ready ? hitPx(p) : HudAction::None; }
    // Returns true when the HUD's appearance changed.
    bool moved(POINT p, Clock::time_point now) {
        std::lock_guard g(m);
        if (!ready) return false;
        const float s = float(view.dpi) / 96.f;
        const auto L = layout();
        bool changed = false;
        if (perfGrab) {
            const float W = float(view.w) / s, H = float(view.h) / s;
            perfX = std::clamp(float(p.x) / s - perfGrab->x, 0.f, std::max(0.f, W - L.perf.w()));
            perfY = std::clamp(float(p.y) / s - perfGrab->y, 0.f, std::max(0.f, H - L.perf.h()));
            changed = true;
        }
        if (overlayState == HudOverlay::None && !expandTarget && !perfGrab && L.trigger.has(float(p.x) / s, float(p.y) / s)) {
            expandTarget = true; changed = true;
        }
        const auto action = hitDip(L, float(p.x) / s, float(p.y) / s);
        const bool over = overlayState == HudOverlay::None && action != HudAction::None;
        if (over) { if (leaveAt) { leaveAt.reset(); changed = true; } }
        else if (expandTarget && !leaveAt) { leaveAt = now; changed = true; }
        inside = over;
        if (action != hover) { hover = action; changed = true; }
        return changed;
    }
    void left(Clock::time_point now) {
        std::lock_guard g(m);
        inside = false; hover = HudAction::None;
        if (expandTarget && !leaveAt) leaveAt = now;
    }
    // Any press other than on the readout ends a drag of it.
    void setPressed(HudAction a) { std::lock_guard g(m); pressed = a; if (a != HudAction::PerfPanel) perfGrab.reset(); }

    Update step(Clock::time_point now) {
        std::lock_guard g(m);
        bool changed = false, busy = false;
        const float dt = last == Clock::time_point{} ? 0.016f
            : std::clamp(std::chrono::duration<float>(now - last).count(), 0.f, 0.1f);
        last = now;
        if (expandTarget && !inside && leaveAt &&
            std::chrono::duration<float>(now - *leaveAt).count() >= collapseSeconds) {
            expandTarget = false; leaveAt.reset(); changed = true;
        }
        const float goal = expandTarget ? 1.f : 0.f;
        if (prog != goal) {
            prog = goal > prog ? std::min(goal, prog + dt / expandSeconds) : std::max(goal, prog - dt / expandSeconds);
            changed = busy = true;
        }
        if (toastAt) {
            const float t = std::chrono::duration<float>(now - *toastAt).count();
            if (t >= toastSeconds) { toastAt.reset(); toastAlpha = 0; }
            else { toastAlpha = std::clamp(std::min(t / 0.15f, (toastSeconds - t) / 0.3f), 0.f, 1.f); busy = true; }
            changed = true;
        }
        return {changed, busy ? 16u : expandTarget ? 150u : 0u};
    }

    // Draws over the video already blitted into the target. Never throws.
    void draw(ID2D1DeviceContext* c, ID2D1SolidColorBrush* brush, UINT w, UINT h, UINT dpi) noexcept {
        try {
            std::lock_guard g(m);
            if (!ready) return;
            view = {std::max(1u, w), std::max(1u, h), std::max(96u, dpi)};
            c->SetDpi(float(view.dpi), float(view.dpi));
            c->SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            const Painter p{c, brush};
            const auto L = layout();
            const float s = float(view.dpi) / 96.f, W = float(w) / s, H = float(h) / s;
            if (overlayState != HudOverlay::None) { drawCard(p, L, W, H); return; }
            drawPerf(p, L);
            drawPill(p, L);
            drawToast(p, L, W);
        } catch (...) {
        }
    }
};
}
