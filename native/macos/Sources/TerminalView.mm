#import "TerminalView.h"

#import <CoreText/CoreText.h>

#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

#include "Config.h"
#include "Diagnostics.h"
#include "FrameBuffers.h"
#include "KeyMap.h"
#include "Metrics.h"
#include "Palette.h"
#include "Session.h"

#if DEBUG
static const bool kDebugBuild = true;
#else
static const bool kDebugBuild = false;
#endif

/// The wake-up callback. It runs on the engine's reader thread, so it does one
/// thing and one thing only: ask the main thread for a redraw (PRD-mac §7).
/// Drawing here, or calling back into the engine, would be a deadlock or worse.
static void CrusttyWakeUp(void *ctx) {
    TerminalView *view = (__bridge TerminalView *)ctx;
    dispatch_async(dispatch_get_main_queue(), ^{
      [view setNeedsDisplay:YES];
    });
}

@implementation TerminalView {
    glue::Session _session;
    glue::FrameBuffers _frame;
    glue::Config _config;
    glue::CellSize _cell;
    double _fontSize;
    CTFontRef _font;
    NSString *_markedText;
    BOOL _hungUp;
    std::string _titleSuffix;
    std::string _overlay;
}

#pragma mark - Setup

- (instancetype)initWithFrame:(NSRect)frame {
    self = [super initWithFrame:frame];
    if (self == nil) {
        return nil;
    }
    _config = [self loadConfig];
    _fontSize = _config.font_size;
    [self rebuildFont];
    return self;
}

- (void)dealloc {
    [self shutdown];
    if (_font != nullptr) {
        CFRelease(_font);
    }
}

/// Read NSUserDefaults, and let Glue decide what to believe (PRD-mac §8).
- (glue::Config)loadConfig {
    NSUserDefaults *defaults = [NSUserDefaults standardUserDefaults];
    glue::Defaults raw;

    NSString *fontName = [defaults stringForKey:@"fontName"];
    std::string fontNameStorage = fontName != nil ? fontName.UTF8String : "";
    raw.font_name = fontNameStorage.empty() ? nullptr : fontNameStorage.c_str();

    if ([defaults objectForKey:@"fontSize"] != nil) {
        raw.font_size = [defaults doubleForKey:@"fontSize"];
    }

    NSString *shell = [defaults stringForKey:@"shell"];
    std::string shellStorage = shell != nil ? shell.UTF8String : "";
    raw.shell = shellStorage.empty() ? nullptr : shellStorage.c_str();

    if ([defaults objectForKey:@"optionIsMeta"] != nil) {
        raw.option_is_meta = [defaults boolForKey:@"optionIsMeta"] ? 1 : 0;
    }

    const char *envShell = getenv("SHELL");
    return glue::resolve(raw, envShell);
}

/// Measure the font. Everything about the grid follows from these numbers.
- (void)rebuildFont {
    if (_font != nullptr) {
        CFRelease(_font);
        _font = nullptr;
    }

    NSFont *font = nil;
    if (!_config.font_name.empty()) {
        font = [NSFont fontWithName:@(_config.font_name.c_str()) size:_fontSize];
    }
    if (font == nil) {
        // Always present, and actually monospaced, which a named font from a
        // preference might not be.
        font = [NSFont monospacedSystemFontOfSize:_fontSize weight:NSFontWeightRegular];
    }
    _font = (CTFontRef)CFRetain((__bridge CFTypeRef)font);

    UniChar sample = 'M';
    CGGlyph glyph = 0;
    CGSize advance = CGSizeZero;
    if (CTFontGetGlyphsForCharacters(_font, &sample, &glyph, 1)) {
        CTFontGetAdvancesForGlyphs(_font, kCTFontOrientationHorizontal, &glyph, &advance, 1);
    }
    _cell = glue::cell_size(advance.width, CTFontGetAscent(_font), CTFontGetDescent(_font),
                            CTFontGetLeading(_font));
}

- (NSSize)preferredSize {
    return NSMakeSize(_config.cols * _cell.width, _config.rows * _cell.height);
}

#pragma mark - Session

