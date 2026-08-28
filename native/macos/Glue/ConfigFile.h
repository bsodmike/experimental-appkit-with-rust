// The config file: where it lives, and how a line becomes a setting.
//
// Ghostty's format, because it is the one the user already knows: flat
// `key = value`, kebab-case keys, `#` comments, and repeated keys where a
// setting is really a list. No sections.
//
// This only turns text into entries. What the keys mean is Config's problem,
// and what a colour is is Palette's — keeping those apart is what lets each of
// them be tested for one thing.

#pragma once

#include <string>
#include <string_view>
#include <vector>

namespace glue {

/// One `key = value` line, remembered with the line it came from so that a
/// complaint about it can say where to look.
struct Entry {
    std::string key;
    std::string value;
    int line = 0;
};

/// Something wrong with a line. Never fatal: the rest of the file still counts.
struct Diagnostic {
    int line = 0;
    std::string message;
};

struct ParsedFile {
    std::vector<Entry> entries;
    std::vector<Diagnostic> diagnostics;
};

/// Parse config text.
///
/// A `#` starts a comment **only at the start of a line**. This is not a
/// stylistic choice: `background = #1e1e1e` is a colour, and a trailing-comment
/// rule would silently eat every colour in the file.
ParsedFile parse(std::string_view text);

/// Where the config file is, given the environment:
///
/// 1. `$XDG_CONFIG_HOME/crustty/config`, when that variable is set
/// 2. `$HOME/.config/crustty/config`
/// 3. `$HOME/.config/crustty`, when that path is a regular file rather than a
///    directory — so that either reading of "the config lives at
///    ~/.config/crustty" turns out to be right
///
/// Empty when there is no home directory to look in.
std::string config_path(const char* xdg_config_home, const char* home);

/// Read and parse a file. A file that is not there is not an error: it parses
/// as nothing, and every setting keeps its default.
ParsedFile load(const std::string& path);

}  // namespace glue
