/* The calling task's allocated signal mask, for leak checks. */
#include <stdint.h>
#include <proto/exec.h>

uint32_t ea_sigalloc(void)
{
    return FindTask(NULL)->tc_SigAlloc;
}
