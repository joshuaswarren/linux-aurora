/* SPDX-License-Identifier: MIT
 * Draw the existing X11 cursor as an input-transparent composited window.
 * appledrm exposes no legacy cursor plane, so Xorg otherwise erases/restores
 * its software sprite in the scanout buffer during every overlapping redraw.
 * Putting the cursor in KWin's scene makes it part of each completed frame.
 * XFixesHideCursor is client-owned: disconnecting restores the native cursor.
 */
#define _POSIX_C_SOURCE 200809L
#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/extensions/Xfixes.h>
#include <X11/extensions/Xrender.h>
#include <X11/extensions/shape.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <systemd/sd-bus.h>
#include <time.h>

static volatile sig_atomic_t stopping;

static void stop(int signal_number)
{
    (void)signal_number;
    stopping = 1;
}

static XImage *copy_cursor(Display *dpy, Visual *visual,
                           const XFixesCursorImage *cursor)
{
    if (!cursor->width || !cursor->height ||
        cursor->width > 1024 || cursor->height > 1024)
        return NULL;
    size_t count = (size_t)cursor->width * cursor->height;
    uint32_t *pixels = calloc(count, sizeof(*pixels));
    if (!pixels)
        return NULL;
    for (size_t i = 0; i < count; ++i)
        pixels[i] = (uint32_t)cursor->pixels[i];
    XImage *image = XCreateImage(dpy, visual, 32, ZPixmap, 0, (char *)pixels,
                                 cursor->width, cursor->height, 32, 0);
    if (!image)
        free(pixels);
    return image;
}

static bool screen_locked(sd_bus **bus)
{
    if (!*bus) {
        if (sd_bus_open_user(bus) < 0)
            return true;
        sd_bus_set_method_call_timeout(*bus, 100000);
    }
    sd_bus_message *reply = NULL;
    int locked = 1;
    int result = sd_bus_call_method(*bus, "org.freedesktop.ScreenSaver",
        "/ScreenSaver", "org.freedesktop.ScreenSaver", "GetActive", NULL,
        &reply, "");
    if (result >= 0)
        result = sd_bus_message_read(reply, "b", &locked);
    sd_bus_message_unref(reply);
    if (result < 0) {
        *bus = sd_bus_unref(*bus);
        return true;
    }
    return locked != 0;
}

