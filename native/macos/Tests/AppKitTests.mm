// The tier that genuinely needs a Mac.
//
// Deliberately small (PRD-mac §12). Everything that decides something lives in
// Glue and is tested on Linux; what is left here is whether the AppKit objects
// can be constructed, measured, resized and torn down without falling over.
// Tests that assert pixels would fail whenever the system font changed, which
// is not a signal anybody wants.

#import <XCTest/XCTest.h>

#import "TerminalView.h"

#include "Config.h"
#include "ConfigFile.h"
#include "Metrics.h"

@interface AppKitTests : XCTestCase
@end

@implementation AppKitTests

- (void)testViewComputesAGridFromARealFont {
    TerminalView *view = [[TerminalView alloc] initWithFrame:NSMakeRect(0, 0, 800, 480)];
    XCTAssertNotNil(view);

    // The preferred size comes from measuring the actual system font, which is
    // the one thing here that cannot be checked without a Mac.
    const NSSize size = [view preferredSize];
    XCTAssertGreaterThan(size.width, 0);
    XCTAssertGreaterThan(size.height, 0);
    XCTAssertGreaterThan(size.width, size.height, @"80 columns is wider than 24 rows is tall");
}

- (void)testZoomChangesThePreferredSize {
    TerminalView *view = [[TerminalView alloc] initWithFrame:NSMakeRect(0, 0, 800, 480)];
    const NSSize before = [view preferredSize];
    [view zoomBy:4];
    const NSSize bigger = [view preferredSize];
    XCTAssertGreaterThan(bigger.width, before.width);

    [view resetZoom];
    XCTAssertEqualWithAccuracy([view preferredSize].width, before.width, 0.5);
}

- (void)testAViewWithNoSessionDrawsWithoutCrashing {
    TerminalView *view = [[TerminalView alloc] initWithFrame:NSMakeRect(0, 0, 400, 240)];
    NSBitmapImageRep *rep = [view bitmapImageRepForCachingDisplayInRect:view.bounds];
    XCTAssertNotNil(rep);
    [view cacheDisplayInRect:view.bounds toBitmapImageRep:rep];
}

- (void)testAShellDrawsItsOutput {
    TerminalView *view = [[TerminalView alloc] initWithFrame:NSMakeRect(0, 0, 640, 400)];
    XCTAssertTrue([view startSession], @"the configured shell should start");

    // Give the shell a moment to produce a prompt, then draw for real.
    XCTestExpectation *drawn = [self expectationWithDescription:@"drawn"];
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(1.0 * NSEC_PER_SEC)),
                   dispatch_get_main_queue(), ^{
                     NSBitmapImageRep *rep =
                         [view bitmapImageRepForCachingDisplayInRect:view.bounds];
                     [view cacheDisplayInRect:view.bounds toBitmapImageRep:rep];
                     [drawn fulfill];
                   });
    [self waitForExpectations:@[ drawn ] timeout:10];

    [view shutdown];
    [view shutdown];  // ordered teardown is idempotent
}

- (void)testResizingReachesTheEngine {
    TerminalView *view = [[TerminalView alloc] initWithFrame:NSMakeRect(0, 0, 640, 400)];
    XCTAssertTrue([view startSession]);
    [view setFrameSize:NSMakeSize(320, 200)];
    [view setFrameSize:NSMakeSize(900, 600)];
    [view shutdown];
}

- (void)testReloadingTheConfigDoesNotDisturbAViewWithNoSession {
    TerminalView *view = [[TerminalView alloc] initWithFrame:NSMakeRect(0, 0, 640, 400)];
    [view reloadConfig];
    XCTAssertGreaterThan([view preferredSize].width, 0);
}

- (void)testTheTitleIsAlwaysSomething {
    TerminalView *view = [[TerminalView alloc] initWithFrame:NSMakeRect(0, 0, 640, 400)];
    XCTAssertGreaterThan([view windowTitle].length, 0u,
                         @"a window with no title from the shell still needs one");
}

@end
