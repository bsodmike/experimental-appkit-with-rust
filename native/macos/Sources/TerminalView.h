// The view: pixels, keystrokes, and nothing else.
//
// PRD-mac §2. Every decision this file needs has already been made -- by the
// engine, or by Glue. What is left here is translation: an NSEvent into a
// struct, a run into glyphs, a size into a grid. If you find yourself writing a
// condition that matters, it belongs in Glue where it can be tested.

#import <AppKit/AppKit.h>

NS_ASSUME_NONNULL_BEGIN

@interface TerminalView : NSView <NSTextInputClient>

/// Start a shell. Returns NO if it could not be started, in which case the
/// error is on screen in a Debug build.
- (BOOL)startSession;

/// Stop everything, in order: the engine joins its reader thread and hangs up
/// the shell before this returns (PRD §7). Safe to call twice.
- (void)shutdown;

/// The size the window should open at, for this font and grid.
- (NSSize)preferredSize;

/// The window title the shell has asked for, plus any exit marker.
- (NSString *)windowTitle;

/// Re-read ~/.config/crustty/config and apply the font, colours and keyboard
/// mode to the running session. The shell and the opening size cannot change
/// under a session that already exists.
- (void)reloadConfig;

/// Font size, which Cmd-+ and Cmd-- walk.
- (void)zoomBy:(int)steps;
- (void)resetZoom;

@end

NS_ASSUME_NONNULL_END
