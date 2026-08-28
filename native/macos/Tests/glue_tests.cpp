// The frontend's decisions, tested where they can be tested.
//
// Everything here is pure C++ over the real libterminal_ffi.a, so it runs on
// Linux as well as on macOS. What it does not cover is anything that needs
// AppKit — see AppKitTests.mm, which is deliberately much smaller.

#include <chrono>
#include <string>
#include <thread>

#include "../Glue/Config.h"
#include "../Glue/Diagnostics.h"
#include "../Glue/FrameBuffers.h"
#include "../Glue/KeyMap.h"
#include "../Glue/Metrics.h"
#include "../Glue/Palette.h"
#include "../Glue/Session.h"
#include "harness.h"

using namespace glue;

// ---------------------------------------------------------------- Metrics

TEST(cell_width_is_rounded_to_whole_points) {
    // The whole point of rounding once: 80 columns of 7.8pt advances drift 16pt
    // if each glyph is placed where the font says it goes.
    const CellSize cell = cell_size(7.8, 10.0, 3.0, 0.0);
    CHECK_EQ(cell.width, 8.0);
    CHECK_EQ(column_x(80, cell), 640.0);
}

TEST(cell_height_covers_the_whole_line) {
    const CellSize cell = cell_size(8.0, 10.2, 3.1, 0.5);
    CHECK_EQ(cell.height, 14.0);  // ceil(13.8)
    CHECK_EQ(cell.ascent, 10.2);
}

TEST(a_nonsense_font_cannot_produce_a_zero_sized_cell) {
    const CellSize cell = cell_size(0.0, 0.0, 0.0, 0.0);
    CHECK(cell.width >= 1.0);
    CHECK(cell.height >= 1.0);
}

TEST(the_grid_is_derived_from_the_view) {
    const CellSize cell = cell_size(8.0, 12.0, 4.0, 0.0);  // 8 x 16
    const GridSize grid = grid_for(640.0, 320.0, cell);
    CHECK_EQ(grid.cols, 80);
    CHECK_EQ(grid.rows, 20);
}

TEST(leftover_pixels_are_padding_not_a_partial_cell) {
    const CellSize cell = cell_size(8.0, 12.0, 4.0, 0.0);
    const GridSize grid = grid_for(647.0, 335.0, cell);
    CHECK_EQ(grid.cols, 80);
    CHECK_EQ(grid.rows, 20);
}

TEST(a_window_too_small_for_one_cell_still_has_one) {
    const CellSize cell = cell_size(8.0, 12.0, 4.0, 0.0);
    const GridSize grid = grid_for(3.0, 2.0, cell);
    CHECK_EQ(grid.rows, 1);
    CHECK_EQ(grid.cols, 1);
}

TEST(row_zero_is_at_the_top_of_an_unflipped_view) {
    const CellSize cell = cell_size(8.0, 12.0, 4.0, 0.0);  // height 16, ascent 12
    const double view_height = 320.0;
    CHECK_EQ(baseline_y(0, cell, view_height), 308.0);   // 320 - 0 - 12
    CHECK_EQ(baseline_y(1, cell, view_height), 292.0);   // 320 - 16 - 12
    CHECK(baseline_y(19, cell, view_height) < baseline_y(0, cell, view_height));
}

TEST(a_run_rect_covers_its_columns) {
    const CellSize cell = cell_size(8.0, 12.0, 4.0, 0.0);
    const Rect rect = cell_rect(1, 3, 2, cell, 320.0);
    CHECK_EQ(rect.x, 24.0);
    CHECK_EQ(rect.width, 16.0);
    CHECK_EQ(rect.height, 16.0);
    CHECK_EQ(rect.y, 288.0);  // 320 - (1+1)*16
}

TEST(a_rows_rect_sits_directly_under_its_baseline) {
    const CellSize cell = cell_size(8.0, 12.0, 4.0, 0.0);
    const Rect rect = cell_rect(5, 0, 1, cell, 320.0);
    const double baseline = baseline_y(5, cell, 320.0);
    CHECK(baseline >= rect.y);
    CHECK(baseline <= rect.y + rect.height);
}

// ---------------------------------------------------------------- Palette