- (BOOL)startSession {
    glue::SessionConfig config;
    config.program = _config.shell;
    config.args = _config.shell_args;
    config.cwd = "";  // the engine reads this as the home directory
    const glue::GridSize grid =
        glue::grid_for(self.bounds.size.width, self.bounds.size.height, _cell);
    config.rows = grid.rows;
    config.cols = grid.cols;
    config.wake_up = CrusttyWakeUp;
    config.wake_up_ctx = (__bridge void *)self;

    _session = glue::Session::spawn(config);
    if (!_session.valid()) {
        _hungUp = YES;
        _overlay = kDebugBuild ? [self lastEngineError] : std::string();
        [self setNeedsDisplay:YES];
        return NO;
    }
    return YES;
}

- (void)shutdown {
    // Joins the reader thread and hangs up the shell before returning, which is
    // why this happens while the view is still alive (PRD-mac §9).
    _session.close();
}

- (std::string)lastEngineError {
    uint32_t len = 0;
    terminal_copy_last_error(nullptr, 0, &len);
    if (len == 0) {
        return {};
    }
    std::string message(len, '\0');
    if (terminal_copy_last_error(reinterpret_cast<uint8_t *>(message.data()), len, &len) !=
        TerminalStatus_Ok) {
        return {};
    }
    return message;
}

- (NSString *)windowTitle {
    std::string title = _session.valid() ? _session.title() : std::string();
    if (title.empty()) {
        title = "Crustty";
    }
    title += _titleSuffix;
    return @(title.c_str());
}

/// Ask how the shell ended, and act on the answer (PRD-mac §13).
- (void)checkChildStatus {
    if (_hungUp || !_session.valid()) {
        return;
    }
    TerminalChildStatus status{};
    if (terminal_child_status(_session.handle(), &status) != TerminalStatus_Ok) {
        return;
    }
    if (!status.hung_up) {
        return;
    }

    _hungUp = YES;
    const glue::ExitPresentation presentation =
        glue::present_exit(status, [self lastEngineError], kDebugBuild);
    _titleSuffix = presentation.title_suffix;
    _overlay = presentation.overlay;

    if (presentation.close_window) {
        // The ordinary end of a session: close, as Alacritty does.
        [self.window performClose:nil];
        return;
    }
    // Otherwise the screen stays exactly as the shell left it, because whatever
    // went wrong is written on it.
    [self.window setTitle:[self windowTitle]];
}

#pragma mark - Geometry

- (void)setFrameSize:(NSSize)newSize {
    [super setFrameSize:newSize];
    [self syncTerminalSize];
}

/// Tell the engine the new grid, but only when the grid actually changed:
/// dragging across a few points of width is not a resize (PRD-mac §10).
- (void)syncTerminalSize {
    if (!_session.valid()) {
        return;
    }
    const glue::GridSize grid =
        glue::grid_for(self.bounds.size.width, self.bounds.size.height, _cell);
    if (grid.rows == _frame.info().rows && grid.cols == _frame.info().cols) {
        return;
    }
    _session.resize(grid.rows, grid.cols);
    [self setNeedsDisplay:YES];
}

- (void)zoomBy:(int)steps {
    const double next = glue::zoomed(_fontSize, steps);
    if (next == _fontSize) {
        return;
    }
    _fontSize = next;
    [self rebuildFont];
    [self syncTerminalSize];
    [self setNeedsDisplay:YES];
}

- (void)resetZoom {
    if (_fontSize == _config.font_size) {
        return;
    }
    _fontSize = _config.font_size;
    [self rebuildFont];
    [self syncTerminalSize];
    [self setNeedsDisplay:YES];
}

#pragma mark - Drawing

- (BOOL)isOpaque {
    return YES;
}

