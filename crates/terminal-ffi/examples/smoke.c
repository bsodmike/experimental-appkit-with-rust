/*
 * A C program that uses the boundary exactly as the AppKit frontend will:
 * create a terminal, wait for a redraw, copy one frame into buffers it owns,
 * and destroy the handle. It exists to prove the generated header and the
 * static library actually agree (PRD §14) -- on Linux, without a Mac.
 *
 * Build and run it with crates/terminal-ffi/examples/run-smoke.sh.
 */
#include "terminal.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static volatile int redraws = 0;

static void wake_up(void *ctx) {
    /* Runs on the reader thread: the real frontend would dispatch_async here. */
    (void)ctx;
    redraws++;
}

static void sleep_ms(long ms) {
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&ts, NULL);
}

static TerminalBytes str(const char *s) {
    TerminalBytes b = {(const uint8_t *)s, (uint32_t)strlen(s)};
    return b;
}

int main(void) {
    TerminalBytes args[2] = {str("-c"), str("printf 'hello from C'")};
    TerminalEnvPair env[1] = {{str("GRILL_SMOKE"), str("1")}};
    /* Zero-initialised, so a field added to the struct later is empty rather
     * than whatever was on the stack. */
    TerminalConfig config;
    memset(&config, 0, sizeof config);
    config.size.rows = 10;
    config.size.cols = 40;
    config.program = str("/bin/sh");
    config.args = args;
    config.args_len = 2;
    config.cwd = str("/tmp");
    config.env = env;
    config.env_len = 1;
    config.wake_up = wake_up;
    config.wake_up_ctx = NULL;

    TerminalSession *session = terminal_create(&config);
    if (session == NULL) {
        fprintf(stderr, "terminal_create failed\n");
        return 1;
    }

    bool gone = false;
    for (int i = 0; i < 200 && !gone; i++) {
        sleep_ms(10);
        terminal_has_hung_up(session, &gone);
    }

    /* Size the frame, then copy it: the two-call pattern of PRD §11. */
    TerminalFrameInfo info;
    memset(&info, 0, sizeof info);
    TerminalStatus status = terminal_copy_frame(session, NULL, &info);
    if (status != TerminalStatus_BufferTooSmall) {
        fprintf(stderr, "sizing call returned %d\n", (int)status);
        terminal_destroy(session);
        return 1;
    }

    TerminalRun *runs = calloc(info.runs_len ? info.runs_len : 1, sizeof(TerminalRun));
    uint8_t *text = calloc(info.text_len ? info.text_len : 1, 1);
    TerminalFrameBuffers buffers;
    buffers.runs = runs;
    buffers.runs_cap = info.runs_len;
    buffers.text = text;
    buffers.text_cap = info.text_len;

    status = terminal_copy_frame(session, &buffers, &info);
    if (status != TerminalStatus_Ok) {
        fprintf(stderr, "copy returned %d\n", (int)status);
        terminal_destroy(session);
        return 1;
    }

    printf("size:    %ux%u\n", info.rows, info.cols);
    printf("cursor:  row %u col %u, %s\n", info.cursor_row, info.cursor_col,
           info.cursor_visible ? "visible" : "hidden");
    printf("runs:    %u\n", info.runs_len);
    for (uint32_t i = 0; i < info.runs_len; i++) {
        TerminalRun r = runs[i];
        printf("  row %u col %u cols %u fg %08x bg %08x attrs %04x  \"%.*s\"\n", r.row, r.col,
               r.cols, r.fg, r.bg, r.attrs, (int)r.utf8_len, text + r.utf8_offset);
    }
    printf("redraws: %d\n", redraws);

    int ok = info.runs_len == 1 && info.text_len == strlen("hello from C") &&
             memcmp(text, "hello from C", info.text_len) == 0 && redraws > 0;

    free(runs);
    free(text);
    terminal_destroy(session);

    puts(ok ? "OK" : "MISMATCH");
    return ok ? 0 : 1;
}
