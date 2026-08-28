// A test harness small enough to read in one go.
//
// The glue tests build as one plain C++ binary so they can run on Linux, where
// XCTest does not exist and where most of this project's verification happens.
// Nothing here is clever: a registry, two macros, and a main that returns
// non-zero if anything failed.

#pragma once

#include <cstdio>
#include <functional>
#include <string>
#include <vector>

namespace harness {

struct Test {
    std::string name;
    std::function<void()> body;
};

inline std::vector<Test>& tests() {
    static std::vector<Test> registry;
    return registry;
}

inline int& failures() {
    static int count = 0;
    return count;
}

inline const char*& current() {
    static const char* name = "";
    return name;
}

struct Registrar {
    Registrar(const char* name, std::function<void()> body) {
        tests().push_back(Test{name, std::move(body)});
    }
};

inline void fail(const char* file, int line, const std::string& what) {
    std::fprintf(stderr, "  FAIL %s\n    %s:%d: %s\n", current(), file, line, what.c_str());
    ++failures();
}

template <typename A, typename B>
std::string describe(const A& actual, const B& expected) {
    std::string message = "expected ";
    if constexpr (std::is_convertible_v<B, std::string>) {
        message += "\"" + std::string(expected) + "\", got \"" + std::string(actual) + "\"";
    } else {
        message += std::to_string(expected) + ", got " + std::to_string(actual);
    }
    return message;
}

}  // namespace harness

#define TEST(name)                                                            \
    static void name();                                                       \
    static harness::Registrar registrar_##name(#name, name);                  \
    static void name()

#define CHECK(cond)                                                           \
    do {                                                                      \
        if (!(cond)) {                                                        \
            harness::fail(__FILE__, __LINE__, "CHECK(" #cond ") failed");      \
        }                                                                     \
    } while (0)

#define CHECK_MSG(cond, message)                                              \
    do {                                                                      \
        if (!(cond)) {                                                        \
            harness::fail(__FILE__, __LINE__,                                  \
                          std::string("CHECK(" #cond ") failed: ") + (message)); \
        }                                                                     \
    } while (0)

#define CHECK_EQ(actual, expected)                                            \
    do {                                                                      \
        const auto actual_value = (actual);                                   \
        const auto expected_value = (expected);                               \
        if (!(actual_value == expected_value)) {                              \
            harness::fail(__FILE__, __LINE__,                                  \
                          harness::describe(actual_value, expected_value));   \
        }                                                                     \
    } while (0)

#define HARNESS_MAIN()                                                        \
    int main() {                                                              \
        for (const harness::Test& test : harness::tests()) {                  \
            harness::current() = test.name.c_str();                           \
            const int before = harness::failures();                           \
            test.body();                                                      \
            if (harness::failures() == before) {                              \
                std::printf("  ok   %s\n", test.name.c_str());                \
            }                                                                 \
        }                                                                     \
        std::printf("\n%zu tests, %d failures\n", harness::tests().size(),     \
                    harness::failures());                                     \
        return harness::failures() == 0 ? 0 : 1;                              \
    }