- (void)drawRect:(NSRect)dirtyRect {
    // Every redraw is also when we notice the shell has gone: the engine marks
    // the screen dirty one last time on its way out, so this always runs.
    [self checkChildStatus];

    CGContextRef ctx = (CGContextRef)[[NSGraphicsContext currentContext] CGContext];
    const glue::Theme &theme = glue::default_theme();
    const double height = self.bounds.size.height;

    [self fillRect:dirtyRect color:theme.background inContext:ctx];

    if (_session.valid() && _frame.copy(_session.handle()) != TerminalStatus_Ok) {
        // A failed copy leaves the previous frame in the buffers; drawing a
        // mixture of two frames is worse than drawing neither.
        [self drawOverlayInContext:ctx];
        return;
    }

    // Backgrounds first, so a run never paints over its neighbour's.
    for (size_t i = 0; i < _frame.run_count(); ++i) {
        const TerminalRun &run = _frame.runs()[i];
        const glue::Resolved colors = glue::resolve(run.fg, run.bg, run.attrs, theme);
        if (colors.bg == theme.background) {
            continue;
        }
        const glue::Rect rect = glue::cell_rect(run.row, run.col, run.cols, _cell, height);
        [self fillRect:NSMakeRect(rect.x, rect.y, rect.width, rect.height)
                 color:colors.bg
             inContext:ctx];
    }

    CGContextSetTextMatrix(ctx, CGAffineTransformIdentity);
    for (size_t i = 0; i < _frame.run_count(); ++i) {
        [self drawRun:_frame.runs()[i] theme:theme inContext:ctx];
    }

    [self drawCursorWithTheme:theme inContext:ctx];
    [self drawOverlayInContext:ctx];
}

- (void)fillRect:(NSRect)rect color:(glue::Rgba)color inContext:(CGContextRef)ctx {
    CGContextSetRGBFillColor(ctx, color.r / 255.0, color.g / 255.0, color.b / 255.0,
                             color.a / 255.0);
    CGContextFillRect(ctx, rect);
}

