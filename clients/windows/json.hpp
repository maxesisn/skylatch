// SPDX-License-Identifier: Apache-2.0
#pragma once
#include <charconv>
#include <cstdint>
#include <map>
#include <stdexcept>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

namespace skylatch::json {

struct Value;
using Array = std::vector<Value>;
using Object = std::map<std::string, Value, std::less<>>;

struct Value {
    std::variant<std::nullptr_t, bool, double, std::string, Array, Object> data;

    const Array& asArray() const {
        if (const auto* a = std::get_if<Array>(&data)) return *a;
        throw std::runtime_error("JSON value is not an array");
    }
    const Object& asObject() const {
        if (const auto* o = std::get_if<Object>(&data)) return *o;
        throw std::runtime_error("JSON value is not an object");
    }
    const std::string& asString() const {
        if (const auto* s = std::get_if<std::string>(&data)) return *s;
        throw std::runtime_error("JSON value is not a string");
    }
    double asNumber() const {
        if (const auto* n = std::get_if<double>(&data)) return *n;
        throw std::runtime_error("JSON value is not a number");
    }
    bool asBool() const {
        if (const auto* b = std::get_if<bool>(&data)) return *b;
        throw std::runtime_error("JSON value is not a boolean");
    }
    const Value* field(std::string_view key) const {
        const auto& o = asObject();
        const auto it = o.find(key);
        return it == o.end() ? nullptr : &it->second;
    }
    const Value& req(std::string_view key) const {
        const auto* v = field(key);
        if (!v) throw std::runtime_error("missing JSON field");
        return *v;
    }
};

class Parser {
    std::string_view s;
    size_t i{};
    int depth{};

    void ws() {
        while (i < s.size() && (s[i] == ' ' || s[i] == '\t' || s[i] == '\r' || s[i] == '\n')) ++i;
    }
    char peek() {
        ws();
        if (i >= s.size()) throw std::runtime_error("unexpected end of JSON");
        return s[i];
    }
    void expect(char c) {
        if (peek() != c) throw std::runtime_error("unexpected JSON token");
        ++i;
    }
    static void appendUtf8(std::string& out, uint32_t cp) {
        if (cp <= 0x7F) {
            out.push_back(char(cp));
        } else if (cp <= 0x7FF) {
            out.push_back(char(0xC0 | (cp >> 6)));
            out.push_back(char(0x80 | (cp & 0x3F)));
        } else if (cp <= 0xFFFF) {
            out.push_back(char(0xE0 | (cp >> 12)));
            out.push_back(char(0x80 | ((cp >> 6) & 0x3F)));
            out.push_back(char(0x80 | (cp & 0x3F)));
        } else if (cp <= 0x10FFFF) {
            out.push_back(char(0xF0 | (cp >> 18)));
            out.push_back(char(0x80 | ((cp >> 12) & 0x3F)));
            out.push_back(char(0x80 | ((cp >> 6) & 0x3F)));
            out.push_back(char(0x80 | (cp & 0x3F)));
        } else {
            throw std::runtime_error("invalid Unicode code point");
        }
    }
    uint16_t hex4() {
        if (i + 4 > s.size()) throw std::runtime_error("truncated JSON \\u escape");
        uint16_t v = 0;
        for (int k = 0; k < 4; ++k) {
            const char c = s[i++];
            v <<= 4;
            if (c >= '0' && c <= '9') v |= uint16_t(c - '0');
            else if (c >= 'a' && c <= 'f') v |= uint16_t(c - 'a' + 10);
            else if (c >= 'A' && c <= 'F') v |= uint16_t(c - 'A' + 10);
            else throw std::runtime_error("invalid hex in JSON \\u escape");
        }
        return v;
    }
    std::string parseString() {
        expect('"');
        std::string out;
        while (i < s.size()) {
            const unsigned char c = static_cast<unsigned char>(s[i++]);
            if (c == '"') return out;
            if (c < 0x20) throw std::runtime_error("unescaped control character in JSON string");
            if (c != '\\') { out.push_back(char(c)); continue; }
            if (i >= s.size()) throw std::runtime_error("truncated JSON escape");
            const char e = s[i++];
            switch (e) {
            case '"': case '\\': case '/': out.push_back(e); break;
            case 'b': out.push_back('\b'); break;
            case 'f': out.push_back('\f'); break;
            case 'n': out.push_back('\n'); break;
            case 'r': out.push_back('\r'); break;
            case 't': out.push_back('\t'); break;
            case 'u': {
                const uint32_t hi = hex4();
                if (hi >= 0xD800 && hi <= 0xDBFF) {
                    if (i + 2 > s.size() || s[i] != '\\' || s[i + 1] != 'u')
                        throw std::runtime_error("unpaired UTF-16 high surrogate");
                    i += 2;
                    const uint32_t lo = hex4();
                    if (lo < 0xDC00 || lo > 0xDFFF) throw std::runtime_error("invalid UTF-16 low surrogate");
                    appendUtf8(out, 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00));
                } else if (hi >= 0xDC00 && hi <= 0xDFFF) {
                    throw std::runtime_error("unexpected UTF-16 low surrogate");
                } else {
                    appendUtf8(out, hi);
                }
                break;
            }
            default: throw std::runtime_error("invalid JSON escape");
            }
        }
        throw std::runtime_error("unterminated JSON string");
    }
    Value parseValue() {
        if (++depth > 64) throw std::runtime_error("JSON nesting limit exceeded");
        struct Pop { int& d; ~Pop() { --d; } } pop{depth};
        const char c = peek();
        if (c == '"') return Value{parseString()};
        if (c == '[') {
            ++i;
            Array arr;
            if (peek() == ']') { ++i; return Value{std::move(arr)}; }
            for (;;) {
                arr.push_back(parseValue());
                const char sep = peek();
                if (sep == ']') { ++i; return Value{std::move(arr)}; }
                if (sep != ',') throw std::runtime_error("expected ',' or ']' in JSON array");
                ++i;
            }
        }
        if (c == '{') {
            ++i;
            Object obj;
            if (peek() == '}') { ++i; return Value{std::move(obj)}; }
            for (;;) {
                auto key = parseString();
                expect(':');
                obj.insert_or_assign(std::move(key), parseValue());
                const char sep = peek();
                if (sep == '}') { ++i; return Value{std::move(obj)}; }
                if (sep != ',') throw std::runtime_error("expected ',' or '}' in JSON object");
                ++i;
            }
        }
        if (s.substr(i, 4) == "true") { i += 4; return Value{true}; }
        if (s.substr(i, 5) == "false") { i += 5; return Value{false}; }
        if (s.substr(i, 4) == "null") { i += 4; return Value{nullptr}; }
        const size_t start = i;
        if (s[i] == '-') ++i;
        while (i < s.size() && ((s[i] >= '0' && s[i] <= '9') || s[i] == '.' || s[i] == 'e' || s[i] == 'E' || s[i] == '+' || s[i] == '-')) ++i;
        double num = 0;
        const auto res = std::from_chars(s.data() + start, s.data() + i, num);
        if (res.ec != std::errc{} || res.ptr != s.data() + i) throw std::runtime_error("invalid JSON number");
        return Value{num};
    }

public:
    explicit Parser(std::string_view input) : s(input) {}
    Value parse() {
        auto v = parseValue();
        ws();
        if (i != s.size()) throw std::runtime_error("trailing characters after JSON");
        return v;
    }
};

inline Value parse(std::string_view input) { return Parser(input).parse(); }

}
