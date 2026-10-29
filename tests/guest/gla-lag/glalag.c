/* Does a GLA swap show the frame just drawn? Each step fills the window
   with a new colour (glClear, or a quad drawn over another clear colour),
   swaps, and reads the window centre back with ReadPixelArray.
   Usage: glalag [MODE clear|quad|finish] >file
   Each line: step, mode, expected RGB, read RGB, the step whose colour
   was read (or -1). */
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <proto/exec.h>
#include <proto/dos.h>
#include <proto/intuition.h>
#include <proto/cybergraphics.h>
#include <cybergraphx/cybergraphics.h>
#include <intuition/intuition.h>
#include <GL/gla.h>
#include <GL/gl.h>

static const unsigned char colours[8][3] = {
    {255,0,0},{0,255,0},{0,0,255},{255,255,0},{0,255,255},{255,0,255},{128,64,32},{32,128,64}};

static int which(const unsigned char *p)
{
    for (int i = 0; i < 8; i++)
        if (abs(p[0]-colours[i][0]) < 8 && abs(p[1]-colours[i][1]) < 8 && abs(p[2]-colours[i][2]) < 8)
            return i;
    return -1;
}

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "clear";
    struct Window *w = OpenWindowTags(NULL, WA_InnerWidth, 320, WA_InnerHeight, 200,
        WA_Title, (IPTR)"glalag", WA_DragBar, TRUE, WA_Activate, TRUE, TAG_DONE);
    if (!w) { printf("no window\n"); return 20; }
    struct TagItem tags[] = {
        {GLA_Window, (IPTR)w}, {GLA_Left, w->BorderLeft}, {GLA_Top, w->BorderTop},
        {GLA_Width, 320}, {GLA_Height, 200}, {GLA_DoubleBuf, GL_TRUE}, {GLA_RGBMode, GL_TRUE},
        {TAG_DONE, 0}};
    GLAContext ctx = glACreateContext(tags);
    if (!ctx) { printf("no context\n"); CloseWindow(w); return 20; }
    glAMakeCurrent(ctx);
    printf("GL_RENDERER %s\nmode %s\n", glGetString(GL_RENDERER), mode);
    int behind = 0;
    for (int step = 0; step < 8; step++) {
        const unsigned char *c = colours[step];
        if (!strcmp(mode, "quad")) {
            glClearColor(0.5f, 0.5f, 0.5f, 1.0f);
            glClear(GL_COLOR_BUFFER_BIT);
            glColor3ub(c[0], c[1], c[2]);
            glBegin(GL_QUADS);
            glVertex2f(-1, -1); glVertex2f(1, -1); glVertex2f(1, 1); glVertex2f(-1, 1);
            glEnd();
        } else {
            glClearColor(c[0]/255.0f, c[1]/255.0f, c[2]/255.0f, 1.0f);
            glClear(GL_COLOR_BUFFER_BIT);
        }
        if (!strcmp(mode, "finish"))
            glFinish();
        glASwapBuffers(ctx);
        Delay(10);
        unsigned char px[4] = {0};
        ReadPixelArray(px, 0, 0, 4, w->RPort, w->BorderLeft + 160, w->BorderTop + 100, 1, 1, RECTFMT_RGBA);
        int got = which(px);
        if (got != step) behind++;
        printf("step %d expected %d,%d,%d read %d,%d,%d shows %d\n", step, c[0], c[1], c[2], px[0], px[1], px[2], got);
    }
    printf("result %s (%d of 8 steps did not show their own frame)\n", behind ? "STALE" : "OK", behind);
    glADestroyContext(ctx);
    CloseWindow(w);
    return 0;
}