/// Draw one run: shaped by Core Text, positioned by the grid.
///
/// The line is built for its shaping -- combining marks have to be shaped with
/// the character they attach to -- and then taken apart, because CTLineDraw
/// would place each glyph at the font's own advance and a row of those drifts
/// away from the column it belongs to (PRD-mac §5).
- (void)drawRun:(const TerminalRun &)run
          theme:(const glue::Theme &)theme
      inContext:(CGContextRef)ctx {
    const std::string_view text = _frame.run_text(run);
    if (text.empty()) {
        return;
    }
    const glue::Resolved colors = glue::resolve(run.fg, run.bg, run.attrs, theme);
    if (colors.fg == colors.bg) {
        return;  // hidden text: present, and invisible
    }

    NSString *string = [[NSString alloc] initWithBytes:text.data()
                                                length:text.size()
                                              encoding:NSUTF8StringEncoding];
    if (string == nil) {
        return;
    }

    // CTFontDrawGlyphs paints with the context's fill colour. The foreground
    // attribute below only takes effect when CTLineDraw does the drawing, and
    // this deliberately does not (PRD-mac §5) -- so without this line every
    // glyph is drawn in whatever colour was last set, which is the window
    // background, and the text is perfectly invisible.
    CGContextSetRGBFillColor(ctx, colors.fg.r / 255.0, colors.fg.g / 255.0, colors.fg.b / 255.0,
                             colors.fg.a / 255.0);

    CGColorRef color = CGColorCreateGenericRGB(colors.fg.r / 255.0, colors.fg.g / 255.0,
                                               colors.fg.b / 255.0, colors.fg.a / 255.0);
    NSDictionary *attributes = @{
        (__bridge NSString *)kCTFontAttributeName : (__bridge NSFont *)_font,
        (__bridge NSString *)kCTForegroundColorAttributeName : (__bridge id)color,
        // A ligature merges two clusters into one glyph, and the column a glyph
        // belongs to is counted in clusters (PRD-mac §5).
        (__bridge NSString *)kCTLigatureAttributeName : @0,
    };
    NSAttributedString *attributed = [[NSAttributedString alloc] initWithString:string
                                                                    attributes:attributes];
    CTLineRef line = CTLineCreateWithAttributedString((__bridge CFAttributedStringRef)attributed);
    CGColorRelease(color);

    // Which grapheme cluster each UTF-16 index belongs to. The engine promises
    // every cluster in a run is one column wide, except a run that is a single
    // double-width cluster -- so cluster index is column offset.
    const CFIndex length = CFStringGetLength((__bridge CFStringRef)string);
    std::vector<int> clusterOfIndex(static_cast<size_t>(length), 0);
    int clusters = 0;
    for (CFIndex i = 0; i < length;) {
        const CFRange cluster =
            CFStringGetRangeOfComposedCharactersAtIndex((__bridge CFStringRef)string, i);
        for (CFIndex k = cluster.location; k < cluster.location + cluster.length; ++k) {
            clusterOfIndex[static_cast<size_t>(k)] = clusters;
        }
        i = cluster.location + cluster.length;
        ++clusters;
    }

    const double baseline = glue::baseline_y(run.row, _cell, self.bounds.size.height);
    CFArrayRef glyphRuns = CTLineGetGlyphRuns(line);
    for (CFIndex r = 0; r < CFArrayGetCount(glyphRuns); ++r) {
        CTRunRef glyphRun = (CTRunRef)CFArrayGetValueAtIndex(glyphRuns, r);
        const CFIndex count = CTRunGetGlyphCount(glyphRun);
        if (count == 0) {
            continue;
        }

        std::vector<CGGlyph> glyphs(static_cast<size_t>(count));
        std::vector<CFIndex> indices(static_cast<size_t>(count));
        std::vector<CGPoint> positions(static_cast<size_t>(count));
        CTRunGetGlyphs(glyphRun, CFRangeMake(0, count), glyphs.data());
        CTRunGetStringIndices(glyphRun, CFRangeMake(0, count), indices.data());

        for (CFIndex g = 0; g < count; ++g) {
            const CFIndex index = indices[static_cast<size_t>(g)];
            const int cluster =
                (index >= 0 && index < length) ? clusterOfIndex[static_cast<size_t>(index)] : 0;
            positions[static_cast<size_t>(g)] =
                CGPointMake(glue::column_x(run.col + cluster, _cell), baseline);
        }

        CTFontRef runFont =
            (CTFontRef)CFDictionaryGetValue(CTRunGetAttributes(glyphRun), kCTFontAttributeName);
        CTFontDrawGlyphs(runFont != nullptr ? runFont : _font, glyphs.data(), positions.data(),
                         static_cast<size_t>(count), ctx);
    }

    if ((run.attrs & TERMINAL_ATTR_UNDERLINE) != 0) {
        const glue::Rect rect = glue::cell_rect(run.row, run.col, run.cols, _cell,
                                                self.bounds.size.height);
        [self fillRect:NSMakeRect(rect.x, baseline - 2.0, rect.width, 1.0)
                 color:colors.fg
             inContext:ctx];
    }
    if ((run.attrs & TERMINAL_ATTR_STRIKETHROUGH) != 0) {
        const glue::Rect rect = glue::cell_rect(run.row, run.col, run.cols, _cell,
                                                self.bounds.size.height);
        [self fillRect:NSMakeRect(rect.x, baseline + _cell.ascent / 3.0, rect.width, 1.0)
                 color:colors.fg
             inContext:ctx];
    }

    CFRelease(line);
}

- (void)drawCursorWithTheme:(const glue::Theme &)theme inContext:(CGContextRef)ctx {
    if (_hungUp || !_frame.info().cursor_visible) {
        return;
    }
    const glue::Rect rect = glue::cell_rect(_frame.info().cursor_row, _frame.info().cursor_col, 1,
                                            _cell, self.bounds.size.height);
    [self fillRect:NSMakeRect(rect.x, rect.y, rect.width, rect.height)
             color:theme.cursor
         inContext:ctx];
}

/// The Debug-only diagnostic band (PRD-mac §13). Release builds draw nothing.
- (void)drawOverlayInContext:(CGContextRef)ctx {
    if (_overlay.empty()) {
        return;
    }
    NSString *text = @(_overlay.c_str());
    NSDictionary *attributes = @{
        NSFontAttributeName : [NSFont monospacedSystemFontOfSize:11.0
                                                          weight:NSFontWeightRegular],
        NSForegroundColorAttributeName : [NSColor whiteColor],
        NSBackgroundColorAttributeName : [NSColor colorWithSRGBRed:0.5 green:0 blue:0 alpha:0.9],
    };
    const NSSize size = [text sizeWithAttributes:attributes];
    const NSRect band = NSMakeRect(0, 0, self.bounds.size.width, size.height + 8);
    [self fillRect:band color:glue::Rgba{80, 0, 0, 230} inContext:ctx];
    [text drawAtPoint:NSMakePoint(6, 4) withAttributes:attributes];
}

