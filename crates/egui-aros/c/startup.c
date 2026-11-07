/* Program start data and DOS names for src/startup.rs (from the VectorCraft AROS port).

   Workbench owns WBenchMsg and its argument locks until CRT startup returns.
 * We borrow them on the main task only; every returned string is copied into
 * caller storage. Never ReplyMsg, UnLock a borrowed lock, or retain DiskObject data. */
#include <aros/startup.h>
#include <workbench/startup.h>
#include <workbench/workbench.h>
#include <dos/dosextens.h>
#include <proto/exec.h>
#include <proto/dos.h>
#include <proto/icon.h>
#include <proto/intuition.h>
#include <intuition/intuition.h>
#include <string.h>

int ea_startup_workbench(void) { return WBenchMsg != NULL; }

/* Unix-style probes (font folders, settings folders) name volumes that do not exist on AROS;
 * DOS would ask the user to insert each one. The program and the threads it starts (which
 * inherit pr_WindowPtr) get an error instead. */
void ea_startup_quiet_dos(void) {
    struct Process *me = (struct Process *)FindTask(NULL);
    me->pr_WindowPtr = (APTR)-1;
}

static int wb_path(int index, char *out, int cap) {
    if (!WBenchMsg || index < 0 || index >= WBenchMsg->sm_NumArgs) return 0;
    struct WBArg *arg = &WBenchMsg->sm_ArgList[index];
    /* No lock: wa_Name is a full name already (a volume or device) or relative to nothing. */
    if (!arg->wa_Lock) {
        size_t len = strlen((const char *)arg->wa_Name);
        if (len >= (size_t)cap) return -1;
        memcpy(out, arg->wa_Name, len + 1);
        return 1;
    }
    if (!NameFromLock(arg->wa_Lock, (STRPTR)out, cap)) return -1;
    return AddPart((STRPTR)out, (CONST_STRPTR)arg->wa_Name, cap) ? 1 : -1;
}
int ea_startup_wb_file(int index, char *out, int cap) { return wb_path(index, out, cap); }
int ea_startup_tooltype(const char *key, char *out, int cap) {
    char path[8192];
    if (!WBenchMsg) return 0;
    if (wb_path(0, path, sizeof(path)) != 1) return -1;
    struct Library *IconBase = OpenLibrary((CONST_STRPTR)"icon.library", 44);
    if (!IconBase) return -1;
    struct DiskObject *icon = GetDiskObject((CONST_STRPTR)path);
    if (!icon) { CloseLibrary(IconBase); return -1; }
    const char *value = (const char *)FindToolType(icon->do_ToolTypes, (STRPTR)key);
    int result = 0;
    if (value) {
        size_t len = strlen(value);
        if (len >= (size_t)cap) result = -1;
        else { memcpy(out, value, len + 1); result = 1; }
    }
    FreeDiskObject(icon);
    CloseLibrary(IconBase);
    return result;
}
/* The program's drawer as a full DOS name. */
int ea_startup_progdir(char *out, int cap) {
    BPTR dir = GetProgramDir();
    if (!dir) return 0;
    return NameFromLock(dir, (STRPTR)out, cap) ? 1 : -1;
}
/* DOS resolves volumes, assigns, colon roots and parent slashes. For a new
 * save name, resolve its existing drawer and append only the final component. */
int ea_startup_absolute(const char *name, char *out, int cap) {
    BPTR lock = Lock((CONST_STRPTR)name, ACCESS_READ);
    if (lock) {
        int ok = NameFromLock(lock, (STRPTR)out, cap);
        UnLock(lock);
        return ok ? 1 : -1;
    }
    const char *file = (const char *)FilePart((CONST_STRPTR)name);
    size_t prefix = file - name;
    char drawer[8192];
    if (!*file || prefix >= sizeof(drawer)) return -1;
    memcpy(drawer, name, prefix); drawer[prefix] = 0;
    lock = Lock((CONST_STRPTR)drawer, ACCESS_READ);
    if (!lock) return -1;
    int ok = NameFromLock(lock, (STRPTR)out, cap);
    UnLock(lock);
    return ok && AddPart((STRPTR)out, (CONST_STRPTR)file, cap) ? 1 : -1;
}
void ea_startup_error(const char *title, const char *message) {
    struct EasyStruct es = { sizeof(struct EasyStruct), 0, (CONST_STRPTR)title, (CONST_STRPTR)"%s", (CONST_STRPTR)"OK" };
    IPTR args[] = { (IPTR)message };
    EasyRequestArgs(NULL, &es, NULL, (RAWARG)args);
}
