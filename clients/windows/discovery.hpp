// SPDX-License-Identifier: Apache-2.0
#pragma once
#include "input.hpp"
#include "skylatch_client_core.h"
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Data.Json.h>
#include <cstdio>
#include <memory>
#include <optional>

namespace skylatch {
struct Device {
    std::string id;
    std::wstring name, os, endpoints;
    uint32_t monitors;
};

// Prevent remote metadata from inserting terminal escapes or extra rows in the list.
inline std::wstring displayText(std::wstring_view value) {
    std::wstring s(value);
    for (auto& c : s) {
        if (c < 32 || (c >= 127 && c <= 159) || c == 0x2028 || c == 0x2029) c = L' ';
    }
    return s;
}

inline std::vector<Device> discover(const std::string& hub, const std::string& token) {
    char err[1024]{};
    std::unique_ptr<char, decltype(&skylatch_free_string)> json(
        skylatch_list_devices_json(hub.c_str(), token.c_str(), err, sizeof err), skylatch_free_string);
    require(bool(json), err[0] ? err : "device discovery failed");
    using winrt::Windows::Data::Json::JsonArray;
    try {
        winrt::init_apartment();
        struct Apartment { ~Apartment() { winrt::uninit_apartment(); } } apartment;
        const auto array = JsonArray::Parse(winrt::to_hstring(json.get()));
        std::vector<Device> devices;
        devices.reserve(array.Size());
        for (const auto& value : array) {
            const auto obj = value.GetObject();
            auto id = winrt::to_string(obj.GetNamedString(L"device_id"));
            require(!id.empty() && id.find('\0') == std::string::npos, "hub returned an invalid device ID");
            Device d{std::move(id), displayText(obj.GetNamedString(L"name")), displayText(obj.GetNamedString(L"os")), L"",
                static_cast<uint32_t>(obj.GetNamedNumber(L"monitors"))};
            const auto endpoints = obj.GetNamedArray(L"direct_endpoints");
            for (const auto& endpoint : endpoints) {
                const auto e = endpoint.GetObject();
                auto host = displayText(e.GetNamedString(L"host"));
                if (host.find(L':') != host.npos && !host.starts_with(L'[')) host = L"[" + host + L"]";
                if (!d.endpoints.empty()) d.endpoints += L", ";
                d.endpoints += displayText(e.GetNamedString(L"kind")) + L":" + host + L":" +
                    std::to_wstring(static_cast<uint16_t>(e.GetNamedNumber(L"port")));
            }
            if (d.endpoints.empty()) d.endpoints = L"-";
            devices.push_back(std::move(d));
        }
        return devices;
    } catch (const winrt::hresult_error&) {
        throw std::runtime_error("could not read the hub device list with Windows.Data.Json");
    }
}

inline void printDevices(const std::vector<Device>& devices) {
    using Row = std::array<std::wstring, 5>;
    std::vector<Row> rows{{L"device_id", L"name", L"os", L"monitors", L"direct_endpoints"}};
    std::array<size_t, 4> widths{};
    for (const auto& d : devices) rows.push_back({displayText(fromUtf8(d.id)), d.name, d.os, std::to_wstring(d.monitors), d.endpoints});
    for (const auto& row : rows) {
        for (size_t i = 0; i < widths.size(); ++i) widths[i] = std::max(widths[i], row[i].size());
    }
    for (const auto& row : rows) {
        std::wstring line;
        for (size_t i = 0; i < widths.size(); ++i) {
            line += row[i];
            line.append(widths[i] - row[i].size() + 2, L' ');
        }
        line += row.back();
        DWORD n = 0;
        if (!WriteConsoleW(GetStdHandle(STD_OUTPUT_HANDLE), (line + L"\r\n").c_str(), DWORD(line.size() + 2), &n, nullptr))
            std::puts(toUtf8(line).c_str());
    }
}

inline std::optional<size_t> pickDevice(HWND owner, const std::vector<Device>& devices) {
    constexpr int listId = 100;
    struct Picker { std::vector<std::wstring> labels; } state;
    for (const auto& d : devices)
        state.labels.push_back(d.name + L"  [" + displayText(fromUtf8(d.id)) + L"]  " + d.os);

    struct alignas(DWORD) Template {
        DLGTEMPLATE dialog;
        WORD menu, windowClass;
        wchar_t title[std::size(L"Choose a Skylatch device")];
    } layout{{WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME | DS_CENTER, 0, 0, 0, 0, 290, 168},
        0, 0, L"Choose a Skylatch device"};
    static_assert(offsetof(Template, menu) == 18);
    const INT_PTR choice = DialogBoxIndirectParamW(GetModuleHandleW(nullptr), &layout.dialog, owner,
        [](HWND dialog, UINT message, WPARAM w, LPARAM l) -> INT_PTR {
            if (message == WM_INITDIALOG) {
                const auto& picker = *reinterpret_cast<const Picker*>(l);
                const auto make = [=](DWORD ex, const wchar_t* klass, const wchar_t* text, DWORD style, RECT r, int id) {
                    MapDialogRect(dialog, &r);
                    const auto h = CreateWindowExW(ex, klass, text, WS_CHILD | WS_VISIBLE | style, r.left, r.top, r.right - r.left,
                        r.bottom - r.top, dialog, reinterpret_cast<HMENU>(INT_PTR(id)), GetModuleHandleW(nullptr), nullptr);
                    if (h) SendMessageW(h, WM_SETFONT, WPARAM(GetStockObject(DEFAULT_GUI_FONT)), TRUE);
                    return h;
                };
                const auto label = make(0, L"STATIC", L"Select a device to connect to:", 0, {8, 8, 282, 20}, -1);
                const auto list = make(WS_EX_CLIENTEDGE, L"LISTBOX", L"", WS_VSCROLL | WS_TABSTOP | LBS_NOTIFY | LBS_NOINTEGRALHEIGHT,
                    {8, 22, 282, 138}, listId);
                const auto connect = make(0, L"BUTTON", L"Connect", WS_TABSTOP | BS_DEFPUSHBUTTON, {168, 144, 222, 159}, IDOK);
                const auto cancel = make(0, L"BUTTON", L"Cancel", WS_TABSTOP | BS_PUSHBUTTON, {228, 144, 282, 159}, IDCANCEL);
                if (!label || !list || !connect || !cancel) { EndDialog(dialog, -1); return TRUE; }
                for (const auto& item : picker.labels) {
                    if (SendMessageW(list, LB_ADDSTRING, 0, reinterpret_cast<LPARAM>(item.c_str())) < 0) {
                        EndDialog(dialog, -1); return TRUE;
                    }
                }
                SendMessageW(dialog, DM_SETDEFID, IDOK, 0);
                SendMessageW(list, LB_SETCURSEL, 0, 0);
                SetFocus(list);
                return FALSE;
            }
            if (message == WM_CLOSE || (message == WM_COMMAND && LOWORD(w) == IDCANCEL)) { EndDialog(dialog, 0); return TRUE; }
            if (message == WM_COMMAND && (LOWORD(w) == IDOK || (LOWORD(w) == listId && HIWORD(w) == LBN_DBLCLK))) {
                const auto row = SendDlgItemMessageW(dialog, listId, LB_GETCURSEL, 0, 0);
                if (row >= 0) EndDialog(dialog, row + 1);
                return TRUE;
            }
            return FALSE;
        }, reinterpret_cast<LPARAM>(&state));
    require(choice >= 0, "could not open the device picker");
    if (choice == 0) return std::nullopt;
    return size_t(choice - 1);
}
}
