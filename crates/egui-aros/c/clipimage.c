/* Pictures on clipboard unit 0 for src/clipboard_image.rs.

   Reading: the clipboard's FORM type is looked at first with iffparse, so an
   empty clipboard or text is told apart from a picture no datatype can read.
   A picture is then opened with datatypes (DTST_CLIPBOARD, picture group,
   PMODE_V43) and its pixels are copied with PDTM_READPIXELARRAY into the
   caller's buffer. The clipboard is only read.

   Writing: a FORM ILBM with the caller's BMHD and BODY payloads, through
   iffparse. Libraries are opened per call and closed before returning. */
#include <stdint.h>
#include <string.h>
#include <exec/memory.h>
#include <devices/clipboard.h>
#include <dos/dos.h>
#include <libraries/iffparse.h>
#include <datatypes/datatypes.h>
#include <datatypes/datatypesclass.h>
#include <datatypes/pictureclass.h>
#include <intuition/classusr.h>
#include <utility/tagitem.h>
#include <proto/exec.h>
#include <proto/dos.h>
#include <proto/iffparse.h>
#include <proto/datatypes.h>
#include <clib/alib_protos.h>

/* ID_ILBM, ID_BMHD and ID_BODY come from datatypes/pictureclass.h. */
#define ID_FTXT MAKE_ID('F', 'T', 'X', 'T')

#define KIND_EMPTY   0
#define KIND_TEXT    1
#define KIND_PICTURE 2

struct clip_image {
    struct Library *datatypes;
    Object *object;
    uint32_t width, height;
};

static void say(char *err, int32_t cap, const char *text, LONG code)
{
    size_t len;

    if (cap <= 0)
        return;
    len = strlen(text);
    if (len >= (size_t)cap)
        len = cap - 1;
    memcpy(err, text, len);
    err[len] = '\0';
    if (code && len + 16 < (size_t)cap) {
        /* The DOS error text after ": ". */
        err[len++] = ':';
        err[len++] = ' ';
        err[len] = '\0';
        if (!Fault(code, NULL, (STRPTR)err + len, cap - len))
            err[len - 2] = '\0';
    }
}

/* The type of the clipboard's first FORM (0 when it holds none). */
static ULONG clip_form_type(void)
{
    struct Library *IFFParseBase = OpenLibrary((CONST_STRPTR)"iffparse.library", 39);
    struct IFFHandle *iff;
    ULONG type = 0;

    if (!IFFParseBase)
        return 0;
    iff = AllocIFF();
    if (iff) {
        iff->iff_Stream = (IPTR)OpenClipboard(PRIMARY_CLIP);
        if (iff->iff_Stream) {
            InitIFFasClip(iff);
            if (OpenIFF(iff, IFFF_READ) == 0) {
                if (ParseIFF(iff, IFFPARSE_STEP) == 0) {
                    struct ContextNode *cn = CurrentChunk(iff);

                    if (cn && cn->cn_ID == ID_FORM)
                        type = cn->cn_Type;
                }
                CloseIFF(iff);
            }
            CloseClipboard((struct ClipboardHandle *)iff->iff_Stream);
        }
        FreeIFF(iff);
    }
    CloseLibrary(IFFParseBase);
    return type;
}

