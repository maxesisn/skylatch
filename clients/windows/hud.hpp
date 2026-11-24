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

// In-window viewer UI: a top-edge latch strip that expands into a control pill, short toasts, and a
// disconnect / reconnect card. State and layout live here (UI thread writes, renderer thread reads
// under one lock); drawing goes through a Direct2D device context that targets the swap chain's own
// back buffer, so the HUD needs no extra window and cannot flicker or lose z-order.

struct HudText {
    const wchar_t *monitor, *capture, *swap, *fullscreen;
    const wchar_t *captureOn, *captureOff, *fullscreenOn, *fullscreenOff, *swapOn, *swapOff;
    const wchar_t *lostTitle, *reconnectingTitle, *reconnect, *reconnecting, *close, *userDisconnected;
};
inline const HudText& hudText() {
    static const HudText zh{L"屏 %u", L"系统键", L"Ctrl↔Cmd", L"全屏",
        L"已捕获系统键 · 双按右 Ctrl 释放", L"已释放系统键 · 双按右 Ctrl 捕获",
        L"全屏 · 已捕获系统键 · 长按 Esc 退出", L"已退出全屏", L"Ctrl ↔ Cmd 互换已开启", L"Ctrl ↔ Cmd 互换已关闭",
        L"与 %ls 的连接已中断", L"正在重新连接 %ls…", L"立即重连", L"重连中…", L"关闭窗口", L"你已主动断开连接"};
    static const HudText en{L"Screen %u", L"System keys", L"Ctrl↔Cmd", L"Fullscreen",
        L"System keys captured · double-tap Right Ctrl to release", L"System keys released · double-tap Right Ctrl to capture",
        L"Fullscreen · system keys captured · hold Esc to exit", L"Exited fullscreen", L"Ctrl ↔ Cmd swap on", L"Ctrl ↔ Cmd swap off",
        L"Connection to %ls was lost", L"Reconnecting to %ls…", L"Reconnect now", L"Reconnecting…", L"Close window", L"You disconnected"};
    static const bool chinese = PRIMARYLANGID(GetUserDefaultUILanguage()) == LANG_CHINESE;
    return chinese ? zh : en;
}

// The five pill buttons are consecutive so a button index maps straight to its action.
enum class HudAction { None, Surface, Monitor, Capture, Swap, Fullscreen, Disconnect, Reconnect, Close };
enum class HudOverlay { None, Lost, Reconnecting };

class Hud {
    struct Rect {
        float l{}, t{}, r{}, b{};
        bool has(float x, float y) const { return x >= l && x < r && y >= t && y < b; }
        float w() const { return r - l; }
        float h() const { return b - t; }
    };
    struct Layout {
        Rect trigger, strip, pill, logo, name, stat, sep, btn[5], card, icon, title, reason, cbtn[2];
        bool vis[5]{}, showStat{};
    };
    struct View { UINT w{1}, h{1}, dpi{96}; };

    static constexpr float pillH = 40, pillTop = 8, toastSeconds = 1.2f, collapseSeconds = 1.8f, expandSeconds = 0.18f;

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
    ComPtr<IDWriteTextFormat> fName, fStat, fBtn, fToast, fTitle, fBody, fCardBtn;
    std::wstring device, stat, toastText, reason, label[5];
    unsigned monitorActive{};
    size_t monitorCount{};
    bool capture{}, swap{}, fullscreen{};
    View view;
    float nameW{}, statW{}, toastW{}, labelW[5]{};
    bool metricsDirty{true};
    HudOverlay overlayState{HudOverlay::None};
    bool expandTarget{}, inside{};
    float prog{};
    std::optional<Clock::time_point> leaveAt, toastAt;
    Clock::time_point last{};
    float toastAlpha{};
    HudAction hover{HudAction::None}, pressed{HudAction::None};

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
        label[0] = buf; label[1] = t.capture; label[2] = t.swap; label[3] = t.fullscreen; label[4].clear();
        for (int i = 0; i < 4; ++i) labelW[i] = measure(fBtn.Get(), label[i]);
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
        for (int i = 1; i < 5; ++i) L.vis[i] = true;
        const auto buttonW = [&](int i) { return i == 4 ? 30.f : labelW[i] + 22.f; };
        float buttons = 0;
        int shown = 0;
        for (int i = 0; i < 5; ++i) if (L.vis[i]) buttons += buttonW(i) + (shown++ ? btnGap : 0);
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
        for (int i = 0; i < 5; ++i) {
            if (!L.vis[i]) continue;
            L.btn[i] = {x, pillTop + 6, x + buttonW(i), pillTop + pillH - 6};
            x += buttonW(i) + btnGap;
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
            for (int i = 0; i < 5; ++i)
                if (L.vis[i] && L.btn[i].has(x, y)) return HudAction(int(HudAction::Monitor) + i);
            return HudAction::Surface;
        }
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
        for (int i = 0; i < 5; ++i) {
            if (!L.vis[i]) continue;
            const Rect& r = L.btn[i];
            const auto action = HudAction(int(HudAction::Monitor) + i);
            const bool hot = hover == action, down = hot && pressed == action;
            if (i == 4) {
                p.fill(r, 8, down ? hex(0xB91C1C, a) : hot ? hex(0xEF4444, a) : hex(0xDC2626, .85f * a));
                const float cx = (r.l + r.r) / 2, cy = (r.t + r.b) / 2;
                p.line(cx - 4.5f, cy - 4.5f, cx + 4.5f, cy + 4.5f, hex(0xFFFFFF, a), 1.7f);
                p.line(cx - 4.5f, cy + 4.5f, cx + 4.5f, cy - 4.5f, hex(0xFFFFFF, a), 1.7f);
                continue;
            }
            const bool on = (i == 1 && capture) || (i == 2 && swap) || (i == 3 && fullscreen);
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
        if (overlayState == HudOverlay::None && !expandTarget && L.trigger.has(float(p.x) / s, float(p.y) / s)) {
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
    void setPressed(HudAction a) { std::lock_guard g(m); pressed = a; }

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
            drawPill(p, L);
            drawToast(p, L, W);
        } catch (...) {
        }
    }
};
}
