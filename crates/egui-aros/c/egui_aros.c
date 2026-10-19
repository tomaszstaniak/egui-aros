/*
 * What egui-aros needs from AROS beyond winit and aros-gui-sys: showing the
 * CPU-rendered frame in a window, the busy pointer, and ASL requesters that
 * return absolute paths. Library bases come from the AROS autoinit link
 * libraries. The window is the struct Window * of winit's raw window handle.
 *
 * The requester and drawer code comes from the PhotoCraft AROS shell
 * (aros-development-team/ports, graphics/photocraft/photocraft-aros), as
 * extended in the WordCraft and PdfCraft AROS ports; MIT OR Apache-2.0.
 */

#include <stdint.h>
#include <string.h>

#include <proto/exec.h>
#include <proto/dos.h>
#include <proto/intuition.h>
#include <proto/cybergraphics.h>
#include <proto/asl.h>

#include <dos/dostags.h>
#include <dos/dosextens.h>

#include <intuition/intuition.h>
#include <libraries/asl.h>
#include <cybergraphx/cybergraphics.h>

/* Copies a `width` x `height` RGBA frame to the window's interior, clipped
 * to it. winit's windows are not GimmeZeroZero: the interior starts at the
 * border. */
void ea_present(struct Window *win, const uint8_t *rgba, int32_t width, int32_t height, int32_t stride)
{
    int32_t iw = win->Width - win->BorderLeft - win->BorderRight;
    int32_t ih = win->Height - win->BorderTop - win->BorderBottom;
    if (width > iw)
        width = iw;
    if (height > ih)
        height = ih;
    if (width <= 0 || height <= 0)
        return;
    WritePixelArray((APTR)rgba, 0, 0, stride, win->RPort, win->BorderLeft, win->BorderTop,
                    width, height, RECTFMT_RGBA);
}

void ea_set_busy(struct Window *win, int32_t busy)
{
    SetWindowPointer(win, WA_BusyPointer, busy ? TRUE : FALSE, TAG_DONE);
}

/*
 * The requester's drawer as an absolute path in `buf`. ASL hands back what
 * the user typed or browsed to: an empty drawer (the current directory) or a
 * relative one ("/out dir") is resolved against the current directory, which
 * the opener of the file does not share (a program started from Workbench
 * runs in its own drawer). Falls back to the drawer as given.
 */
static void absolute_drawer(const char *drawer, char *buf, LONG size)
{
    BPTR lock;
    buf[0] = 0;
    if (!drawer)
        drawer = "";
    lock = Lock((CONST_STRPTR)drawer, SHARED_LOCK);
    if (lock) {
        BOOL ok = NameFromLock(lock, (STRPTR)buf, size);
        UnLock(lock);
        if (ok)
            return;
    }
    strncpy(buf, drawer, size - 1);
    buf[size - 1] = 0;
}

/*
 * An ASL file requester. `save` selects save mode; `mode` 0 is one file, 1
 * several files, 2 a drawer. `dir`, `file` and `pattern` may be empty. The
 * chosen paths are written to `out`, separated by newlines; returns the
 * number of paths (0: cancelled or nothing fits).
 */
int32_t ea_file_request(struct Window *win, int32_t save, int32_t mode, const char *title,
                        const char *dir, const char *file, const char *pattern, char *out, int32_t outlen)
{
    struct FileRequester *req;
    int32_t count = 0;
    char path[1024];

    if (!out || outlen < 2)
        return 0;
    out[0] = 0;
    req = AllocAslRequestTags(ASL_FileRequest,
        ASLFR_Window, (IPTR)win,
        ASLFR_TitleText, (IPTR)(title ? title : "Select file"),
        ASLFR_InitialDrawer, (IPTR)(dir ? dir : ""),
        ASLFR_InitialFile, (IPTR)(file ? file : ""),
        ASLFR_DoSaveMode, save ? TRUE : FALSE,
        ASLFR_DoMultiSelect, (mode == 1 && !save) ? TRUE : FALSE,
        ASLFR_DrawersOnly, mode == 2 ? TRUE : FALSE,
        ASLFR_DoPatterns, (pattern && pattern[0]) ? TRUE : FALSE,
        ASLFR_InitialPattern, (IPTR)((pattern && pattern[0]) ? pattern : "#?"),
        ASLFR_RejectIcons, TRUE,
        TAG_DONE);
    if (!req)
        return 0;
    if (AslRequestTags(req, TAG_DONE)) {
        if (mode == 2) {
            absolute_drawer((const char *)req->fr_Drawer, path, sizeof path);
            if (strlen(path) + 1 <= (size_t)outlen) { strcpy(out, path); count = 1; }
        } else if (mode == 1 && !save && req->fr_NumArgs > 0) {
            LONG i;
            for (i = 0; i < req->fr_NumArgs; i++) {
                absolute_drawer((const char *)req->fr_Drawer, path, sizeof path);
                if (!AddPart((STRPTR)path, (CONST_STRPTR)req->fr_ArgList[i].wa_Name, sizeof path))
                    continue;
                if (strlen(out) + strlen(path) + 2 > (size_t)outlen)
                    break;
                if (count)
                    strcat(out, "\n");
                strcat(out, path);
                count++;
            }
        } else if (req->fr_File && req->fr_File[0]) {
            absolute_drawer((const char *)req->fr_Drawer, path, sizeof path);
            if (AddPart((STRPTR)path, (CONST_STRPTR)req->fr_File, sizeof path) && strlen(path) + 1 <= (size_t)outlen) {
                strcpy(out, path);
                count = 1;
            }
        }
    }
    FreeAslRequest(req);
    return count;
}

/*
 * Hands a URL to the system's OpenURL command (openurl.library's front end).
 * Refuses quotes, `*`, spaces and control or non-ASCII bytes, so the URL cannot
 * end the quoted argument. SystemTags gives the command a DupLock of our
 * PROGDIR: (pr_HomeDir) that it may never release, which keeps the program's
 * drawer "in use" until a reboot; on ABIv11 SystemTags drops an NP_HomeDir
 * tag, so the home directory is cleared for the duration of the call instead.
 * From the WordCraft/PdfCraft AROS shells.
 */
int32_t ea_open_url(const char *url)
{
    char cmd[2048];
    const char *p;
    struct Process *me = (struct Process *)FindTask(NULL);
    BPTR home;
    LONG rc;
    if (!url || strlen(url) > sizeof cmd - 16)
        return 0;
    for (p = url; *p; p++)
        if (*p == '"' || *p == '*' || (unsigned char)*p <= ' ' || (unsigned char)*p >= 0x7f)
            return 0;
    strcpy(cmd, "OpenURL \"");
    strcat(cmd, url);
    strcat(cmd, "\"");
    home = me->pr_HomeDir;
    me->pr_HomeDir = BNULL;
    rc = SystemTags((CONST_STRPTR)cmd, SYS_Asynch, FALSE, TAG_DONE);
    me->pr_HomeDir = home;
    return rc == 0;
}

void ea_beep(void)
{
    DisplayBeep(NULL);
}

/* Sets the calling task's priority and returns the previous one. */
int32_t ea_set_task_priority(int32_t pri)
{
    return SetTaskPri(FindTask(NULL), pri);
}

/* The calling task's allocated signal mask: lets tests check that a run
 * gives back every signal it took. */
uint32_t ea_task_sigalloc(void)
{
    return FindTask(NULL)->tc_SigAlloc;
}