#pragma mark - Input

- (BOOL)acceptsFirstResponder {
    return YES;
}

- (void)keyDown:(NSEvent *)event {
    if (_hungUp) {
        return;  // nothing to type into
    }

    NSString *characters = event.charactersIgnoringModifiers ?: @"";
    const char *utf8 = characters.UTF8String;
    glue::Options options;
    options.option_is_meta = _config.option_is_meta;
    const glue::Decision decision =
        glue::map_key(event.keyCode, static_cast<uint32_t>(event.modifierFlags), utf8,
                      utf8 != nullptr ? strlen(utf8) : 0, options);

    switch (decision.action) {
        case glue::Action::SendKey:
            _session.send_key(decision.event);
            break;
        case glue::Action::SendAsText:
            // The input system owns this one: dead keys and IME composition
            // cannot work any other way (PRD-mac §6).
            [self interpretKeyEvents:@[ event ]];
            break;
        case glue::Action::Ignore:
            [super keyDown:event];
            break;
    }
}

- (void)paste:(id)sender {
    NSString *text = [[NSPasteboard generalPasteboard] stringForType:NSPasteboardTypeString];
    if (text == nil || _hungUp) {
        return;
    }
    // The engine frames it, and strips any end marker the text contains.
    _session.paste(text.UTF8String);
}

#pragma mark - NSTextInputClient

- (void)insertText:(id)string replacementRange:(NSRange)replacementRange {
    (void)replacementRange;
    _markedText = nil;
    NSString *text = [string isKindOfClass:[NSAttributedString class]] ? [string string] : string;
    if (text.length == 0 || _hungUp) {
        return;
    }
    _session.send_text(text.UTF8String);
}

- (void)setMarkedText:(id)string
        selectedRange:(NSRange)selectedRange
     replacementRange:(NSRange)replacementRange {
    (void)selectedRange;
    (void)replacementRange;
    // Composition in progress: shown here, sent nowhere. The shell must not see
    // provisional text it would have no way to take back (PRD-mac §6).
    _markedText = [string isKindOfClass:[NSAttributedString class]] ? [string string] : string;
    [self setNeedsDisplay:YES];
}

- (void)unmarkText {
    _markedText = nil;
    [self setNeedsDisplay:YES];
}

- (BOOL)hasMarkedText {
    return _markedText.length > 0;
}

- (NSRange)markedRange {
    return _markedText.length > 0 ? NSMakeRange(0, _markedText.length)
                                  : NSMakeRange(NSNotFound, 0);
}

- (NSRange)selectedRange {
    return NSMakeRange(NSNotFound, 0);
}

- (nullable NSAttributedString *)attributedSubstringForProposedRange:(NSRange)range
                                                         actualRange:(NSRangePointer)actualRange {
    (void)range;
    (void)actualRange;
    return nil;
}

- (NSArray<NSAttributedStringKey> *)validAttributesForMarkedText {
    return @[];
}

- (NSRect)firstRectForCharacterRange:(NSRange)range actualRange:(NSRangePointer)actualRange {
    (void)range;
    (void)actualRange;
    // Where the candidate window should appear: at the cursor.
    const glue::Rect rect = glue::cell_rect(_frame.info().cursor_row, _frame.info().cursor_col, 1,
                                            _cell, self.bounds.size.height);
    NSRect inView = NSMakeRect(rect.x, rect.y, rect.width, rect.height);
    return [self.window convertRectToScreen:[self convertRect:inView toView:nil]];
}

- (NSUInteger)characterIndexForPoint:(NSPoint)point {
    (void)point;
    return NSNotFound;
}

- (void)doCommandBySelector:(SEL)selector {
    // The input system's idea of what a key means. The engine already decided,
    // so anything arriving here is deliberately dropped rather than acted on.
    (void)selector;
}

@end