TEST(the_default_colour_defers_to_the_theme) {
    const Theme& theme = default_theme();
    const Resolved resolved = resolve(0, 0, 0, theme);
    CHECK(resolved.fg == theme.foreground);
    CHECK(resolved.bg == theme.background);
}

TEST(an_indexed_colour_reads_the_palette) {
    const Theme& theme = default_theme();
    const std::uint32_t red = (TERMINAL_COLOR_INDEXED << TERMINAL_COLOR_TAG_SHIFT) | 1;
    const Resolved resolved = resolve(red, 0, 0, theme);
    CHECK(resolved.fg == theme.palette[1]);
}

TEST(truecolour_is_used_exactly_as_given) {
    const std::uint32_t packed = (TERMINAL_COLOR_RGB << TERMINAL_COLOR_TAG_SHIFT) | 0x11'22'33;
    const Resolved resolved = resolve(packed, 0, 0, default_theme());
    CHECK_EQ(resolved.fg.r, 0x11);
    CHECK_EQ(resolved.fg.g, 0x22);
    CHECK_EQ(resolved.fg.b, 0x33);
}

TEST(the_colour_cube_and_grey_ramp_follow_the_convention) {
    const Theme& theme = default_theme();
    // 16 is the corner of the cube: pure black, not palette 0's black.
    CHECK(theme.palette[16] == (Rgba{0, 0, 0, 255}));
    // 196 is 16 + 36*5: full red, no green, no blue.
    CHECK(theme.palette[196] == (Rgba{255, 0, 0, 255}));
    CHECK(theme.palette[231] == (Rgba{255, 255, 255, 255}));
    CHECK(theme.palette[232] == (Rgba{8, 8, 8, 255}));
    CHECK(theme.palette[255] == (Rgba{238, 238, 238, 255}));
}

TEST(bold_brightens_the_ansi_eight_and_nothing_else) {
    const Theme& theme = default_theme();
    const std::uint32_t red = (TERMINAL_COLOR_INDEXED << TERMINAL_COLOR_TAG_SHIFT) | 1;
    const Resolved bold = resolve(red, 0, TERMINAL_ATTR_BOLD, theme);
    CHECK(bold.fg == theme.palette[9]);

    const std::uint32_t bright = (TERMINAL_COLOR_INDEXED << TERMINAL_COLOR_TAG_SHIFT) | 9;
    CHECK(resolve(bright, 0, TERMINAL_ATTR_BOLD, theme).fg == theme.palette[9]);

    const std::uint32_t exact = (TERMINAL_COLOR_RGB << TERMINAL_COLOR_TAG_SHIFT) | 0x010203;
    const Resolved unchanged = resolve(exact, 0, TERMINAL_ATTR_BOLD, theme);
    CHECK_EQ(unchanged.fg.b, 3);
}

TEST(reverse_swaps_after_resolution_not_before) {
    // Reverse on default colours must give the theme's background drawn on its
    // foreground -- which only works if the swap happens after resolving.
    const Theme& theme = default_theme();
    const Resolved resolved = resolve(0, 0, TERMINAL_ATTR_REVERSE, theme);
    CHECK(resolved.fg == theme.background);
    CHECK(resolved.bg == theme.foreground);
}

TEST(dim_blends_toward_whatever_the_background_ended_up_being) {
    const Theme& theme = default_theme();
    const Resolved dim = resolve(0, 0, TERMINAL_ATTR_DIM, theme);
    // Halfway between the theme's foreground and its background.
    CHECK(dim.fg != theme.foreground);
    CHECK(dim.fg != theme.background);
    const int mid = (theme.foreground.r + theme.background.r) / 2;
    CHECK(std::abs(static_cast<int>(dim.fg.r) - mid) <= 1);
}

TEST(hidden_draws_the_text_in_the_background_colour) {
    const Theme& theme = default_theme();
    const std::uint32_t red = (TERMINAL_COLOR_INDEXED << TERMINAL_COLOR_TAG_SHIFT) | 1;
    const Resolved hidden = resolve(red, 0, TERMINAL_ATTR_HIDDEN, theme);
    CHECK(hidden.fg == hidden.bg);
}

