// The entry point, and the menu bar.
//
// Both are built in code rather than in a nib: there is one window and six menu
// items, and a nib would be a second file to keep in step with them.

#import <AppKit/AppKit.h>

#import "AppDelegate.h"

static NSMenuItem *ItemWithTitle(NSString *title, SEL action, NSString *key,
                                 NSEventModifierFlags modifiers) {
    NSMenuItem *item = [[NSMenuItem alloc] initWithTitle:title action:action keyEquivalent:key];
    item.keyEquivalentModifierMask = modifiers;
    return item;
}

static void BuildMenuBar(void) {
    NSMenu *bar = [[NSMenu alloc] init];

    NSMenuItem *appItem = [[NSMenuItem alloc] init];
    NSMenu *appMenu = [[NSMenu alloc] init];
    [appMenu addItem:ItemWithTitle(@"Hide Crustty", @selector(hide:), @"h",
                                   NSEventModifierFlagCommand)];
    [appMenu addItem:[NSMenuItem separatorItem]];
    [appMenu addItem:ItemWithTitle(@"Quit Crustty", @selector(terminate:), @"q",
                                   NSEventModifierFlagCommand)];
    appItem.submenu = appMenu;
    [bar addItem:appItem];

    NSMenuItem *editItem = [[NSMenuItem alloc] init];
    NSMenu *editMenu = [[NSMenu alloc] initWithTitle:@"Edit"];
    // Copy is here for the shortcut to exist; selection arrives in a later
    // slice, so it does nothing yet rather than pretending to.
    [editMenu addItem:ItemWithTitle(@"Copy", @selector(copy:), @"c", NSEventModifierFlagCommand)];
    [editMenu addItem:ItemWithTitle(@"Paste", @selector(paste:), @"v",
                                    NSEventModifierFlagCommand)];
    editItem.submenu = editMenu;
    [bar addItem:editItem];

    NSMenuItem *viewItem = [[NSMenuItem alloc] init];
    NSMenu *viewMenu = [[NSMenu alloc] initWithTitle:@"View"];
    [viewMenu addItem:ItemWithTitle(@"Bigger", @selector(zoomIn:), @"+",
                                    NSEventModifierFlagCommand)];
    [viewMenu addItem:ItemWithTitle(@"Smaller", @selector(zoomOut:), @"-",
                                    NSEventModifierFlagCommand)];
    [viewMenu addItem:ItemWithTitle(@"Actual Size", @selector(zoomReset:), @"0",
                                    NSEventModifierFlagCommand)];
    viewItem.submenu = viewMenu;
    [bar addItem:viewItem];

    NSApp.mainMenu = bar;
}

int main(int argc, const char *argv[]) {
    (void)argc;
    (void)argv;
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];

        // NSApplication holds its delegate weakly, so something else has to
        // hold it. A static outlives the application object, which is exactly
        // as long as it needs to.
        static AppDelegate *delegate;
        delegate = [[AppDelegate alloc] init];
        NSApp.delegate = delegate;

        BuildMenuBar();
        [NSApp run];
    }
    return 0;
}