void *ea_clip_image_open(int32_t *kind, uint32_t *width, uint32_t *height, char *err, int32_t cap)
{
    struct clip_image *ci;
    struct BitMapHeader *bmh = NULL;
    ULONG type = clip_form_type();
    struct Library *DataTypesBase;

    *width = *height = 0;
    if (cap > 0)
        err[0] = '\0';
    if (type == 0) {
        *kind = KIND_EMPTY;
        return NULL;
    }
    if (type == ID_FTXT) {
        *kind = KIND_TEXT;
        return NULL;
    }
    *kind = KIND_PICTURE;

    ci = AllocVec(sizeof(*ci), MEMF_ANY | MEMF_CLEAR);
    if (!ci) {
        say(err, cap, "not enough memory", 0);
        return NULL;
    }
    ci->datatypes = DataTypesBase = OpenLibrary((CONST_STRPTR)"datatypes.library", 39);
    if (!ci->datatypes) {
        say(err, cap, "datatypes.library cannot be opened", 0);
    } else {
        struct TagItem tags[] = {
            { DTA_SourceType, DTST_CLIPBOARD },
            { DTA_GroupID, GID_PICTURE },
            { PDTA_DestMode, PMODE_V43 },
            { PDTA_Remap, FALSE },
            { TAG_DONE, 0 },
        };
        /* For DTST_CLIPBOARD the name is the clipboard unit. */
        ci->object = NewDTObjectA((APTR)(IPTR)PRIMARY_CLIP, tags);
        if (!ci->object) {
            say(err, cap, "no datatype could read the picture on the clipboard", IoErr());
        } else {
            struct TagItem get[] = { { PDTA_BitMapHeader, (IPTR)&bmh }, { TAG_DONE, 0 } };

            if (GetDTAttrsA(ci->object, get) == 1 && bmh && bmh->bmh_Width && bmh->bmh_Height) {
                ci->width = *width = bmh->bmh_Width;
                ci->height = *height = bmh->bmh_Height;
                return ci;
            }
            say(err, cap, "the picture on the clipboard has no size", 0);
        }
    }
    if (ci->object)
        DisposeDTObject(ci->object);
    if (ci->datatypes)
        CloseLibrary(ci->datatypes);
    FreeVec(ci);
    return NULL;
}

int32_t ea_clip_image_read(void *handle, uint8_t *rgba, uint32_t stride)
{
    struct clip_image *ci = handle;
    struct pdtBlitPixelArray msg;

    /* The whole width: picture.datatype's conversion loop always walks it. */
    msg.MethodID = PDTM_READPIXELARRAY;
    msg.pbpa_PixelData = rgba;
    msg.pbpa_PixelFormat = PBPAFMT_RGBA;
    msg.pbpa_PixelArrayMod = stride;
    msg.pbpa_Left = 0;
    msg.pbpa_Top = 0;
    msg.pbpa_Width = ci->width;
    msg.pbpa_Height = ci->height;
    return DoMethodA(ci->object, (Msg)&msg) ? 1 : 0;
}

void ea_clip_image_close(void *handle)
{
    struct clip_image *ci = handle;
    struct Library *DataTypesBase = ci->datatypes;

    DisposeDTObject(ci->object);
    CloseLibrary(ci->datatypes);
    FreeVec(ci);
}

int32_t ea_clip_ilbm_write(const uint8_t *bmhd, const uint8_t *body, uint32_t body_len, char *err, int32_t cap)
{
    struct Library *IFFParseBase = OpenLibrary((CONST_STRPTR)"iffparse.library", 39);
    struct IFFHandle *iff;
    int32_t rc = -1;

    if (cap > 0)
        err[0] = '\0';
    if (!IFFParseBase) {
        say(err, cap, "iffparse.library cannot be opened", 0);
        return -1;
    }
    iff = AllocIFF();
    if (!iff) {
        say(err, cap, "not enough memory", 0);
    } else {
        iff->iff_Stream = (IPTR)OpenClipboard(PRIMARY_CLIP);
        if (!iff->iff_Stream) {
            say(err, cap, "the clipboard cannot be opened", 0);
        } else {
            InitIFFasClip(iff);
            if (OpenIFF(iff, IFFF_WRITE) != 0) {
                say(err, cap, "the clipboard cannot be written", 0);
            } else {
                if (PushChunk(iff, ID_ILBM, ID_FORM, IFFSIZE_UNKNOWN) == 0
                    && PushChunk(iff, 0, ID_BMHD, 20) == 0
                    && WriteChunkBytes(iff, (APTR)bmhd, 20) == 20
                    && PopChunk(iff) == 0
                    && PushChunk(iff, 0, ID_BODY, body_len) == 0
                    && WriteChunkBytes(iff, (APTR)body, body_len) == (LONG)body_len
                    && PopChunk(iff) == 0
                    && PopChunk(iff) == 0)
                    rc = 0;
                else
                    say(err, cap, "writing the picture to the clipboard failed", 0);
                CloseIFF(iff);
            }
            CloseClipboard((struct ClipboardHandle *)iff->iff_Stream);
        }
        FreeIFF(iff);
    }
    CloseLibrary(IFFParseBase);
    return rc;
}