TEST(an_unknown_colour_tag_falls_back_to_the_theme) {
    const Theme& theme = default_theme();
    const Resolved resolved = resolve(0x7F'00'00'00, 0, 0, theme);
    CHECK(resolved.fg == theme.foreground);
}

// ---------------------------------------------------------------- KeyMap

namespace {
constexpr std::uint16_t kUpArrow = 0x7E;
constexpr std::uint16_t kReturn = 0x24;
constexpr std::uint16_t kBackspace = 0x33;
constexpr std::uint16_t kKeyC = 0x08;  // the physical C key
constexpr std::uint16_t kKeypad7 = 0x59;
constexpr std::uint16_t kF3 = 0x63;

Decision press(std::uint16_t key_code, std::uint32_t flags, const char* chars = "",
               Options options = {}) {
    return map_key(key_code, flags, chars, std::char_traits<char>::length(chars), options);
}
}  // namespace

TEST(command_never_reaches_the_engine) {
    // Cmd+C is Copy, not Ctrl+C, and the difference is a running process.
    const Decision decision = press(kKeyC, kModCommand, "c");
    CHECK(decision.action == Action::Ignore);
}

TEST(ordinary_typing_is_left_to_the_input_system) {
    const Decision decision = press(kKeyC, 0, "c");
    CHECK(decision.action == Action::SendAsText);
}

TEST(control_combinations_are_encoded_by_the_engine) {
    const Decision decision = press(kKeyC, kModControl, "c");
    CHECK(decision.action == Action::SendKey);
    CHECK(decision.event.code == TerminalKeyCode_Char);
    CHECK_EQ(decision.event.codepoint, static_cast<std::uint32_t>('c'));
    CHECK_EQ(decision.event.modifiers, TERMINAL_MOD_CTRL);
}

TEST(option_is_meta_by_default_and_can_be_turned_off) {
    const Decision meta = press(kKeyC, kModOption, "c");
    CHECK(meta.action == Action::SendKey);
    CHECK_EQ(meta.event.modifiers, TERMINAL_MOD_ALT);

    Options literal;
    literal.option_is_meta = false;
    const Decision typed = press(kKeyC, kModOption, "c", literal);
    CHECK(typed.action == Action::SendAsText);
}

TEST(arrows_are_not_reported_as_modified_by_the_keys_own_flags) {
    // AppKit sets Function and NumericPad on the arrow keys. Passing those
    // through would turn Up into Shift-Up and send a different sequence.
    const std::uint32_t function = 1u << 23;
    const std::uint32_t numeric_pad = 1u << 21;
    const Decision decision = press(kUpArrow, function | numeric_pad);
    CHECK(decision.action == Action::SendKey);
    CHECK(decision.event.code == TerminalKeyCode_Up);
    CHECK_EQ(decision.event.modifiers, 0);
}

TEST(the_special_keys_map_to_their_codes) {
    CHECK(press(kReturn, 0).event.code == TerminalKeyCode_Enter);
    CHECK(press(kBackspace, 0).event.code == TerminalKeyCode_Backspace);
    CHECK(press(0x75, 0).event.code == TerminalKeyCode_Delete);
    CHECK(press(0x35, 0).event.code == TerminalKeyCode_Escape);
    CHECK(press(0x73, 0).event.code == TerminalKeyCode_Home);
    CHECK(press(0x77, 0).event.code == TerminalKeyCode_End);
}

TEST(function_keys_carry_their_number) {
    const Decision decision = press(kF3, 0);
    CHECK(decision.event.code == TerminalKeyCode_F);
    CHECK_EQ(decision.event.number, 3);
    CHECK_EQ(press(0x6F, 0).event.number, 12);
}

TEST(the_keypad_is_distinguishable_from_the_digits_above_the_letters) {
    const Decision decision = press(kKeypad7, 0);
    CHECK(decision.event.code == TerminalKeyCode_KeypadDigit);
    CHECK_EQ(decision.event.number, 7);
    CHECK(press(0x4C, 0).event.code == TerminalKeyCode_KeypadEnter);
}

TEST(modifiers_are_masked_to_the_four_that_matter) {
    const std::uint32_t caps_lock = 1u << 16;
    CHECK_EQ(modifiers_from_flags(caps_lock), 0);
    CHECK_EQ(modifiers_from_flags(kModShift | kModControl),
             static_cast<std::uint16_t>(TERMINAL_MOD_SHIFT | TERMINAL_MOD_CTRL));
}

TEST(codepoints_are_decoded_from_utf8) {
    CHECK_EQ(first_codepoint("a", 1), static_cast<std::uint32_t>('a'));
    CHECK_EQ(first_codepoint("\xC3\xA9", 2), 0xE9u);          // e-acute
    CHECK_EQ(first_codepoint("\xE6\xBC\xA2", 3), 0x6F22u);    // a CJK ideograph
    CHECK_EQ(first_codepoint("", 0), 0u);
    CHECK_EQ(first_codepoint("\xC3", 1), 0u);                 // truncated
    CHECK_EQ(first_codepoint("\x80", 1), 0u);                 // a continuation byte
}

// ---------------------------------------------------------------- Config

TEST(unset_defaults_produce_the_documented_fallbacks) {
    const Config config = resolve(Defaults{}, nullptr);
    CHECK(config.font_name.empty());  // the system monospaced font
    CHECK_EQ(config.font_size, kDefaultFontSize);
    CHECK_EQ(config.shell, std::string("/bin/zsh"));
    CHECK(config.option_is_meta);
    CHECK_EQ(config.rows, kDefaultRows);
    CHECK_EQ(config.cols, kDefaultCols);
}

TEST(the_shell_comes_from_the_environment_before_the_fallback) {
    CHECK_EQ(resolve(Defaults{}, "/bin/fish").shell, std::string("/bin/fish"));
    CHECK_EQ(resolve(Defaults{}, "").shell, std::string("/bin/zsh"));

    Defaults preference;
    preference.shell = "/opt/homebrew/bin/fish";
    CHECK_MSG(resolve(preference, "/bin/zsh").shell == "/opt/homebrew/bin/fish",
              "an explicit preference beats the environment");
}

TEST(the_shell_is_always_a_login_shell) {
    // An app bundle inherits a stub PATH; only the login profile rebuilds it.
    const Config config = resolve(Defaults{}, nullptr);
    CHECK_EQ(config.shell_args.size(), 1u);
    CHECK_EQ(config.shell_args[0], std::string("-l"));
}

TEST(an_unreasonable_font_size_is_clamped_rather_than_obeyed) {
    Defaults tiny;
    tiny.font_size = 0.5;
    CHECK_EQ(resolve(tiny, nullptr).font_size, kMinFontSize);

    Defaults huge;
    huge.font_size = 4000.0;
    CHECK_EQ(resolve(huge, nullptr).font_size, kMaxFontSize);

    Defaults sensible;
    sensible.font_size = 15.0;
    CHECK_EQ(resolve(sensible, nullptr).font_size, 15.0);
}

TEST(an_unset_font_size_is_not_a_size_of_zero) {
    Defaults unset;
    unset.font_size = 0.0;
    CHECK_EQ(resolve(unset, nullptr).font_size, kDefaultFontSize);
}

TEST(option_is_meta_unless_it_is_turned_off) {
    Defaults off;
    off.option_is_meta = 0;
    CHECK(!resolve(off, nullptr).option_is_meta);

    Defaults on;
    on.option_is_meta = 1;
    CHECK(resolve(on, nullptr).option_is_meta);
}

TEST(zoom_walks_within_the_limits) {
    CHECK_EQ(zoomed(13.0, 1), 14.0);
    CHECK_EQ(zoomed(13.0, -1), 12.0);
    CHECK_EQ(zoomed(kMaxFontSize, 1), kMaxFontSize);
    CHECK_EQ(zoomed(kMinFontSize, -1), kMinFontSize);
}

// ----------------------------------------------------------- Diagnostics

namespace {
TerminalChildStatus running() { return TerminalChildStatus{false, false, 0, 0}; }
TerminalChildStatus exited_with(int code) { return TerminalChildStatus{true, true, code, 0}; }
TerminalChildStatus killed_by(int signal) { return TerminalChildStatus{true, true, 0, signal}; }
TerminalChildStatus disconnected() { return TerminalChildStatus{true, false, 0, 0}; }
}  // namespace

TEST(a_running_shell_presents_nothing) {
    const ExitPresentation p = present_exit(running(), "", true);
    CHECK(!p.close_window);
    CHECK(p.title_suffix.empty());
    CHECK(p.overlay.empty());
}

TEST(typing_exit_closes_the_window) {
    const ExitPresentation p = present_exit(exited_with(0), "", true);
    CHECK_MSG(p.close_window, "a clean exit ends the session, as in Alacritty");
    CHECK(p.title_suffix.empty());
}

TEST(a_failed_exit_keeps_the_screen_readable) {
    const ExitPresentation p = present_exit(exited_with(3), "", false);
    CHECK_MSG(!p.close_window, "the error it printed is still on screen");
    CHECK_EQ(p.title_suffix, std::string(" [exited: 3]"));
}

TEST(a_killed_shell_is_named_by_its_signal) {
    const ExitPresentation p = present_exit(killed_by(11), "", false);
    CHECK(!p.close_window);
    CHECK_EQ(p.title_suffix, std::string(" [killed: SIGSEGV]"));
    CHECK_EQ(signal_name(15), std::string("SIGTERM"));
    CHECK_EQ(signal_name(64), std::string("signal 64"));
}

TEST(a_hangup_without_a_status_is_never_read_as_success) {
    // The reader thread stopped for its own reasons. Closing the window here
    // would hide the case where the engine itself is what broke.
    const ExitPresentation p = present_exit(disconnected(), "", false);
    CHECK(!p.close_window);
    CHECK_EQ(p.title_suffix, std::string(" [disconnected]"));
}

TEST(the_overlay_is_a_debug_build_thing_only) {
    const ExitPresentation release = present_exit(exited_with(3), "boom", false);
    CHECK(release.overlay.empty());

    const ExitPresentation debug = present_exit(exited_with(3), "boom", true);
    CHECK(debug.overlay.find("exited: 3") != std::string::npos);
    CHECK_MSG(debug.overlay.find("boom") != std::string::npos,
              "the engine's own complaint is the useful half");
}

TEST(a_disconnected_session_says_so_in_the_overlay) {
    const ExitPresentation p = present_exit(disconnected(), "index out of bounds, screen.rs:412",
                                            true);
    CHECK(p.overlay.find("the engine stopped reading") != std::string::npos);
    CHECK(p.overlay.find("screen.rs:412") != std::string::npos);
}

TEST(closing_the_window_never_depends_on_the_build) {
    // Whether you can read an error may depend on the build. Whether the window
    // closes must not.
    for (const TerminalChildStatus& status :
         {running(), exited_with(0), exited_with(1), killed_by(9), disconnected()}) {
        CHECK_EQ(present_exit(status, "", true).close_window,
                 present_exit(status, "", false).close_window);
    }
}

// ---------------------------------------------- FrameBuffers and Session

namespace {

/// A session running `/bin/sh -c script`, with no wake-up.
Session shell(const std::string& script, std::uint16_t rows = 10, std::uint16_t cols = 40) {
    SessionConfig config;
    config.program = "/bin/sh";
    config.args = {"-c", script};
    config.rows = rows;
    config.cols = cols;
    return Session::spawn(config);
}

/// Wait for the shell to finish, so the test reads a settled screen.
bool wait_for_hangup(const Session& session) {
    for (int i = 0; i < 2000; ++i) {
        if (session.has_hung_up()) {
            return true;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(5));
    }
    return false;
}

std::string row_text(const FrameBuffers& frame, std::uint16_t row) {
    std::string out;
    std::uint16_t col = 0;
    for (std::size_t i = 0; i < frame.run_count(); ++i) {
        const TerminalRun& run = frame.runs()[i];
        if (run.row != row) {
            continue;
        }
        out.append(run.col - col, ' ');
        out.append(frame.run_text(run));
        col = run.col + run.cols;
    }
    return out;
}

}  // namespace

TEST(a_session_starts_a_shell_and_shows_its_output) {
    Session session = shell("printf hello");
    CHECK(session.valid());
    CHECK(wait_for_hangup(session));

    FrameBuffers frame;
    CHECK(frame.copy(session.handle()) == TerminalStatus_Ok);
    CHECK_EQ(frame.info().rows, 10);
    CHECK_EQ(frame.info().cols, 40);
    CHECK_EQ(row_text(frame, 0), std::string("hello"));
}

TEST(the_buffers_grow_once_and_then_stop_allocating) {
    Session session = shell("printf 'a line of output'");
    CHECK(wait_for_hangup(session));

    FrameBuffers frame;
    CHECK_EQ(frame.run_capacity(), 0u);
    CHECK(frame.copy(session.handle()) == TerminalStatus_Ok);
    const std::size_t runs_after_first = frame.run_capacity();
    const std::size_t text_after_first = frame.text_capacity();
    CHECK(runs_after_first > 0);

    for (int i = 0; i < 5; ++i) {
        CHECK(frame.copy(session.handle()) == TerminalStatus_Ok);
    }
    CHECK_MSG(frame.run_capacity() == runs_after_first, "a steady redraw must not reallocate");
    CHECK_MSG(frame.text_capacity() == text_after_first, "a steady redraw must not reallocate");
}

TEST(a_blank_screen_copies_cleanly_from_empty_buffers) {
    // Nothing has been printed, so the frame has no runs at all -- the copy must
    // succeed rather than reporting that a zero-length buffer is too small.
    Session session = shell("sleep 30");
    FrameBuffers frame;
    CHECK(frame.copy(session.handle()) == TerminalStatus_Ok);
    CHECK_EQ(frame.run_count(), 0u);
}

TEST(run_text_never_reads_past_the_buffer) {
    Session session = shell("printf hi");
    CHECK(wait_for_hangup(session));
    FrameBuffers frame;
    CHECK(frame.copy(session.handle()) == TerminalStatus_Ok);

    TerminalRun bogus{};
    bogus.utf8_offset = 1000;
    bogus.utf8_len = 10;
    CHECK(frame.run_text(bogus).empty());
}

TEST(a_null_session_is_a_status_not_a_crash) {
    FrameBuffers frame;
    CHECK(frame.copy(nullptr) == TerminalStatus_NullHandle);
}

TEST(input_reaches_the_shell) {
    Session session = shell("stty -echo -icanon; echo ready; head -c 2 | cat -v");
    FrameBuffers frame;
    bool ready = false;
    for (int i = 0; i < 2000 && !ready; ++i) {
        std::this_thread::sleep_for(std::chrono::milliseconds(5));
        if (frame.copy(session.handle()) == TerminalStatus_Ok) {
            ready = row_text(frame, 0).find("ready") != std::string::npos;
        }
    }
    CHECK(ready);

    CHECK(session.send_text("h") == TerminalStatus_Ok);
    TerminalKeyEvent up{};
    up.code = TerminalKeyCode_Up;
    CHECK(session.send_key(up) == TerminalStatus_Ok);

    CHECK(wait_for_hangup(session));
    CHECK(frame.copy(session.handle()) == TerminalStatus_Ok);
    std::string screen;
    for (std::uint16_t row = 0; row < frame.info().rows; ++row) {
        screen += row_text(frame, row);
    }
    CHECK_MSG(screen.find("h^[") != std::string::npos, screen);
}

TEST(the_title_crosses_with_the_two_call_pattern) {
    Session session = shell("printf '\033]2;a title\007'");
    CHECK(wait_for_hangup(session));
    CHECK_EQ(session.title(), std::string("a title"));
}

TEST(a_session_with_no_title_returns_an_empty_string) {
    Session session = shell("true");
    CHECK(wait_for_hangup(session));
    CHECK(session.title().empty());
}

TEST(resizing_reaches_the_engine) {
    Session session = shell("sleep 30");
    CHECK(session.resize(20, 60) == TerminalStatus_Ok);
    FrameBuffers frame;
    CHECK(frame.copy(session.handle()) == TerminalStatus_Ok);
    CHECK_EQ(frame.info().rows, 20);
    CHECK_EQ(frame.info().cols, 60);
}

TEST(a_moved_session_destroys_exactly_once) {
    // The destructor is the entire lifetime contract (PRD §6), so a move must
    // leave the source holding nothing.
    Session session = shell("sleep 30");
    TerminalSession* handle = session.handle();
    Session moved = std::move(session);
    CHECK(moved.handle() == handle);
    CHECK(!session.valid());
    CHECK(session.title().empty());  // safe on a moved-from session
}

TEST(closing_twice_is_harmless) {
    Session session = shell("true");
    session.close();
    session.close();
    CHECK(!session.valid());
}

TEST(a_failed_spawn_is_reported_rather_than_returned_as_a_handle) {
    SessionConfig config;
    config.program = "/nonexistent/shell";
    Session session = Session::spawn(config);
    CHECK(!session.valid());
}

HARNESS_MAIN()