static uint64_t monotonic_ms(void)
{
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (uint64_t)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

int main(int argc, char **argv)
{
    bool plasma = argc == 2 && strcmp(argv[1], "--plasma") == 0;
    if (argc > 1 && !plasma) {
        fprintf(stderr, "Usage: %s [--plasma]\n", argv[0]);
        return 1;
    }
    Display *dpy = XOpenDisplay(NULL);
    if (!dpy) {
        fprintf(stderr, "Cannot open the X11 display\n");
        return 1;
    }
    int screen = DefaultScreen(dpy);
    Window root = RootWindow(dpy, screen);
    int event_base, error_base, major = 5, minor = 0;
    if (!XFixesQueryExtension(dpy, &event_base, &error_base) ||
        !XFixesQueryVersion(dpy, &major, &minor) || major < 4) {
        fprintf(stderr, "XFixes 4 or newer is required\n");
        XCloseDisplay(dpy);
        return 1;
    }
    char selection_name[64];
    snprintf(selection_name, sizeof(selection_name), "_NET_WM_CM_S%d", screen);
    Atom compositor_selection = XInternAtom(dpy, selection_name, False);
    snprintf(selection_name, sizeof(selection_name), "_NEO_COMPOSITED_CURSOR_S%d", screen);
    Atom own_selection = XInternAtom(dpy, selection_name, False);
    if (XGetSelectionOwner(dpy, own_selection) != None) {
        fprintf(stderr, "A composited cursor already owns this display\n");
        XCloseDisplay(dpy);
        return 1;
    }
    XVisualInfo wanted = {.screen = screen, .depth = 32, .class = TrueColor};
    int count = 0;
    XVisualInfo *visuals = XGetVisualInfo(dpy, VisualScreenMask | VisualDepthMask |
                                         VisualClassMask, &wanted, &count);
    Visual *visual = NULL;
    for (int i = 0; i < count; ++i) {
        XRenderPictFormat *format = XRenderFindVisualFormat(dpy, visuals[i].visual);
        if (format && format->type == PictTypeDirect && format->direct.alphaMask) {
            visual = visuals[i].visual;
            break;
        }
    }
    if (!visual) {
        fprintf(stderr, "No compositable ARGB visual\n");
        if (visuals)
            XFree(visuals);
        XCloseDisplay(dpy);
        return 1;
    }
    Colormap colormap = XCreateColormap(dpy, root, visual, AllocNone);
    XSetWindowAttributes attrs = {.override_redirect = True, .colormap = colormap,
        .border_pixel = 0, .background_pixel = 0, .event_mask = ExposureMask};
    Window window = XCreateWindow(dpy, root, 0, 0, 1, 1, 0, 32, InputOutput,
        visual, CWOverrideRedirect | CWColormap | CWBorderPixel | CWBackPixel |
        CWEventMask, &attrs);
    XSetSelectionOwner(dpy, own_selection, window, CurrentTime);
    if (XGetSelectionOwner(dpy, own_selection) != window) {
        fprintf(stderr, "Another cursor won ownership\n");
        XCloseDisplay(dpy);
        return 1;
    }
    XStoreName(dpy, window, "Neo composited cursor");
    Atom type = XInternAtom(dpy, "_NET_WM_WINDOW_TYPE_NOTIFICATION", False);
    XChangeProperty(dpy, window, XInternAtom(dpy, "_NET_WM_WINDOW_TYPE", False),
                    XA_ATOM, 32, PropModeReplace, (unsigned char *)&type, 1);
    unsigned long no_bypass = 2;
    XChangeProperty(dpy, window, XInternAtom(dpy, "_NET_WM_BYPASS_COMPOSITOR", False),
                    XA_CARDINAL, 32, PropModeReplace, (unsigned char *)&no_bypass, 1);
    XserverRegion empty = XFixesCreateRegion(dpy, NULL, 0);
    XFixesSetWindowShapeRegion(dpy, window, ShapeInput, 0, 0, empty);
    XFixesDestroyRegion(dpy, empty);
    XFixesSelectCursorInput(dpy, root, XFixesDisplayCursorNotifyMask);
    XSelectInput(dpy, root, SubstructureNotifyMask);
    XGCValues gc_values = {.graphics_exposures = False};
    GC gc = XCreateGC(dpy, window, GCGraphicsExposures, &gc_values);
    struct sigaction action = {.sa_handler = stop};
    sigemptyset(&action.sa_mask);
    sigaction(SIGINT, &action, NULL);
    sigaction(SIGTERM, &action, NULL);
    sigaction(SIGHUP, &action, NULL);
    bool hidden = false, mapped = false, dirty = true, repaint = false, raise = true;
    int last_x = -32768, last_y = -32768, xhot = 0, yhot = 0;
    XImage *image = NULL;
    unsigned long shapes = 0, moves = 0;
    sd_bus *bus = NULL;
    bool locked = plasma;
    uint64_t last_lock_check = 0;
    fprintf(stderr, "Cursor ready: display=%s overlay=0x%lx; awaiting compositor\n",
            DisplayString(dpy), window);
    while (!stopping) {
        while (XPending(dpy)) {
            XEvent event;
            XNextEvent(dpy, &event);
            if (event.type == event_base + XFixesCursorNotify)
                dirty = true;
            else if (event.type == Expose && event.xexpose.window == window)
                repaint = true;
            else if (event.type == MapNotify && event.xmap.window != window)
                raise = true;
            else if (event.type == SelectionClear && event.xselectionclear.selection == own_selection)
                stopping = 1;
        }
        bool compositing = XGetSelectionOwner(dpy, compositor_selection) != None;
        uint64_t now = monotonic_ms();
        if (plasma && now - last_lock_check >= 250) {
            locked = screen_locked(&bus);
            last_lock_check = now;
        }
        Window root_return, child_return;
        int x, y, window_x, window_y;
        unsigned int mask;
        bool on_screen = XQueryPointer(dpy, root, &root_return, &child_return,
                                       &x, &y, &window_x, &window_y, &mask);
        if (!compositing || !on_screen || locked) {
            if (mapped) {
                XUnmapWindow(dpy, window);
                mapped = false;
            }
            if (hidden) {
                XFixesShowCursor(dpy, root);
                hidden = false;
                fprintf(stderr, "Native cursor restored: compositor unavailable or screen locked\n");
            }
            dirty = true;
        } else {
            if (dirty) {
                XFixesCursorImage *cursor = XFixesGetCursorImage(dpy);
                XImage *replacement = cursor ? copy_cursor(dpy, visual, cursor) : NULL;
                if (!replacement) {
                    if (cursor)
                        XFree(cursor);
                    fprintf(stderr, "Cursor image read failed; restoring native cursor\n");
                    break;
                }
                xhot = cursor->xhot;
                yhot = cursor->yhot;
                XFree(cursor);
                if (image)
                    XDestroyImage(image);
                image = replacement;
                ++shapes;
                dirty = false;
                repaint = true;
            }
            bool moved = x != last_x || y != last_y;
            if (repaint || moved || !mapped || raise) {
                /* Keep resize, upload and move together across shape changes. */
                XGrabServer(dpy);
                XMoveResizeWindow(dpy, window, x - xhot, y - yhot,
                                   image->width, image->height);
                if (repaint)
                    XPutImage(dpy, window, gc, image, 0, 0, 0, 0,
                               image->width, image->height);
                if (!mapped)
                    XMapRaised(dpy, window);
                else if (raise)
                    XRaiseWindow(dpy, window);
                XUngrabServer(dpy);
                mapped = true;
                repaint = false;
                raise = false;
                last_x = x;
                last_y = y;
                moves += moved;
            }
            if (!hidden) {
                XFixesHideCursor(dpy, root);
                hidden = true;
                fprintf(stderr, "Cursor is now part of the composited frame\n");
            }
        }
        XFlush(dpy);
        struct pollfd descriptor = {.fd = ConnectionNumber(dpy), .events = POLLIN};
        poll(&descriptor, 1, 8);
    }
    if (hidden)
        XFixesShowCursor(dpy, root);
    XUnmapWindow(dpy, window);
    XSetSelectionOwner(dpy, own_selection, None, CurrentTime);
    XSync(dpy, False);
    fprintf(stderr, "Cursor stopped: shapes=%lu moves=%lu; native cursor restored\n",
            shapes, moves);
    if (image)
        XDestroyImage(image);
    XFreeGC(dpy, gc);
    XDestroyWindow(dpy, window);
    XFreeColormap(dpy, colormap);
    XFree(visuals);
    XCloseDisplay(dpy);
    sd_bus_unref(bus);
    return 0;
}
