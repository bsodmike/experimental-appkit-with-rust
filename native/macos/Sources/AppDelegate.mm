#import "AppDelegate.h"

#import "TerminalView.h"

@implementation AppDelegate {
    NSWindow *_window;
    TerminalView *_view;
}

- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    (void)notification;

    _view = [[TerminalView alloc] initWithFrame:NSMakeRect(0, 0, 800, 480)];
    const NSSize size = [_view preferredSize];
    [_view setFrameSize:size];

    _window = [[NSWindow alloc]
        initWithContentRect:NSMakeRect(0, 0, size.width, size.height)
                  styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                            NSWindowStyleMaskMiniaturizable | NSWindowStyleMaskResizable
                    backing:NSBackingStoreBuffered
                      defer:NO];
    _window.title = @"Crustty";
    _window.delegate = self;
    _window.contentView = _view;
    _window.releasedWhenClosed = NO;
    [_window center];
    [_window makeKeyAndOrderFront:nil];
    [_window makeFirstResponder:_view];

    if (![_view startSession]) {
        // The window stays up: a Debug build has the engine's own complaint on
        // screen, and a Release build at least shows that something failed.
        _window.title = @"Crustty [no shell]";
        return;
    }

    // The title follows the shell's OSC sequences, which arrive asynchronously.
    [NSTimer scheduledTimerWithTimeInterval:0.5
                                    repeats:YES
                                      block:^(NSTimer *timer) {
                                        (void)timer;
                                        NSString *title = [self->_view windowTitle];
                                        if (![title isEqualToString:self->_window.title]) {
                                            self->_window.title = title;
                                        }
                                      }];

    [NSApp activateIgnoringOtherApps:YES];
}

- (void)applicationWillTerminate:(NSNotification *)notification {
    (void)notification;
    // Ordered teardown while the view the wake-up points at is still alive
    // (PRD §7, PRD-mac §9).
    [_view shutdown];
}

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication *)sender {
    (void)sender;
    // One window, one shell: when it goes, so does the app.
    return YES;
}

- (void)windowWillClose:(NSNotification *)notification {
    (void)notification;
    [_view shutdown];
}

#pragma mark - Menu actions

- (void)zoomIn:(id)sender {
    (void)sender;
    [_view zoomBy:1];
}

- (void)zoomOut:(id)sender {
    (void)sender;
    [_view zoomBy:-1];
}

- (void)zoomReset:(id)sender {
    (void)sender;
    [_view resetZoom];
}

- (void)reloadConfig:(id)sender {
    (void)sender;
    [_view reloadConfig];
}

@end
