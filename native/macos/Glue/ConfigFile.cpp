#include "ConfigFile.h"

#include <fstream>
#include <sstream>
#include <sys/stat.h>

namespace glue {
namespace {

std::string_view trim(std::string_view s) {
    const auto is_space = [](char c) { return c == ' ' || c == '\t' || c == '\r'; };
    while (!s.empty() && is_space(s.front())) {
        s.remove_prefix(1);
    }
    while (!s.empty() && is_space(s.back())) {
        s.remove_suffix(1);
    }
    return s;
}

bool is_regular_file(const std::string& path) {
    struct stat info {};
    return stat(path.c_str(), &info) == 0 && S_ISREG(info.st_mode);
}

}  // namespace

ParsedFile parse(std::string_view text) {
    ParsedFile parsed;
    int line_number = 0;
    std::size_t start = 0;

    while (start <= text.size()) {
        const std::size_t newline = text.find('\n', start);
        const std::size_t end = newline == std::string_view::npos ? text.size() : newline;
        std::string_view line = trim(text.substr(start, end - start));
        ++line_number;

        // A comment only counts at the start of a line, so that a value may
        // contain a '#' -- which every colour in the file does.
        if (!line.empty() && line.front() != '#') {
            const std::size_t equals = line.find('=');
            if (equals == std::string_view::npos) {
                parsed.diagnostics.push_back(
                    {line_number, "expected 'key = value', got '" + std::string(line) + "'"});
            } else {
                const std::string_view key = trim(line.substr(0, equals));
                const std::string_view value = trim(line.substr(equals + 1));
                if (key.empty()) {
                    parsed.diagnostics.push_back({line_number, "a setting with no name"});
                } else {
                    parsed.entries.push_back({std::string(key), std::string(value), line_number});
                }
            }
        }

        if (newline == std::string_view::npos) {
            break;
        }
        start = newline + 1;
    }
    return parsed;
}

std::string config_path(const char* xdg_config_home, const char* home) {
    if (xdg_config_home != nullptr && *xdg_config_home != '\0') {
        return std::string(xdg_config_home) + "/crustty/config";
    }
    if (home == nullptr || *home == '\0') {
        return {};
    }
    const std::string directory_form = std::string(home) + "/.config/crustty/config";
    const std::string file_form = std::string(home) + "/.config/crustty";
    // The plain path wins only when it is a file, so that a directory called
    // crustty behaves the way every other tool's config directory does.
    if (!is_regular_file(directory_form) && is_regular_file(file_form)) {
        return file_form;
    }
    return directory_form;
}

ParsedFile load(const std::string& path) {
    if (path.empty()) {
        return {};
    }
    std::ifstream file(path);
    if (!file) {
        return {};  // no file is not a problem; it means "all defaults"
    }
    std::ostringstream contents;
    contents << file.rdbuf();
    return parse(contents.str());
}

}  // namespace glue
